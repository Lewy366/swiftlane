//! The share server people reach through a link (same rules as share.py's Handler):
//! - listens on 127.0.0.1 only; the internet reaches it through Tailscale Funnel, your home
//!   network can't reach it at all; only the expected Host names are answered;
//! - download only (request links only take files in); pages are plain server-made HTML with
//!   no scripts (request pages load one small upload script); names are escaped; files are
//!   always sent as downloads;
//! - previews are fresh small JPEGs (upright, no EXIF/GPS), never the original file, and never
//!   count as a download; password-protected shares show nothing until unlocked;
//! - at most 48 connections at once; wrong passwords lock out per address and per link.

use axum::body::{Body, Bytes};
use axum::extract::{Request, State};
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Router;
use futures_util::StreamExt;
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

use crate::api::AppState;
use crate::config;
use crate::share::{self, is_active, now};
use crate::transfer::{human, place_file, safe_name};

const MAX_CONNECTIONS: usize = 48;
const PW_FAILS_PER_ADDRESS: u32 = 5;
const PW_FAILS_PER_LINK: u32 = 40;
const PW_LOCK: f64 = 15.0 * 60.0;
const UPLOAD_HEADROOM: u64 = 1024 * 1024 * 1024;
const PREVIEW_MAX_BYTES: u64 = 80 * 1024 * 1024;
const PREVIEW_MAX_PIXELS: u64 = 120_000_000;
const PREVIEW_BIG: u32 = 1200;
const PREVIEW_SMALL: u32 = 96;
const PREVIEW_CACHE_BYTES: usize = 48 * 1024 * 1024;
pub const READY_PATH: &str = "/.well-known/swiftlane-ready/";
const PAGE_CSP: &str = "default-src 'none'; style-src 'unsafe-inline'; img-src 'self'; form-action 'self'; frame-ancestors 'none'; base-uri 'none'";
const REQUEST_CSP: &str = "default-src 'none'; script-src 'self'; style-src 'unsafe-inline'; connect-src 'self'; img-src 'self'; form-action 'self'; frame-ancestors 'none'; base-uri 'none'";
const PAGE_STYLE: &str = include_str!("share_page.css");
const REQUEST_JS: &str = include_str!("share_request.js");
const PICTURES: &[&str] = &[".jpg", ".jpeg", ".png", ".gif", ".webp", ".bmp", ".tif", ".tiff", ".heic", ".heif"];
const PROGRAMS: &[&str] = &[".exe", ".msi", ".msix", ".appx", ".bat", ".cmd", ".com", ".scr", ".pif", ".cpl", ".ps1", ".psm1",
    ".vbs", ".vbe", ".js", ".jse", ".wsf", ".wsh", ".hta", ".jar", ".reg", ".lnk", ".dll", ".sys", ".apk"];

/// What the running share server keeps between requests.
pub struct ServerState {
    pub host_name: Mutex<Option<String>>, // e.g. mypc.tail1234.ts.net
    pub ready_nonce: Mutex<Option<String>>,
    pub busy: AtomicUsize,                // downloads/uploads running right now
    lockout: Mutex<HashMap<String, (u32, f64, f64)>>, // key -> (count, window start, locked until)
    previews: Mutex<(VecDeque<(String, Option<Vec<u8>>)>, usize)>,
    making: Mutex<()>,
    slots: Arc<tokio::sync::Semaphore>,
    pub port: u16,
}

impl ServerState {
    fn new(host: Option<String>, port: u16) -> Self {
        ServerState {
            host_name: Mutex::new(host), ready_nonce: Mutex::new(None), busy: AtomicUsize::new(0),
            lockout: Mutex::new(HashMap::new()), previews: Mutex::new((VecDeque::new(), 0)), making: Mutex::new(()),
            slots: Arc::new(tokio::sync::Semaphore::new(MAX_CONNECTIONS)), port,
        }
    }

    fn locked(&self, keys: &[String]) -> bool {
        let l = self.lockout.lock().unwrap();
        keys.iter().any(|k| l.get(k).is_some_and(|e| e.2 > now()))
    }

    fn fail(&self, key: String, limit: u32) {
        let mut l = self.lockout.lock().unwrap();
        let e = l.entry(key).or_insert((0, now(), 0.0));
        if now() - e.1 > PW_LOCK {
            *e = (0, now(), e.2);
        }
        e.0 += 1;
        if e.0 >= limit {
            e.2 = now() + PW_LOCK;
        }
    }
}

/// Counts a running transfer for as long as it lives (Funnel stays on while one runs).
struct Busy(AppHandle);
impl Busy {
    fn new(app: &AppHandle) -> Self {
        if let Some(srv) = server(app) {
            srv.busy.fetch_add(1, Ordering::Relaxed);
        }
        Busy(app.clone())
    }
}
impl Drop for Busy {
    fn drop(&mut self) {
        if let Some(srv) = server(&self.0) {
            srv.busy.fetch_sub(1, Ordering::Relaxed);
        }
    }
}

pub fn server(app: &AppHandle) -> Option<Arc<ServerState>> {
    app.state::<AppState>().sharing.server.lock().unwrap().as_ref().map(|(s, _)| s.clone())
}

// ------------------------------------------------------------------ small helpers

pub fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&#x27;")
}

fn ext(name: &str) -> String {
    let base = name.rsplit('/').next().unwrap_or(name);
    match base.rfind('.') {
        Some(i) if i > 0 => base[i..].to_lowercase(),
        _ => String::new(),
    }
}

fn base_name(name: &str) -> &str {
    name.rsplit('/').next().unwrap_or(name)
}

/// Plain-language type for the page: "PDF document", "Program", ...
fn file_kind(name: &str) -> String {
    let e = ext(name);
    let kinds: &[(&str, &[&str])] = &[
        ("PDF document", &[".pdf"]), ("Word document", &[".doc", ".docx", ".odt", ".rtf"]),
        ("Excel spreadsheet", &[".xls", ".xlsx", ".ods", ".csv"]), ("PowerPoint presentation", &[".ppt", ".pptx", ".odp"]),
        ("Text file", &[".txt", ".md", ".log"]),
        ("Picture", &[".jpg", ".jpeg", ".png", ".gif", ".webp", ".bmp", ".tif", ".tiff", ".heic", ".heif", ".svg", ".raw", ".dng", ".cr2", ".nef", ".arw"]),
        ("Video", &[".mp4", ".mov", ".mkv", ".avi", ".webm", ".wmv", ".m4v"]),
        ("Music or audio", &[".mp3", ".wav", ".flac", ".m4a", ".aac", ".ogg", ".opus", ".wma"]),
        ("Archive", &[".zip", ".rar", ".7z", ".tar", ".gz", ".tgz", ".bz2", ".xz"]),
        ("Disk image", &[".iso", ".img", ".vhd", ".vhdx"]), ("Program", PROGRAMS),
    ];
    for (k, exts) in kinds {
        if exts.contains(&e.as_str()) {
            return k.to_string();
        }
    }
    if e.len() > 1 && e.len() <= 8 { format!("{} file", e[1..].to_uppercase()) } else { "File".into() }
}

fn is_program(name: &str) -> bool {
    PROGRAMS.contains(&ext(name).as_str())
}

fn previewable(item: &Value) -> bool {
    PICTURES.contains(&ext(item["name"].as_str().unwrap_or("")).as_str())
        && item["size"].as_u64().is_some_and(|s| s > 0 && s < PREVIEW_MAX_BYTES)
}

/// The file whose preview chat apps may show in the link card: only the first file, only a
/// picture, never with a password, and only if the sender allowed it.
fn card_picture(s: &Value) -> Option<usize> {
    if s["kind"] == "request" || !s["password"].is_null() || !s["card_picture"].as_bool().unwrap_or(false) {
        return None;
    }
    s["items"].get(0).filter(|i| previewable(i)).map(|_| 0)
}

fn left(seconds: f64) -> String {
    if seconds < 3600.0 {
        format!("{} min", (seconds / 60.0).round().max(1.0))
    } else if seconds < 86400.0 * 2.0 {
        format!("{} h", (seconds / 3600.0).round())
    } else {
        format!("{} days", (seconds / 86400.0).round())
    }
}

pub fn disposition(name: &str) -> String {
    let ascii: String = name.chars().map(|c| if (' '..='~').contains(&c) && c != '"' && c != '\\' { c } else { '_' }).collect();
    let utf8 = percent_encoding::utf8_percent_encode(name, percent_encoding::NON_ALPHANUMERIC);
    format!("attachment; filename=\"{ascii}\"; filename*=UTF-8''{utf8}")
}

fn common(h: &mut HeaderMap) {
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    h.insert("X-Content-Type-Options", HeaderValue::from_static("nosniff"));
    h.insert("Referrer-Policy", HeaderValue::from_static("no-referrer"));
    h.insert("X-Robots-Tag", HeaderValue::from_static("noindex, nofollow"));
    h.insert("X-Frame-Options", HeaderValue::from_static("DENY"));
}

fn page(status: StatusCode, title: &str, body: &str, head: &str, csp: &'static str, extra: Vec<(header::HeaderName, String)>) -> Response {
    let doc = format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\
         <meta name=\"robots\" content=\"noindex,nofollow\"><meta name=\"referrer\" content=\"no-referrer\">{head}\
         <title>{}</title><style>{PAGE_STYLE}</style></head><body><main>{body}</main></body></html>", esc(title));
    let mut r = (status, doc).into_response();
    let h = r.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static("text/html; charset=utf-8"));
    h.insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static(csp));
    common(h);
    for (k, v) in extra {
        if let Ok(v) = HeaderValue::from_str(&v) {
            h.append(k, v);
        }
    }
    r
}

fn gone() -> Response {
    page(StatusCode::NOT_FOUND, "Link not available", "<h1>This link isn’t available</h1>\
         <p>It may have expired, been used up or been switched off by the person who sent it.</p>", "", PAGE_CSP, vec![])
}

fn plain(status: StatusCode) -> Response {
    let mut r = status.into_response();
    common(r.headers_mut());
    r
}

fn redirect(to: &str) -> Response {
    let mut r = plain(StatusCode::SEE_OTHER);
    if let Ok(v) = HeaderValue::from_str(to) {
        r.headers_mut().insert(header::LOCATION, v);
    }
    r
}

fn json_err(status: StatusCode, msg: &str) -> Response {
    let mut r = (status, axum::Json(json!({ "error": msg }))).into_response();
    common(r.headers_mut());
    r.headers_mut().insert(header::CONNECTION, HeaderValue::from_static("close"));
    r
}

/// Open Graph and Twitter tags: what chat apps show in the link card.
fn card_tags(title: &str, description: &str, image: Option<&str>, alt: &str) -> String {
    let mut tags = vec![("property", "og:type", "website"), ("property", "og:site_name", "Swiftlane"),
        ("property", "og:title", title), ("property", "og:description", description),
        ("name", "twitter:card", if image.is_some() { "summary_large_image" } else { "summary" }),
        ("name", "twitter:title", title), ("name", "twitter:description", description)];
    if let Some(img) = image {
        tags.extend([("property", "og:image", img), ("property", "og:image:type", "image/jpeg"), ("property", "og:image:alt", alt),
            ("name", "twitter:image", img), ("name", "twitter:image:alt", alt)]);
    }
    tags.iter().map(|(k, v, c)| format!("<meta {k}=\"{v}\" content=\"{}\">", esc(c))).collect()
}

struct Ctx {
    app: AppHandle,
    srv: Arc<ServerState>,
    headers: HeaderMap,
    method: Method,
}

impl Ctx {
    /// Tailscale adds the visitor's address as the last X-Forwarded-For entry. Only programs on
    /// this PC can reach this server, so the header can be trusted that far.
    fn client_ip(&self) -> String {
        let f = self.headers.get("X-Forwarded-For").and_then(|v| v.to_str().ok()).unwrap_or("");
        let last = f.rsplit(',').next().unwrap_or("").trim();
        if last.is_empty() { "127.0.0.1".into() } else { last.chars().take(64).collect() }
    }

    fn host(&self) -> String {
        self.headers.get(header::HOST).and_then(|v| v.to_str().ok()).unwrap_or("").to_lowercase()
    }

    fn host_ok(&self) -> bool {
        let h = self.host();
        let port = self.srv.port;
        if h == format!("127.0.0.1:{port}") || h == format!("localhost:{port}") {
            return true;
        }
        self.srv.host_name.lock().unwrap().as_ref().is_some_and(|n| {
            let n = n.to_lowercase();
            h == n || h == format!("{n}:443")
        })
    }

    fn cookie(&self, link: &Value) -> Option<String> {
        let want = format!("sl_{}", link["id"].as_str().unwrap_or(""));
        self.headers.get_all(header::COOKIE).iter().filter_map(|v| v.to_str().ok()).flat_map(|v| v.split(';'))
            .filter_map(|p| p.trim().split_once('=')).find(|(k, _)| *k == want).map(|(_, v)| v.to_string())
    }

    /// https://<this PC's Funnel address> for link-card pictures (chat apps need full addresses).
    fn public_base(&self) -> String {
        let h = self.host();
        match self.srv.host_name.lock().unwrap().as_ref().map(|n| n.to_lowercase()) {
            Some(n) if h == n || h == format!("{n}:443") => format!("https://{n}"),
            _ => format!("http://{h}"),
        }
    }

    fn shares(&self) -> &share::Shares {
        &self.app.state::<AppState>().inner().shares
    }
}

fn query(uri: &axum::http::Uri) -> HashMap<String, String> {
    uri.query().map(|q| url_decode_pairs(q)).unwrap_or_default()
}

fn url_decode_pairs(q: &str) -> HashMap<String, String> {
    q.split('&').filter_map(|kv| {
        let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
        let dec = |s: &str| percent_encoding::percent_decode_str(&s.replace('+', " ")).decode_utf8_lossy().to_string();
        (!k.is_empty()).then(|| (dec(k), dec(v)))
    }).collect()
}

// ------------------------------------------------------------------ requests

async fn handle(State(app): State<AppHandle>, req: Request) -> Response {
    let Some(srv) = server(&app) else { return plain(StatusCode::SERVICE_UNAVAILABLE) };
    let (parts, body) = req.into_parts();
    let ctx = Ctx { app: app.clone(), srv: srv.clone(), headers: parts.headers.clone(), method: parts.method.clone() };
    if !ctx.host_ok() {
        return plain(StatusCode::MISDIRECTED_REQUEST);
    }
    let path = parts.uri.path().to_string();
    let q = query(&parts.uri);
    if ctx.method == Method::GET {
        let nonce = srv.ready_nonce.lock().unwrap().clone();
        if let Some(n) = nonce {
            if constant_time_eq::constant_time_eq(path.as_bytes(), format!("{READY_PATH}{n}").as_bytes()) {
                return plain(StatusCode::NO_CONTENT); // Swiftlane's own check reached us through the internet
            }
        }
    }
    let segs: Vec<&str> = path.split('/').collect();
    if segs.len() < 3 || segs[1] != "s" {
        return gone();
    }
    let Some((sid_share, link_id)) = ctx.shares().find(segs[2]) else { return gone() };
    let rest = segs[3..].join("/");
    let Some(s) = ctx.shares().get(&sid_share) else { return gone() };
    let Some(link) = s["links"].as_array().and_then(|a| a.iter().find(|l| l["id"] == link_id)).cloned() else { return gone() };

    match ctx.method.clone() {
        Method::POST => {
            if s["kind"] == "request" && rest == "upload" {
                let _busy = Busy::new(&app);
                return receive_upload(&ctx, &s, &link, &q, body).await;
            }
            if rest.is_empty() && !s["password"].is_null() {
                return password_post(&ctx, &s, &link, body).await;
            }
            gone()
        }
        Method::HEAD | Method::GET => {
            // Only the page and previews answer HEAD; file links would otherwise use up a download.
            if ctx.method == Method::HEAD && !(rest.is_empty() || rest.starts_with("t/")) {
                return plain(StatusCode::METHOD_NOT_ALLOWED);
            }
            if rest.is_empty() {
                return show_page(&ctx, &s, &link);
            }
            if s["kind"] == "request" {
                if rest == "request.js" {
                    let mut r = REQUEST_JS.into_response();
                    r.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("text/javascript; charset=utf-8"));
                    common(r.headers_mut());
                    return r;
                }
                return gone();
            }
            let number = |p: &str| rest.strip_prefix(p).filter(|n| n.len() < 10 && !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())).and_then(|n| n.parse::<usize>().ok());
            if let Some(n) = number("t/") {
                return send_preview(&ctx, &s, &link, n, &q).await;
            }
            let _busy = Busy::new(&app);
            if rest == "zip" {
                return send_zip(&ctx, &s, &link, q.get("s").cloned()).await;
            }
            if let Some(n) = number("f/") {
                return send_file(&ctx, &s, &link, n, q.get("s").cloned()).await;
            }
            gone()
        }
        _ => plain(StatusCode::METHOD_NOT_ALLOWED),
    }
}

fn password_page(error: &str, status: StatusCode, head: &str, extra: Vec<(header::HeaderName, String)>) -> Response {
    let err = if error.is_empty() { String::new() } else { format!("<p class=\"err\">{}</p>", esc(error)) };
    page(status, "Password needed", &format!("<h1>Password needed</h1><p>Enter the password you were given to see the files.</p>{err}\
        <form method=\"post\"><div class=\"top\"><input type=\"password\" name=\"password\" autofocus autocomplete=\"off\" aria-label=\"Password\">\
        <button type=\"submit\">Open</button></div></form>"), head, PAGE_CSP, extra)
}

async fn password_post(ctx: &Ctx, s: &Value, link: &Value, body: Body) -> Response {
    let len = ctx.headers.get(header::CONTENT_LENGTH).and_then(|v| v.to_str().ok()).and_then(|v| v.parse::<usize>().ok()).unwrap_or(usize::MAX);
    if len > 4096 {
        return plain(StatusCode::PAYLOAD_TOO_LARGE);
    }
    let Ok(bytes) = axum::body::to_bytes(body, 4096).await else { return plain(StatusCode::PAYLOAD_TOO_LARGE) };
    let form = url_decode_pairs(&String::from_utf8_lossy(&bytes));
    let (share_id, link_id) = (s["id"].as_str().unwrap_or(""), link["id"].as_str().unwrap_or(""));
    let sid = ctx.cookie(link);
    let sess = ctx.shares().session(share_id, link_id, sid.as_deref());
    let open = { let inner = ctx.shares().inner.lock().unwrap(); share::link_open(&inner, s, link) };
    if !open && !sess.as_ref().is_some_and(|x| x["counted"].as_bool().unwrap_or(false)) {
        return gone();
    }
    if sess.is_none() {
        return show_page(ctx, s, link);
    }
    let ip = ctx.client_ip();
    let (k_ip, k_link) = (format!("ip:{link_id}:{ip}"), format!("link:{link_id}"));
    if ctx.srv.locked(&[k_ip.clone(), k_link.clone()]) {
        return password_page("Too many wrong tries. Wait 15 minutes and try again.", StatusCode::TOO_MANY_REQUESTS, "", vec![]);
    }
    let given = form.get("password").cloned().unwrap_or_default();
    let stored = s["password"].clone();
    let ok = tokio::task::spawn_blocking(move || share::check_password(&given, &stored)).await.unwrap_or(false);
    if ok {
        ctx.srv.lockout.lock().unwrap().remove(&k_ip);
        ctx.shares().unlock(share_id, sid.as_deref().unwrap_or(""));
        return redirect(&format!("/s/{}", link["token"].as_str().unwrap_or("")));
    }
    ctx.srv.fail(k_ip, PW_FAILS_PER_ADDRESS);
    ctx.srv.fail(k_link, PW_FAILS_PER_LINK);
    config::log(&format!("Share {:?}: wrong password from {ip}", s["title"].as_str().unwrap_or("")));
    password_page("Wrong password.", StatusCode::OK, "", vec![])
}

fn card(ctx: &Ctx, s: &Value, link: &Value) -> String {
    if !s["password"].is_null() {
        return card_tags("Password-protected files", "Shared with Swiftlane. Open the link and enter the password you were given.", None, "");
    }
    let items = s["items"].as_array().cloned().unwrap_or_default();
    let what = if items.len() == 1 { file_kind(items[0]["name"].as_str().unwrap_or("")) } else { format!("{} files", items.len()) };
    let desc = format!("{what} · {} · available for {}", human(s["total"].as_f64().unwrap_or(0.0)), left(s["expires"].as_f64().unwrap_or(0.0) - now()));
    let image = card_picture(s).map(|n| format!("{}/s/{}/t/{n}", ctx.public_base(), link["token"].as_str().unwrap_or("")));
    let alt = format!("Preview of {}", items.first().map(|i| base_name(i["name"].as_str().unwrap_or(""))).unwrap_or(""));
    card_tags(s["title"].as_str().unwrap_or(""), &desc, image.as_deref(), &alt)
}

fn show_page(ctx: &Ctx, s: &Value, link: &Value) -> Response {
    let (share_id, link_id) = (s["id"].as_str().unwrap_or(""), link["id"].as_str().unwrap_or(""));
    let token = link["token"].as_str().unwrap_or("");
    let mut sid = ctx.cookie(link);
    let mut sess = ctx.shares().session(share_id, link_id, sid.as_deref());
    let counted = sess.as_ref().is_some_and(|x| x["counted"].as_bool().unwrap_or(false));
    let open = { let inner = ctx.shares().inner.lock().unwrap(); share::link_open(&inner, s, link) };
    if !is_active(s) || link["blocked"].as_bool().unwrap_or(false) || (!counted && !open) {
        return gone();
    }
    let mut extra = vec![];
    if sess.is_none() {
        let new = ctx.shares().new_session(share_id, link_id);
        sess = ctx.shares().session(share_id, link_id, Some(&new));
        let secure = if ctx.headers.get("X-Forwarded-Proto").is_some_and(|v| v == "https") { "; Secure" } else { "" };
        extra.push((header::SET_COOKIE, format!("sl_{link_id}={new}; Path=/s/{token}; HttpOnly; SameSite=Lax{secure}")));
        sid = Some(new);
    }
    let sid = sid.unwrap_or_default();
    let unlocked = sess.as_ref().is_some_and(|x| x["unlocked"].as_bool().unwrap_or(false));
    if !s["password"].is_null() && !unlocked {
        return password_page("", StatusCode::OK, &card(ctx, s, link), extra);
    }
    let title = s["title"].as_str().unwrap_or("");
    let left_text = left(s["expires"].as_f64().unwrap_or(0.0) - now());
    if s["kind"] == "request" {
        let files_left = s["max_files"].as_u64().unwrap_or(0).saturating_sub(s["items"].as_array().map(|a| a.len() as u64).unwrap_or(0));
        let bytes_left = s["max_bytes"].as_u64().unwrap_or(0).saturating_sub(s["total"].as_u64().unwrap_or(0));
        let q = percent_encoding::utf8_percent_encode(&sid, percent_encoding::NON_ALPHANUMERIC);
        let body = format!("<h1>{}</h1><p>Send files to the person who shared this link. Up to {files_left} more file{}, {} in total · open for {left_text}.</p>\
            <div class=\"box\" id=\"drop\" style=\"padding:28px;text-align:center\"><p style=\"margin:0 0 12px\">Choose files or drop them here</p>\
            <input type=\"file\" id=\"pick\" multiple style=\"max-width:none\"></div><div class=\"box\" id=\"list\" style=\"margin-top:14px\" hidden></div>\
            <p class=\"foot\">Files go straight to the sender’s PC. They can’t be seen by anyone else through this link.</p>\
            <span id=\"cfg\" data-url=\"/s/{token}/upload?s={q}\" data-max=\"{bytes_left}\" data-files=\"{files_left}\" hidden></span>\
            <script src=\"/s/{token}/request.js\"></script>",
            esc(title), if files_left != 1 { "s" } else { "" }, human(bytes_left as f64));
        return page(StatusCode::OK, title, &body, "", REQUEST_CSP, extra);
    }
    let q = format!("?s={}", percent_encoding::utf8_percent_encode(&sid, percent_encoding::NON_ALPHANUMERIC));
    let base = format!("/s/{token}");
    let items = s["items"].as_array().cloned().unwrap_or_default();
    let files = items.len();
    let mut rows = String::new();
    for (n, item) in items.iter().take(2000).enumerate() {
        let name = item["name"].as_str().unwrap_or("");
        let thumb = if previewable(item) && n < 300 {
            format!("<img class=\"th\" src=\"{base}/t/{n}{q}&amp;small=1\" alt=\"\" loading=\"lazy\" width=\"44\" height=\"44\">")
        } else {
            let e: String = ext(name).chars().skip(1).take(4).collect::<String>().to_uppercase();
            format!("<span class=\"th ext\" aria-hidden=\"true\">{}</span>", esc(if e.is_empty() { "FILE" } else { &e }))
        };
        let warn = if is_program(name) { "<span class=\"warn\">Program · only run it if you trust the sender</span>" } else { "" };
        let dl = if files > 1 { format!("<a class=\"dl\" href=\"{base}/f/{n}{q}\" download>Download</a>") } else { String::new() };
        rows.push_str(&format!("<div class=\"row\">{thumb}<span class=\"name\">{}<small>{} · {}</small>{warn}</span>{dl}</div>",
            esc(name), file_kind(name), human(item["size"].as_f64().unwrap_or(0.0))));
    }
    if files > 2000 {
        rows.push_str(&format!("<div class=\"row\"><span class=\"name\">and {} more files (in “Download all”)</span></div>", files - 2000));
    }
    let hero = if files == 1 && previewable(&items[0]) {
        format!("<img class=\"hero\" src=\"{base}/t/0{q}\" alt=\"Preview of {}\">", esc(base_name(items[0]["name"].as_str().unwrap_or(""))))
    } else { String::new() };
    let programs: Vec<&str> = items.iter().filter_map(|i| i["name"].as_str()).filter(|n| is_program(n)).collect();
    let note = match programs.len() {
        0 => String::new(),
        1 => format!("<p class=\"note\">“{}” is a program. Only run it if you trust the sender.</p>", esc(base_name(programs[0]))),
        n => format!("<p class=\"note\">{n} of these files are programs. Only run them if you trust the sender.</p>"),
    };
    let total = human(s["total"].as_f64().unwrap_or(0.0));
    let big = if files > 1 {
        format!("<a class=\"btn big\" href=\"{base}/zip{q}\">Download all ({total}, .zip)</a>")
    } else {
        format!("<a class=\"btn big\" href=\"{base}/f/0{q}\" download>Download ({total})</a>")
    };
    let body = format!("<h1>{}</h1><p>{files} file{} · {total} · available for {left_text}</p>{hero}{note}{big}<div class=\"box\">{rows}</div>\
        <p class=\"foot\">Sent with Swiftlane, straight from the sender’s PC.</p>", esc(title), if files != 1 { "s" } else { "" });
    page(StatusCode::OK, title, &body, &card(ctx, s, link), PAGE_CSP, extra)
}

// ------------------------------------------------------------------ previews

/// A fresh JPEG of the picture, at most `side` pixels on its longest side; None if it can't be
/// made. Upright, and saved from a new blank image, so no EXIF/GPS or other metadata carries over.
fn make_preview(path: &Path, side: u32) -> Option<Vec<u8>> {
    use image::ImageDecoder;
    let reader = image::ImageReader::open(path).ok()?.with_guessed_format().ok()?;
    let (w, h) = reader.into_dimensions().ok()?; // judged from the header, before anything is decoded
    if w == 0 || h == 0 || (w as u64) * (h as u64) > PREVIEW_MAX_PIXELS {
        return None;
    }
    let mut decoder = image::ImageReader::open(path).ok()?.with_guessed_format().ok()?.into_decoder().ok()?;
    let orientation = decoder.orientation().ok();
    let mut img = image::DynamicImage::from_decoder(decoder).ok()?;
    if let Some(o) = orientation {
        img.apply_orientation(o);
    }
    let small = img.thumbnail(side, side);
    let rgba = small.to_rgba8();
    let mut clean = image::RgbImage::new(rgba.width(), rgba.height());
    for (x, y, p) in rgba.enumerate_pixels() {
        let a = p[3] as u32; // flatten transparency onto white
        let mix = |c: u8| ((c as u32 * a + 255 * (255 - a)) / 255) as u8;
        clean.put_pixel(x, y, image::Rgb([mix(p[0]), mix(p[1]), mix(p[2])]));
    }
    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 82).encode_image(&clean).ok()?;
    Some(out)
}

fn preview(srv: &ServerState, path: &str, side: u32) -> Option<Vec<u8>> {
    let meta = std::fs::symlink_metadata(path).ok()?;
    if share::is_link(&meta) || !meta.is_file() || meta.len() == 0 || meta.len() >= PREVIEW_MAX_BYTES {
        return None;
    }
    let mtime = meta.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_nanos()).unwrap_or(0);
    let key = format!("{}|{mtime}|{}|{side}", path.to_lowercase(), meta.len());
    let cached = |srv: &ServerState| srv.previews.lock().unwrap().0.iter().find(|(k, _)| *k == key).map(|(_, v)| v.clone());
    if let Some(v) = cached(srv) {
        return v;
    }
    let _one_at_a_time = srv.making.lock().unwrap();
    if let Some(v) = cached(srv) {
        return v;
    }
    let data = make_preview(Path::new(path), side);
    let mut c = srv.previews.lock().unwrap();
    c.1 += data.as_ref().map_or(0, Vec::len);
    c.0.push_back((key, data.clone()));
    while c.1 > PREVIEW_CACHE_BYTES && c.0.len() > 1 {
        if let Some((_, old)) = c.0.pop_front() {
            c.1 -= old.map_or(0, |o| o.len());
        }
    }
    data
}

/// /s/<token>/t/<n>: a picture's preview. Same rules as the page; never counts as a download.
/// Without a visit (chat apps fetching the link card) only the link card's picture is given.
async fn send_preview(ctx: &Ctx, s: &Value, link: &Value, index: usize, q: &HashMap<String, String>) -> Response {
    let Some(item) = s["items"].get(index).filter(|i| previewable(i)).cloned() else { return gone() };
    let (share_id, link_id) = (s["id"].as_str().unwrap_or(""), link["id"].as_str().unwrap_or(""));
    let sid = q.get("s").cloned().or_else(|| ctx.cookie(link));
    let sess = ctx.shares().session(share_id, link_id, sid.as_deref());
    let counted = sess.as_ref().is_some_and(|x| x["counted"].as_bool().unwrap_or(false));
    let open = { let inner = ctx.shares().inner.lock().unwrap(); share::link_open(&inner, s, link) };
    if !is_active(s) || link["blocked"].as_bool().unwrap_or(false) || (!counted && !open) {
        return gone();
    }
    if !s["password"].is_null() && !sess.as_ref().is_some_and(|x| x["unlocked"].as_bool().unwrap_or(false)) {
        return gone();
    }
    if sess.is_none() && Some(index) != card_picture(s) {
        return gone();
    }
    let side = if q.contains_key("small") { PREVIEW_SMALL } else { PREVIEW_BIG };
    let srv = ctx.srv.clone();
    let path = item["path"].as_str().unwrap_or("").to_string();
    let Some(data) = tokio::task::spawn_blocking(move || preview(&srv, &path, side)).await.ok().flatten() else { return gone() };
    let mut r = if ctx.method == Method::HEAD { Response::new(Body::empty()) } else { Response::new(Body::from(data.clone())) };
    r.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("image/jpeg"));
    r.headers_mut().insert(header::CONTENT_LENGTH, HeaderValue::from(data.len()));
    common(r.headers_mut());
    r
}

// ------------------------------------------------------------------ sending files

/// Ok(()) if this download may start (and it is counted), else the answer to send.
fn allowed(ctx: &Ctx, s: &Value, link: &Value, sid: Option<String>, item: Option<usize>, resume: bool) -> Result<(), Response> {
    let sid = sid.or_else(|| ctx.cookie(link));
    let (share_id, link_id) = (s["id"].as_str().unwrap_or(""), link["id"].as_str().unwrap_or(""));
    match ctx.shares().start_download(share_id, link_id, sid.as_deref(), item, resume) {
        Ok(fresh) => {
            if fresh {
                crate::sharing::downloaded(&ctx.app, s, link);
            }
            Ok(())
        }
        Err("password") | Err("no-session") => Err(redirect(&format!("/s/{}", link["token"].as_str().unwrap_or("")))),
        Err(_) => Err(gone()),
    }
}

/// (start, end) for one "bytes=" range; None for the whole file; Err for an unsatisfiable one.
fn parse_range(value: Option<&str>, size: u64) -> Result<Option<(u64, u64)>, ()> {
    let Some(v) = value.map(str::trim).filter(|v| !v.is_empty()) else { return Ok(None) };
    let Some(spec) = v.strip_prefix("bytes=") else { return Ok(None) };
    let Some((a, b)) = spec.split_once('-') else { return Ok(None) };
    if a.is_empty() && b.is_empty() || !a.bytes().chain(b.bytes()).all(|c| c.is_ascii_digit()) {
        return Ok(None);
    }
    let (start, end) = if !a.is_empty() {
        let start: u64 = a.parse().map_err(|_| ())?;
        let end = if b.is_empty() { size.saturating_sub(1) } else { b.parse::<u64>().map_err(|_| ())?.min(size.saturating_sub(1)) };
        (start, end)
    } else {
        (size.saturating_sub(b.parse::<u64>().map_err(|_| ())?), size.saturating_sub(1))
    };
    if start >= size || start > end {
        return Err(());
    }
    Ok(Some((start, end)))
}

async fn send_file(ctx: &Ctx, s: &Value, link: &Value, index: usize, sid: Option<String>) -> Response {
    let Some(item) = s["items"].get(index).cloned() else { return gone() };
    let path = PathBuf::from(item["path"].as_str().unwrap_or(""));
    let unavailable = || page(StatusCode::GONE, "File not available", "<h1>That file isn’t available any more</h1><p>The sender moved or deleted it.</p>", "", PAGE_CSP, vec![]);
    let Ok(meta) = std::fs::symlink_metadata(&path) else { return unavailable() };
    if share::is_link(&meta) || !meta.is_file() {
        return unavailable();
    }
    let Ok(mut f) = tokio::fs::File::open(&path).await else { return unavailable() };
    let size = f.metadata().await.map(|m| m.len()).unwrap_or(0);
    let ranged = match parse_range(ctx.headers.get(header::RANGE).and_then(|v| v.to_str().ok()), size) {
        Ok(r) => r,
        Err(()) => {
            let mut r = plain(StatusCode::RANGE_NOT_SATISFIABLE);
            r.headers_mut().insert(header::CONTENT_RANGE, HeaderValue::from_str(&format!("bytes */{size}")).unwrap());
            return r;
        }
    };
    if let Err(r) = allowed(ctx, s, link, sid, Some(index), ranged.is_some_and(|(a, _)| a > 0)) {
        return r;
    }
    let (start, end) = ranged.unwrap_or((0, size.saturating_sub(1)));
    let length = if size == 0 { 0 } else { end - start + 1 };
    if f.seek(std::io::SeekFrom::Start(start)).await.is_err() {
        return unavailable();
    }
    let name = base_name(item["name"].as_str().unwrap_or("file")).to_string();
    let (title, label) = (s["title"].as_str().unwrap_or("").to_string(), link["label"].as_str().unwrap_or("").to_string());
    let started = Instant::now();
    let whole = ranged.is_none();
    let mut sent = 0u64;
    let stream = tokio_util::io::ReaderStream::with_capacity(f.take(length), 1024 * 1024).map(move |chunk| {
        if let Ok(b) = &chunk {
            sent += b.len() as u64;
            if sent == length && whole {
                let secs = started.elapsed().as_secs_f64().max(0.001);
                config::log(&format!("Share {title:?}: sent {name} ({}, {}/s) via “{label}”", human(size as f64), human(size as f64 / secs)));
            }
        }
        chunk
    });
    let mut r = Response::new(Body::from_stream(stream));
    *r.status_mut() = if ranged.is_some() { StatusCode::PARTIAL_CONTENT } else { StatusCode::OK };
    let h = r.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static("application/octet-stream"));
    h.insert(header::CONTENT_LENGTH, HeaderValue::from(length));
    h.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    if ranged.is_some() {
        h.insert(header::CONTENT_RANGE, HeaderValue::from_str(&format!("bytes {start}-{end}/{size}")).unwrap());
    }
    if let Ok(v) = HeaderValue::from_str(&disposition(base_name(item["name"].as_str().unwrap_or("file")))) {
        h.insert(header::CONTENT_DISPOSITION, v);
    }
    common(h);
    r
}

/// Sends what the zip writer writes, piece by piece, without keeping the whole zip anywhere.
struct ChannelWriter(tokio::sync::mpsc::Sender<Result<Bytes, std::io::Error>>, u64);
impl std::io::Write for ChannelWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.blocking_send(Ok(Bytes::copy_from_slice(buf))).map_err(|_| std::io::Error::other("visitor went away"))?;
        self.1 += buf.len() as u64;
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// "Download all": a zip made while it's sent (stored, not compressed; large files allowed).
async fn send_zip(ctx: &Ctx, s: &Value, link: &Value, sid: Option<String>) -> Response {
    let items = s["items"].as_array().cloned().unwrap_or_default();
    if items.len() < 2 {
        return gone();
    }
    if let Err(r) = allowed(ctx, s, link, sid, None, false) {
        return r;
    }
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Bytes, std::io::Error>>(8);
    let (title, label) = (s["title"].as_str().unwrap_or("").to_string(), link["label"].as_str().unwrap_or("").to_string());
    let busy = Busy::new(&ctx.app);
    tokio::task::spawn_blocking(move || {
        let _busy = busy;
        let mut zip = zip::ZipWriter::new_stream(ChannelWriter(tx, 0));
        for item in &items {
            let path = PathBuf::from(item["path"].as_str().unwrap_or(""));
            let Ok(meta) = std::fs::symlink_metadata(&path) else { continue };
            if share::is_link(&meta) || !meta.is_file() {
                continue;
            }
            let Ok(mut src) = std::fs::File::open(&path) else { continue };
            let mtime = meta.modified().ok().map(chrono::DateTime::<chrono::Local>::from);
            let mut opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored).large_file(meta.len() >= 0xFFFF_FFFF);
            if let Some(t) = mtime.and_then(|t| {
                use chrono::{Datelike, Timelike};
                zip::DateTime::from_date_and_time(t.year().clamp(1980, 2107) as u16, t.month() as u8, t.day() as u8, t.hour() as u8, t.minute() as u8, t.second() as u8).ok()
            }) {
                opts = opts.last_modified_time(t);
            }
            if zip.start_file(item["name"].as_str().unwrap_or("file"), opts).is_err() || std::io::copy(&mut src, &mut zip).is_err() {
                return;
            }
        }
        if let Ok(w) = zip.finish() {
            config::log(&format!("Share {title:?}: sent all as zip ({}) via “{label}”", human(w.into_inner().1 as f64)));
        }
    });
    let stream = tokio_stream_from(rx);
    let mut r = Response::new(Body::from_stream(stream));
    r.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("application/zip"));
    if let Ok(v) = HeaderValue::from_str(&disposition(&format!("{}.zip", s["title"].as_str().unwrap_or("Shared files")))) {
        r.headers_mut().insert(header::CONTENT_DISPOSITION, v);
    }
    common(r.headers_mut());
    r
}

fn tokio_stream_from(mut rx: tokio::sync::mpsc::Receiver<Result<Bytes, std::io::Error>>) -> impl futures_util::Stream<Item = Result<Bytes, std::io::Error>> {
    futures_util::stream::poll_fn(move |cx| rx.poll_recv(cx))
}

// ------------------------------------------------------------------ request links: receiving files

/// Windows' Mark of the Web: Windows warns before opening it, Office uses Protected View and
/// SmartScreen checks programs, exactly as for a file downloaded from a website.
fn mark_from_internet(path: &Path) {
    if cfg!(windows) {
        let mut ads = path.as_os_str().to_owned();
        ads.push(":Zone.Identifier");
        let _ = std::fs::write(PathBuf::from(ads), "[ZoneTransfer]\r\nZoneId=3\r\nReferrerUrl=about:internet\r\n");
    }
}

async fn receive_upload(ctx: &Ctx, s: &Value, link: &Value, q: &HashMap<String, String>, body: Body) -> Response {
    let (share_id, link_id) = (s["id"].as_str().unwrap_or("").to_string(), link["id"].as_str().unwrap_or("").to_string());
    let sid = q.get("s").cloned().or_else(|| ctx.cookie(link));
    let sess = ctx.shares().session(&share_id, &link_id, sid.as_deref());
    if sess.is_none() || link["blocked"].as_bool().unwrap_or(false)
        || (!s["password"].is_null() && !sess.as_ref().is_some_and(|x| x["unlocked"].as_bool().unwrap_or(false))) {
        return json_err(StatusCode::FORBIDDEN, "Open the link again.");
    }
    if ctx.headers.get(header::TRANSFER_ENCODING).is_some() {
        return json_err(StatusCode::LENGTH_REQUIRED, "Unsupported upload.");
    }
    let Some(size) = ctx.headers.get(header::CONTENT_LENGTH).and_then(|v| v.to_str().ok()).and_then(|v| v.parse::<u64>().ok()) else {
        return json_err(StatusCode::LENGTH_REQUIRED, "Unsupported upload.");
    };
    if size == 0 {
        return json_err(StatusCode::BAD_REQUEST, "Empty file.");
    }
    if let Err(problem) = ctx.shares().reserve(&share_id, size) {
        return json_err(StatusCode::PAYLOAD_TOO_LARGE, &problem);
    }
    let folder = PathBuf::from(s["folder"].as_str().unwrap_or(""));
    let _ = std::fs::create_dir_all(&folder);
    if fs4::available_space(&folder).map(|free| free < size + UPLOAD_HEADROOM).unwrap_or(true) {
        ctx.shares().release(&share_id, size);
        return json_err(StatusCode::INSUFFICIENT_STORAGE, "The receiving PC doesn't have enough free space.");
    }
    let name = safe_name(q.get("name").map(String::as_str).unwrap_or(""));
    let tmp = folder.join(format!(".{}.part", { let mut b = [0u8; 8]; let _ = getrandom::getrandom(&mut b); hex::encode(b) }));
    let result: Result<PathBuf, String> = async {
        let mut f = tokio::fs::OpenOptions::new().write(true).create_new(true).open(&tmp).await.map_err(|e| e.to_string())?;
        let mut stream = body.into_data_stream();
        let mut done = 0u64;
        while let Some(chunk) = tokio::time::timeout(Duration::from_secs(60), stream.next()).await.map_err(|_| "went silent".to_string())? {
            let chunk = chunk.map_err(|_| "interrupted".to_string())?;
            done += chunk.len() as u64;
            if done > size {
                return Err("more than announced".into());
            }
            f.write_all(&chunk).await.map_err(|e| e.to_string())?;
        }
        if done != size {
            return Err("interrupted".into());
        }
        f.flush().await.map_err(|e| e.to_string())?;
        drop(f);
        let final_path = place_file(&tmp, &folder, &name).map_err(|e| e.to_string())?;
        mark_from_internet(&final_path);
        Ok(final_path)
    }.await;
    match result {
        Ok(final_path) => {
            let fname = final_path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            ctx.shares().received(&share_id, &link_id, &fname, size, &ctx.client_ip());
            crate::sharing::uploaded(&ctx.app, s, &fname);
            let mut r = (StatusCode::OK, axum::Json(json!({ "saved": fname }))).into_response();
            common(r.headers_mut());
            r
        }
        Err(e) => {
            let _ = tokio::fs::remove_file(&tmp).await;
            ctx.shares().release(&share_id, size);
            config::log(&format!("Request {:?}: upload failed ({e})", s["title"].as_str().unwrap_or("")));
            json_err(StatusCode::BAD_REQUEST, "The upload didn't finish. Try again.")
        }
    }
}

// ------------------------------------------------------------------ starting and stopping

/// Starts the share server on 127.0.0.1:8766 (only reachable through Tailscale Funnel).
pub fn start(app: &AppHandle, host: Option<String>) -> Result<(), String> {
    let st = app.state::<AppState>();
    if st.sharing.server.lock().unwrap().is_some() {
        return Ok(());
    }
    let port = share::SHARE_PORT;
    let listener = crate::transfer::bind_exclusive_on([127, 0, 0, 1], port)
        .map_err(|_| format!("Port {port} is used by another program, so links can't work."))?;
    let srv = Arc::new(ServerState::new(host, port));
    let (stop_tx, mut stop_rx) = tokio::sync::oneshot::channel::<()>();
    *st.sharing.server.lock().unwrap() = Some((srv.clone(), stop_tx));
    let router = Router::new().fallback(handle).layer(axum::extract::DefaultBodyLimit::disable()).with_state(app.clone());
    tauri::async_runtime::spawn(async move {
        let Ok(listener) = tokio::net::TcpListener::from_std(listener) else { return };
        loop {
            tokio::select! {
                _ = &mut stop_rx => break,
                accepted = listener.accept() => {
                    let Ok((stream, _)) = accepted else { continue };
                    // too many at once: drop instead of queueing
                    let Ok(permit) = srv.slots.clone().try_acquire_owned() else { continue };
                    let router = router.clone();
                    tokio::spawn(async move {
                        let _permit = permit;
                        let service = hyper_util::service::TowerToHyperService::new(router);
                        let _ = hyper::server::conn::http1::Builder::new()
                            .timer(hyper_util::rt::TokioTimer::new())
                            .header_read_timeout(Duration::from_secs(30))
                            .serve_connection(hyper_util::rt::TokioIo::new(stream), service)
                            .await;
                    });
                }
            }
        }
    });
    config::log(&format!("Share server on (127.0.0.1:{port}, only reachable through Tailscale Funnel)"));
    Ok(())
}

pub fn stop(app: &AppHandle) {
    if let Some((_, tx)) = app.state::<AppState>().sharing.server.lock().unwrap().take() {
        let _ = tx.send(());
        config::log("Share server off");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges() {
        assert_eq!(parse_range(Some("bytes=0-9"), 100), Ok(Some((0, 9))));
        assert_eq!(parse_range(Some("bytes=90-"), 100), Ok(Some((90, 99))));
        assert_eq!(parse_range(Some("bytes=-10"), 100), Ok(Some((90, 99))));
        assert_eq!(parse_range(Some("bytes=200-"), 100), Err(()));
        assert_eq!(parse_range(Some("items=1-2"), 100), Ok(None));
        assert_eq!(parse_range(None, 100), Ok(None));
    }

    #[test]
    fn escaping_and_names() {
        assert_eq!(esc("<script>\"x\"&'"), "&lt;script&gt;&quot;x&quot;&amp;&#x27;");
        assert_eq!(file_kind("a/b/setup.EXE"), "Program");
        assert_eq!(file_kind("x.weird"), "WEIRD file");
        assert!(disposition("naïve \"x\".pdf").starts_with("attachment; filename=\"na_ve _x_.pdf\""));
    }

    #[test]
    fn previews_drop_metadata() {
        let dir = std::env::temp_dir().join(format!("swl-prev-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("pic.png");
        image::RgbaImage::from_pixel(300, 200, image::Rgba([10, 20, 30, 128])).save(&p).unwrap();
        let jpg = make_preview(&p, 96).expect("a preview");
        assert!(jpg.starts_with(&[0xff, 0xd8]));
        let img = image::load_from_memory(&jpg).unwrap();
        assert_eq!((img.width(), img.height()), (96, 64));
        assert!(!jpg.windows(4).any(|w| w == b"Exif"));
        let _ = std::fs::remove_dir_all(dir);
    }
}
