//! Phone transfer: the small web server your phone talks to (port 8765), the same rules as the
//! Python version (server.py):
//! - only your own network, this PC or your Tailscale network may connect;
//! - every request needs the pairing code; 10 wrong codes lock that address out for 15 minutes;
//! - uploads stream to a hidden temporary file, then get a safe name and move into their type
//!   folder (Photos, Videos...) without ever overwriting anything;
//! - files for the phone are served through signed links that expire after 10 minutes;
//! - the port is claimed exclusively, so no other program can listen on it too.

use axum::body::Body;
use axum::extract::{Path as UrlPath, Query, Request, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Router};
use futures_util::StreamExt;
use hmac::{Hmac, Mac};
use serde_json::{json, Value};
use sha2::Sha256;
use std::collections::{HashMap, VecDeque};
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Manager};
use tokio::io::AsyncWriteExt;

use crate::api::{push, AppState};
use crate::{config, tls};

const FAIL_LIMIT: u32 = 10;
const LOCKOUT: Duration = Duration::from_secs(15 * 60);
const LINK_SECONDS: u64 = 10 * 60;
const DISK_HEADROOM: u64 = 512 * 1024 * 1024;
const IDLE: Duration = Duration::from_secs(120); // drop uploads that go silent
const PAGE_CSP: &str = "default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; \
    connect-src 'self'; img-src 'self' data:; manifest-src 'self'; form-action 'none'; frame-ancestors 'none'";

const CATEGORIES: &[(&str, &[&str])] = &[
    ("Photos", &["jpg", "jpeg", "heic", "heif", "png", "gif", "webp", "bmp", "tif", "tiff", "dng", "raw", "cr2", "nef", "arw", "svg"]),
    ("Videos", &["mov", "mp4", "m4v", "avi", "mkv", "webm", "hevc", "3gp", "wmv"]),
    ("Audio", &["mp3", "m4a", "wav", "aac", "flac", "ogg", "aiff", "caf", "wma", "opus"]),
    ("Documents", &["pdf", "doc", "docx", "xls", "xlsx", "ppt", "pptx", "txt", "rtf", "csv", "md", "pages", "numbers", "key", "odt", "ods", "odp", "epub", "json"]),
    ("Archives", &["zip", "rar", "7z", "tar", "gz", "bz2", "xz"]),
];

fn now_secs() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

fn hex_token(bytes: usize) -> String {
    let mut b = vec![0u8; bytes];
    getrandom::getrandom(&mut b).expect("Windows' random number source failed");
    hex::encode(b)
}

// ------------------------------------------------------------------ shared state

pub struct Transfer {
    lockout: Mutex<HashMap<IpAddr, (u32, Option<Instant>)>>,
    outbox: Mutex<Vec<Value>>,
    received: Mutex<VecDeque<Value>>,
    ids: AtomicU64,
    link_secret: [u8; 32],
    place_lock: Mutex<()>,
    pub running: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    pub error: Mutex<Option<String>>,
    notify: Mutex<(Vec<String>, u64)>,
    pub ca: Mutex<Option<std::sync::Arc<tls::Authority>>>,
    acceptor: Mutex<Option<tokio_rustls::TlsAcceptor>>,
    tls_addrs: Mutex<Vec<String>>,
}

impl Transfer {
    pub fn new() -> Self {
        let mut secret = [0u8; 32];
        getrandom::getrandom(&mut secret).expect("Windows' random number source failed");
        Transfer {
            lockout: Mutex::new(HashMap::new()),
            outbox: Mutex::new(Vec::new()),
            received: Mutex::new(VecDeque::new()),
            ids: AtomicU64::new(1),
            link_secret: secret, // download links die with the app
            place_lock: Mutex::new(()),
            running: Mutex::new(None),
            error: Mutex::new(None),
            notify: Mutex::new((Vec::new(), 0)),
            ca: Mutex::new(None),
            acceptor: Mutex::new(None),
            tls_addrs: Mutex::new(Vec::new()),
        }
    }

    fn next_id(&self) -> String {
        self.ids.fetch_add(1, Ordering::Relaxed).to_string()
    }

    fn locked(&self, ip: IpAddr) -> bool {
        self.lockout.lock().unwrap().get(&ip).and_then(|e| e.1).is_some_and(|until| until > Instant::now())
    }

    /// Counts a wrong code; true once the address is locked out.
    fn fail(&self, ip: IpAddr) -> bool {
        let mut l = self.lockout.lock().unwrap();
        let e = l.entry(ip).or_insert((0, None));
        if e.1.is_some_and(|until| until <= Instant::now()) {
            *e = (0, None);
        }
        e.0 += 1;
        if e.0 >= FAIL_LIMIT {
            e.1 = Some(Instant::now() + LOCKOUT);
            return true;
        }
        false
    }

    fn clear(&self, ip: IpAddr) {
        self.lockout.lock().unwrap().remove(&ip);
    }

    fn sign(&self, id: &str, expires: u64) -> String {
        let mut mac = Hmac::<Sha256>::new_from_slice(&self.link_secret).unwrap();
        mac.update(format!("{id}:{expires}").as_bytes());
        hex::encode(mac.finalize().into_bytes())[..32].to_string()
    }

    fn download_url(&self, item: &Value) -> String {
        let id = item["id"].as_str().unwrap_or("");
        let expires = now_secs() as u64 + LINK_SECONDS;
        let name = percent_encoding::utf8_percent_encode(item["name"].as_str().unwrap_or(""), percent_encoding::NON_ALPHANUMERIC);
        format!("/dl/{id}/{name}?exp={expires}&sig={}", self.sign(id, expires))
    }

    // ---- outbox: files waiting on the PC for the phone (not copied)
    pub fn outbox_add(&self, paths: &[PathBuf]) {
        let mut ob = self.outbox.lock().unwrap();
        for p in paths {
            let Ok(meta) = std::fs::metadata(p) else { continue };
            let path = p.to_string_lossy().to_string();
            if !meta.is_file() || ob.iter().any(|i| i["path"] == path) {
                continue;
            }
            ob.push(json!({ "id": self.next_id(), "path": path, "name": p.file_name().map(|n| n.to_string_lossy()).unwrap_or_default(),
                            "size": meta.len(), "added": now_secs(), "downloads": 0 }));
        }
    }

    pub fn outbox_list(&self) -> Value {
        let mut items = self.outbox.lock().unwrap().clone();
        items.sort_by(|a, b| b["added"].as_f64().partial_cmp(&a["added"].as_f64()).unwrap_or(std::cmp::Ordering::Equal));
        json!(items)
    }

    pub fn outbox_remove(&self, id: &str) {
        self.outbox.lock().unwrap().retain(|i| i["id"] != id);
    }

    pub fn outbox_clear(&self) {
        self.outbox.lock().unwrap().clear();
    }

    fn outbox_get(&self, id: &str) -> Option<Value> {
        self.outbox.lock().unwrap().iter().find(|i| i["id"] == id).cloned()
    }

    fn outbox_downloaded(&self, id: &str) {
        if let Some(i) = self.outbox.lock().unwrap().iter_mut().find(|i| i["id"] == id) {
            i["downloads"] = json!(i["downloads"].as_u64().unwrap_or(0) + 1);
        }
    }

    // ---- received
    pub fn received_list(&self) -> Value {
        json!(self.received.lock().unwrap().iter().cloned().collect::<Vec<_>>())
    }

    pub fn find_received(&self, id: &str) -> Option<String> {
        self.received.lock().unwrap().iter().find(|e| e["id"] == id).and_then(|e| e["path"].as_str().map(String::from))
    }

    fn add_received(&self, path: &Path, size: u64, ip: IpAddr) -> Value {
        let entry = json!({
            "id": self.next_id(),
            "name": path.file_name().map(|n| n.to_string_lossy()).unwrap_or_default(),
            "path": path.to_string_lossy(),
            "folder": path.parent().and_then(|p| p.file_name()).map(|n| n.to_string_lossy()).unwrap_or_default(),
            "size": size, "time": now_secs(), "from": ip.to_string(),
        });
        let mut r = self.received.lock().unwrap();
        r.push_front(entry.clone());
        r.truncate(200);
        record_created(path);
        entry
    }
}

/// Files Swiftlane made, so the uninstaller can offer to delete only those (same list as before).
pub fn record_created(path: &Path) {
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(config::config_dir().join("created-files.txt")) {
        let _ = writeln!(f, "{}", path.display());
    }
}

// ------------------------------------------------------------------ helpers (same rules as server.py)

/// Only your own network, this PC, or your Tailscale network.
pub fn allowed_client(ip: IpAddr) -> bool {
    let ip = match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(IpAddr::V6(v6)),
        other => other,
    };
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            v4.is_loopback() || v4.is_private() || v4.is_link_local() || (o[0] == 100 && (64..=127).contains(&o[1]))
        }
        IpAddr::V6(v6) => v6.is_loopback() || (v6.segments()[0] & 0xfe00) == 0xfc00 || (v6.segments()[0] & 0xffc0) == 0xfe80,
    }
}

/// A plain Windows file name from whatever the phone sent (no folders, no reserved names).
pub fn safe_name(raw: &str) -> String {
    let base = raw.rsplit(['/', '\\']).next().unwrap_or("");
    let cleaned: String = base.chars().map(|c| if "<>:\"|?*".contains(c) || c.is_control() { '_' } else { c }).collect();
    let mut name = cleaned.trim().trim_end_matches(['.', ' ']).trim_start_matches('.').to_string();
    if name.is_empty() {
        name = format!("Phone {}", config::stamp_now());
    }
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (name[..i].to_string(), name[i..].to_string()),
        _ => (name.clone(), String::new()),
    };
    let first = stem.split('.').next().unwrap_or("").to_uppercase();
    let reserved = ["CON", "PRN", "AUX", "NUL"].contains(&first.as_str())
        || ((first.starts_with("COM") || first.starts_with("LPT")) && first.len() == 4 && first.as_bytes()[3].is_ascii_digit() && first.as_bytes()[3] != b'0');
    let stem = if reserved { format!("_{stem}") } else { stem };
    let stem: String = stem.chars().take(150).collect();
    let ext: String = ext.chars().take(16).collect();
    stem + &ext
}

pub fn category_for(name: &str) -> &'static str {
    let ext = name.rsplit_once('.').map(|(_, e)| e.to_lowercase()).unwrap_or_default();
    CATEGORIES.iter().find(|(_, exts)| exts.contains(&ext.as_str())).map(|(f, _)| *f).unwrap_or("Other")
}

/// Moves `src` into `folder` as `name`, adding (1), (2)... instead of overwriting anything.
pub fn place_file(src: &Path, folder: &Path, name: &str) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(folder)?;
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    for i in 0..10_000 {
        let candidate = folder.join(if i == 0 { name.to_string() } else { format!("{stem} ({i}){ext}") });
        if candidate.exists() {
            continue;
        }
        // create_new reserves the name atomically, so two arrivals can't take the same one
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&candidate) {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
        let moved = std::fs::rename(src, &candidate).or_else(|_| std::fs::copy(src, &candidate).map(|_| ()).and_then(|_| std::fs::remove_file(src)));
        return match moved {
            Ok(()) => Ok(candidate),
            Err(e) => {
                let _ = std::fs::remove_file(&candidate);
                Err(e)
            }
        };
    }
    Err(std::io::Error::other("no free file name"))
}

/// Best-guess extension for a file that arrived without one.
fn sniff_extension(path: &Path, content_type: &str) -> &'static str {
    let mut head = [0u8; 16];
    if let Ok(mut f) = std::fs::File::open(path) {
        let _ = std::io::Read::read(&mut f, &mut head);
    }
    if head.starts_with(b"\xff\xd8\xff") {
        return ".jpg";
    }
    if head.starts_with(b"\x89PNG") {
        return ".png";
    }
    if head.starts_with(b"%PDF") {
        return ".pdf";
    }
    if &head[4..8] == b"ftyp" {
        return match &head[8..12] {
            b"heic" | b"heix" | b"mif1" | b"msf1" => ".heic",
            b"qt  " => ".mov",
            _ => ".mp4",
        };
    }
    match content_type.split(';').next().unwrap_or("").trim().to_lowercase().as_str() {
        "image/jpeg" => ".jpg",
        "image/heic" => ".heic",
        "image/heif" => ".heif",
        "image/png" => ".png",
        "video/quicktime" => ".mov",
        "video/mp4" => ".mp4",
        "application/pdf" => ".pdf",
        _ => "",
    }
}

pub fn human(n: f64) -> String {
    let mut n = n;
    for unit in ["B", "KB", "MB", "GB"] {
        if n < 1024.0 || unit == "GB" {
            return if unit == "B" { format!("{n:.0} {unit}") } else { format!("{n:.1} {unit}") };
        }
        n /= 1024.0;
    }
    unreachable!()
}

/// [(label, ip)]: the address on your Wi-Fi/LAN, and a Tailscale address if there is one.
pub fn local_addresses() -> Vec<(String, String)> {
    let mut found = Vec::new();
    if let Ok(s) = std::net::UdpSocket::bind("0.0.0.0:0") {
        // picks the LAN interface; nothing is sent
        if s.connect("8.8.8.8:80").is_ok() {
            if let Ok(addr) = s.local_addr() {
                found.push(("Wi-Fi / LAN".to_string(), addr.ip().to_string()));
            }
        }
    }
    if let Ok(ifs) = if_addrs::get_if_addrs() {
        for i in ifs {
            if let IpAddr::V4(v4) = i.ip() {
                let o = v4.octets();
                let ip = v4.to_string();
                if o[0] == 100 && (64..=127).contains(&o[1]) && !found.iter().any(|f| f.1 == ip) {
                    found.push(("Tailscale".to_string(), ip));
                }
            }
        }
    }
    if found.is_empty() {
        found.push(("This PC".to_string(), "127.0.0.1".to_string()));
    }
    found
}

/// The QR code for `text` as an SVG (dark squares on a transparent background).
pub fn qr_svg(text: &str) -> Option<String> {
    let code = qrcode::QrCode::with_error_correction_level(text, qrcode::EcLevel::M).ok()?;
    let n = code.width();
    let colors = code.to_colors();
    let mut path = String::new();
    for y in 0..n {
        for x in 0..n {
            if colors[y * n + x] == qrcode::Color::Dark {
                path.push_str(&format!("M{x},{y}h1v1h-1z"));
            }
        }
    }
    Some(format!(r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {n} {n}" shape-rendering="crispEdges"><path fill="#0b0b14" d="{path}"/></svg>"##))
}

// ------------------------------------------------------------------ notifications

/// Files that arrive close together become one Windows notification.
fn notify_received(app: &AppHandle, name: String) {
    let st = app.state::<AppState>();
    if !st.cfg.lock().unwrap()["notify"].as_bool().unwrap_or(true) {
        return;
    }
    let ticket = {
        let mut n = st.transfer.notify.lock().unwrap();
        n.0.push(name);
        n.1 += 1;
        n.1
    };
    let app = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(1500));
        let st = app.state::<AppState>();
        let names = {
            let mut n = st.transfer.notify.lock().unwrap();
            if n.1 != ticket {
                return; // a later file restarted the wait
            }
            std::mem::take(&mut n.0)
        };
        let title = if names.len() == 1 { "File received".to_string() } else { format!("{} files received", names.len()) };
        let mut text = names.iter().take(3).cloned().collect::<Vec<_>>().join(", ");
        if names.len() > 3 {
            text.push_str(" ...");
        }
        crate::notify::show(&app, &title, &text);
    });
}

// ------------------------------------------------------------------ the server

#[derive(Clone, Copy)]
struct Conn {
    ip: IpAddr,
    secure: bool,
    local: SocketAddr,
}

const CERT_PATHS: &[&str] = &["/cert", "/swiftlane.mobileconfig", "/swiftlane-ca.cer"];
const PAGES: &[&str] = &["/", "/manifest.webmanifest", "/icon-180.png", "/icon-512.png", "/cast"];

/// Checks every request goes through first: the address, the lockout, and "only encrypted
/// connections" (plain pages are sent to the encrypted address; the certificate pages stay reachable).
async fn guard(State(app): State<AppHandle>, Extension(c): Extension<Conn>, req: Request, next: axum::middleware::Next) -> Response {
    if let Some(r) = client_ok(&app, c.ip) {
        return r;
    }
    let path = req.uri().path().to_string();
    let st = app.state::<AppState>();
    let https_only = st.cfg.lock().unwrap()["https_only"].as_bool().unwrap_or(false);
    if !c.secure && https_only && st.transfer.ca.lock().unwrap().is_some() && !CERT_PATHS.contains(&path.as_str()) {
        if req.method() == axum::http::Method::GET && PAGES.contains(&path.as_str()) {
            let to = format!("https://{}:{}{}", c.local.ip(), c.local.port(), path);
            let mut r = StatusCode::MOVED_PERMANENTLY.into_response();
            if let Ok(v) = HeaderValue::from_str(&to) {
                r.headers_mut().insert(header::LOCATION, v);
            }
            return r;
        }
        return json_resp(StatusCode::FORBIDDEN, json!({ "error": "This PC only accepts encrypted connections. Open Swiftlane through its secure link (https://)." }));
    }
    next.run(req).await
}

fn json_resp(code: StatusCode, v: Value) -> Response {
    let mut r = (code, axum::Json(v)).into_response();
    common_headers(r.headers_mut());
    r
}

fn common_headers(h: &mut HeaderMap) {
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    h.insert("X-Content-Type-Options", HeaderValue::from_static("nosniff"));
    h.insert("Referrer-Policy", HeaderValue::from_static("no-referrer"));
}

/// Address and lockout checks every request goes through.
fn client_ok(app: &AppHandle, ip: IpAddr) -> Option<Response> {
    if !allowed_client(ip) {
        config::log(&format!("Refused {ip}: not on your local network"));
        return Some(json_resp(StatusCode::FORBIDDEN, json!({ "error": "Not on the local network." })));
    }
    if app.state::<AppState>().transfer.locked(ip) {
        return Some(json_resp(StatusCode::TOO_MANY_REQUESTS, json!({ "error": "Too many wrong codes. Try again in 15 minutes." })));
    }
    None
}

/// The pairing code check (constant time), with the lockout.
fn authorized(app: &AppHandle, ip: IpAddr, headers: &HeaderMap) -> Option<Response> {
    let st = app.state::<AppState>();
    let supplied = config::normalize_code(headers.get("X-Token").and_then(|v| v.to_str().ok()).unwrap_or(""));
    let code = config::normalize_code(st.cfg.lock().unwrap()["code"].as_str().unwrap_or(""));
    if !code.is_empty() && constant_time_eq::constant_time_eq(supplied.as_bytes(), code.as_bytes()) {
        st.transfer.clear(ip);
        return None;
    }
    if st.transfer.fail(ip) {
        config::log(&format!("Locked out {ip} for 15 minutes after {FAIL_LIMIT} wrong codes"));
    } else {
        config::log(&format!("Wrong pairing code from {ip}"));
    }
    Some(json_resp(StatusCode::UNAUTHORIZED, json!({ "error": "Wrong pairing code." })))
}

/// The phone page and its icons, straight from the files built into Swiftlane.
fn static_file(app: &AppHandle, name: &str, content_type: &'static str) -> Response {
    let Some(asset) = app.asset_resolver().get(name.to_string()) else {
        return json_resp(StatusCode::NOT_FOUND, json!({ "error": "Not found." }));
    };
    let mut r = Response::new(Body::from(asset.bytes));
    r.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    if content_type.starts_with("text/html") {
        r.headers_mut().insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static(PAGE_CSP));
    }
    common_headers(r.headers_mut());
    r
}

async fn page(State(app): State<AppHandle>, req: Request) -> Response {
    match req.uri().path() {
        "/" => static_file(&app, "phone.html", "text/html; charset=utf-8"),
        "/manifest.webmanifest" => static_file(&app, "manifest.webmanifest", "application/manifest+json"),
        "/icon-180.png" => static_file(&app, "icon-180.png", "image/png"),
        "/icon-512.png" => static_file(&app, "icon-512.png", "image/png"),
        "/cast" => static_file(&app, "cast.html", "text/html; charset=utf-8"),
        "/cert" | "/swiftlane.mobileconfig" | "/swiftlane-ca.cer" => cert_file(&app, req.uri().path()),
        _ => json_resp(StatusCode::NOT_FOUND, json!({ "error": "Not found." })),
    }
}

/// The certificate pages: how to trust Swiftlane's CA on the phone.
fn cert_file(app: &AppHandle, path: &str) -> Response {
    let Some(ca) = app.state::<AppState>().transfer.ca.lock().unwrap().clone() else {
        return json_resp(StatusCode::NOT_FOUND, json!({ "error": "Encrypted connections aren't available on this PC." }));
    };
    let (body, ctype, disposition): (Vec<u8>, &'static str, Option<&'static str>) = match path {
        "/cert" => {
            let Some(page) = app.asset_resolver().get("cert.html".to_string()) else {
                return json_resp(StatusCode::NOT_FOUND, json!({ "error": "Not found." }));
            };
            let html = String::from_utf8_lossy(&page.bytes).replace("{{FINGERPRINT}}", &ca.fingerprint());
            (html.into_bytes(), "text/html; charset=utf-8", None)
        }
        "/swiftlane.mobileconfig" => (ca.mobileconfig(), "application/x-apple-aspen-config", Some("attachment; filename=\"Swiftlane.mobileconfig\"")),
        _ => (ca.der.clone(), "application/x-x509-ca-cert", Some("attachment; filename=\"Swiftlane.cer\"")),
    };
    let mut r = Response::new(Body::from(body));
    r.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static(ctype));
    if let Some(d) = disposition {
        r.headers_mut().insert(header::CONTENT_DISPOSITION, HeaderValue::from_static(d));
    } else {
        r.headers_mut().insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static(PAGE_CSP));
    }
    common_headers(r.headers_mut());
    r
}

async fn remote_action(State(app): State<AppHandle>, Extension(c): Extension<Conn>, req: Request) -> Response {
    if let Some(r) = authorized(&app, c.ip, req.headers()) {
        return r;
    }
    let enabled = app
        .state::<AppState>()
        .cfg
        .lock()
        .unwrap()["features"]["remote"]
        .as_bool()
        .unwrap_or(false);
    if !enabled {
        return json_resp(
            StatusCode::FORBIDDEN,
            json!({ "error": "The PC remote is switched off in Swiftlane's settings." }),
        );
    }
    let bad = || json_resp(StatusCode::BAD_REQUEST, json!({ "error": "Bad request." }));
    let length = req
        .headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0);
    if !(1..=256).contains(&length) {
        return bad();
    }
    let Ok(Ok(body)) =
        tokio::time::timeout(Duration::from_secs(5), axum::body::to_bytes(req.into_body(), 256))
            .await
    else {
        return bad();
    };
    let action = serde_json::from_slice::<Value>(&body)
        .ok()
        .and_then(|v| v.get("action").and_then(Value::as_str).map(String::from))
        .unwrap_or_default();
    if !crate::tools::remote::allow_press() {
        return json_resp(
            StatusCode::TOO_MANY_REQUESTS,
            json!({ "error": "Too many presses. Wait a moment." }),
        );
    }
    let ip = c.ip.to_string();
    match crate::tools::remote::handle_phone_action(&app, &action, &ip) {
        Ok(()) => json_resp(StatusCode::OK, json!({ "ok": true })),
        Err(problem) => json_resp(StatusCode::FORBIDDEN, json!({ "error": problem })),
    }
}

fn pc_name() -> String {
    std::env::var("COMPUTERNAME").or_else(|_| std::env::var("HOSTNAME")).unwrap_or_else(|_| "PC".into())
}

async fn ping(State(app): State<AppHandle>, Extension(c): Extension<Conn>, headers: HeaderMap) -> Response {
    if let Some(r) = authorized(&app, c.ip, &headers) {
        return r;
    }
    let cfg = app.state::<AppState>().cfg.lock().unwrap().clone();
    json_resp(StatusCode::OK, json!({
        "ok": true, "app": "Swiftlane", "pc": pc_name(), "lang": cfg["language"],
        "remote": cfg["features"]["remote"].as_bool().unwrap_or(false), "power": cfg["remote_power"].as_bool().unwrap_or(false),
        "secure": c.secure, "tls": app.state::<AppState>().transfer.ca.lock().unwrap().is_some(),
    }))
}

async fn outbox(State(app): State<AppHandle>, Extension(c): Extension<Conn>, headers: HeaderMap) -> Response {
    if let Some(r) = authorized(&app, c.ip, &headers) {
        return r;
    }
    let st = app.state::<AppState>();
    let items: Vec<Value> = st.transfer.outbox_list().as_array().cloned().unwrap_or_default().iter()
        .map(|i| json!({ "id": i["id"], "name": i["name"], "size": i["size"], "url": st.transfer.download_url(i) }))
        .collect();
    let cfg = st.cfg.lock().unwrap();
    json_resp(StatusCode::OK, json!({ "items": items, "remote": cfg["features"]["remote"], "power": cfg["remote_power"] }))
}

#[derive(serde::Deserialize)]
struct DlQuery {
    exp: Option<String>,
    sig: Option<String>,
}

/// A queued file. Links are signed and expire, so they work in a plain <a href>.
async fn download(State(app): State<AppHandle>, Extension(c): Extension<Conn>, UrlPath((id, _name)): UrlPath<(String, String)>, Query(q): Query<DlQuery>) -> Response {
    let st = app.state::<AppState>();
    let exp = q.exp.unwrap_or_default();
    let valid = exp.parse::<u64>().ok().filter(|e| (*e as f64) > now_secs()).is_some_and(|e| {
        constant_time_eq::constant_time_eq(q.sig.unwrap_or_default().as_bytes(), st.transfer.sign(&id, e).as_bytes())
    });
    if !valid {
        return json_resp(StatusCode::FORBIDDEN, json!({ "error": "This link has expired. Refresh the page." }));
    }
    let Some(item) = st.transfer.outbox_get(&id) else {
        return json_resp(StatusCode::NOT_FOUND, json!({ "error": "That file is no longer shared." }));
    };
    let path = PathBuf::from(item["path"].as_str().unwrap_or(""));
    let name = item["name"].as_str().unwrap_or("file").to_string();
    let Ok(file) = tokio::fs::File::open(&path).await else {
        return json_resp(StatusCode::NOT_FOUND, json!({ "error": "That file is no longer shared." }));
    };
    let size = file.metadata().await.map(|m| m.len()).unwrap_or(0);
    let ascii: String = name.chars().map(|ch| if ch.is_ascii() && ch != '"' && !ch.is_ascii_control() { ch } else { '_' }).collect();
    let utf8 = percent_encoding::utf8_percent_encode(&name, percent_encoding::NON_ALPHANUMERIC);
    let mime = mime_guess::from_path(&name).first_or_octet_stream().to_string();
    let app2 = app.clone();
    let (id2, name2, ip) = (id.clone(), name.clone(), c.ip);
    let started = Instant::now();
    let mut sent = 0u64;
    let stream = tokio_util::io::ReaderStream::with_capacity(file, 1024 * 1024).map(move |chunk| {
        if let Ok(b) = &chunk {
            sent += b.len() as u64;
            if sent == size {
                let secs = started.elapsed().as_secs_f64().max(0.001);
                config::log(&format!("Sent {name2} to {ip}  ({}, {}/s)", human(size as f64), human(size as f64 / secs)));
                let st = app2.state::<AppState>();
                st.transfer.outbox_downloaded(&id2);
                push(&app2, "sent", json!({ "id": id2, "name": name2 }));
            }
        }
        chunk
    });
    let mut r = Response::new(Body::from_stream(stream));
    let h = r.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_str(&mime).unwrap_or(HeaderValue::from_static("application/octet-stream")));
    h.insert(header::CONTENT_LENGTH, HeaderValue::from(size));
    if let Ok(v) = HeaderValue::from_str(&format!("attachment; filename=\"{ascii}\"; filename*=UTF-8''{utf8}")) {
        h.insert(header::CONTENT_DISPOSITION, v);
    }
    common_headers(h);
    r
}

#[derive(serde::Deserialize)]
struct UploadQuery {
    name: Option<String>,
}

/// A file from the phone: streamed to a hidden temporary file, then moved into place.
async fn upload(State(app): State<AppHandle>, Extension(c): Extension<Conn>, Query(q): Query<UploadQuery>, req: Request) -> Response {
    if let Some(r) = authorized(&app, c.ip, req.headers()) {
        return r;
    }
    let headers = req.headers().clone();
    if headers.get(header::TRANSFER_ENCODING).is_some() {
        return json_resp(StatusCode::LENGTH_REQUIRED, json!({ "error": "Content-Length required." }));
    }
    let Some(length) = headers.get(header::CONTENT_LENGTH).and_then(|v| v.to_str().ok()).and_then(|v| v.parse::<u64>().ok()) else {
        return json_resp(StatusCode::LENGTH_REQUIRED, json!({ "error": "Content-Length required." }));
    };
    let st = app.state::<AppState>();
    let (save_dir, max_bytes, sort) = {
        let cfg = st.cfg.lock().unwrap();
        (PathBuf::from(cfg["save_dir"].as_str().unwrap_or("")),
         (cfg["max_file_gb"].as_f64().unwrap_or(20.0) * 1024f64.powi(3)) as u64,
         cfg["sort_files"].as_bool().unwrap_or(true))
    };
    if length > max_bytes {
        return json_resp(StatusCode::PAYLOAD_TOO_LARGE, json!({ "error": format!("File is larger than the {} limit.", human(max_bytes as f64)) }));
    }
    let _ = std::fs::create_dir_all(&save_dir);
    if fs4::available_space(&save_dir).map(|free| free < length + DISK_HEADROOM).unwrap_or(true) {
        return json_resp(StatusCode::INSUFFICIENT_STORAGE, json!({ "error": "Not enough disk space on the PC." }));
    }
    let raw = q.name.filter(|n| !n.is_empty()).or_else(|| {
        headers.get("X-Filename").and_then(|v| v.to_str().ok()).map(|v| percent_encoding::percent_decode_str(v).decode_utf8_lossy().to_string())
    }).unwrap_or_default();
    let mut name = safe_name(&raw);
    let transfer = hex_token(4);
    let tmp = save_dir.join(format!(".{}.part", hex_token(8)));
    push(&app, "incoming", json!({ "transfer": transfer, "name": name, "done": 0, "total": length }));
    let started = Instant::now();

    let result: Result<(), String> = async {
        let mut f = tokio::fs::OpenOptions::new().write(true).create_new(true).open(&tmp).await.map_err(|e| e.to_string())?;
        let mut body = req.into_body().into_data_stream();
        let (mut done, mut last) = (0u64, Instant::now());
        while let Some(chunk) = tokio::time::timeout(IDLE, body.next()).await.map_err(|_| "upload went silent".to_string())? {
            let chunk = chunk.map_err(|_| "upload interrupted".to_string())?;
            done += chunk.len() as u64;
            if done > length {
                return Err("more data than announced".into());
            }
            f.write_all(&chunk).await.map_err(|e| e.to_string())?;
            if last.elapsed() > Duration::from_millis(200) {
                last = Instant::now();
                push(&app, "incoming", json!({ "transfer": transfer, "name": name, "done": done, "total": length }));
            }
        }
        if done != length {
            return Err("upload interrupted".into());
        }
        f.flush().await.map_err(|e| e.to_string())?;
        Ok(())
    }.await;

    let placed = result.and_then(|()| {
        if !name.contains('.') || name.ends_with('.') {
            name.push_str(sniff_extension(&tmp, headers.get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).unwrap_or("")));
        }
        let folder = if sort { save_dir.join(category_for(&name)) } else { save_dir.clone() };
        let _guard = st.transfer.place_lock.lock().unwrap();
        place_file(&tmp, &folder, &name).map_err(|e| e.to_string())
    });
    match placed {
        Ok(final_path) => {
            let secs = started.elapsed().as_secs_f64().max(0.001);
            let fname = final_path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            config::log(&format!("Saved {fname}  ({}, {}/s)", human(length as f64), human(length as f64 / secs)));
            let entry = st.transfer.add_received(&final_path, length, c.ip);
            push(&app, "received", json!({ "transfer": transfer, "entry": entry }));
            notify_received(&app, fname.clone());
            json_resp(StatusCode::OK, json!({ "saved": fname, "bytes": length }))
        }
        Err(e) => {
            let _ = tokio::fs::remove_file(&tmp).await;
            config::log(&format!("Failed receiving {name:?} from {}: {e}", c.ip));
            push(&app, "incoming-failed", json!({ "transfer": transfer, "name": name }));
            json_resp(StatusCode::BAD_REQUEST, json!({ "error": "Upload failed." }))
        }
    }
}

// ---- cast from a browser (tools/casting.rs): only passes the connection descriptions along

#[derive(serde::Deserialize)]
struct CastQuery {
    id: Option<String>,
}

/// The cast hub if casting from a browser is on and the link's code is right (constant time; wrong
/// codes count towards the same lockout as wrong pairing codes).
fn cast_hub(app: &AppHandle, ip: IpAddr, headers: &HeaderMap) -> Result<&'static crate::tools::casting::hub::CastHub, Response> {
    let hub = crate::tools::casting::cast_hub();
    if !hub.on() {
        return Err(json_resp(StatusCode::FORBIDDEN, json!({ "error": "Casting to this PC is switched off in Swiftlane." })));
    }
    let st = app.state::<AppState>();
    if hub.check(headers.get("X-Cast-Token").and_then(|v| v.to_str().ok()).unwrap_or("")) {
        st.transfer.clear(ip);
        return Ok(hub);
    }
    if st.transfer.fail(ip) {
        config::log(&format!("Locked out {ip} for 15 minutes after {FAIL_LIMIT} wrong codes"));
    }
    Err(json_resp(StatusCode::UNAUTHORIZED, json!({ "error": "This link is out of date. Open the new one Swiftlane shows." })))
}

async fn cast_offer(State(app): State<AppHandle>, Extension(c): Extension<Conn>, req: Request) -> Response {
    let hub = match cast_hub(&app, c.ip, req.headers()) {
        Ok(h) => h,
        Err(r) => return r,
    };
    let bad = || json_resp(StatusCode::BAD_REQUEST, json!({ "error": "Bad request." }));
    let length = req.headers().get(header::CONTENT_LENGTH).and_then(|v| v.to_str().ok()).and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);
    if !(1..=80 * 1024).contains(&length) {
        return bad();
    }
    let Ok(Ok(body)) = tokio::time::timeout(Duration::from_secs(30), axum::body::to_bytes(req.into_body(), 80 * 1024)).await else {
        return bad();
    };
    let Ok(Value::Object(body)) = serde_json::from_slice::<Value>(&body) else { return bad() };
    let name = match body.get("name") {
        Some(Value::String(s)) => s.clone(),
        None | Some(Value::Null) => String::new(),
        Some(v) => v.to_string(),
    };
    match hub.offer(c.ip, body.get("sdp").and_then(Value::as_str), &name) {
        Ok(o) => {
            config::log(&format!("Cast request from {}", c.ip));
            crate::tools::casting::announce(&app, &o);
            json_resp(StatusCode::OK, json!({ "id": o.id }))
        }
        Err(e @ crate::tools::casting::hub::OfferError::TooMany) => json_resp(StatusCode::TOO_MANY_REQUESTS, json!({ "error": e.message() })),
        Err(e) => json_resp(StatusCode::BAD_REQUEST, json!({ "error": e.message() })),
    }
}

async fn cast_answer(State(app): State<AppHandle>, Extension(c): Extension<Conn>, Query(q): Query<CastQuery>, headers: HeaderMap) -> Response {
    match cast_hub(&app, c.ip, &headers) {
        Ok(hub) => json_resp(StatusCode::OK, hub.status(&q.id.unwrap_or_default(), c.ip, c.local.ip())),
        Err(r) => r,
    }
}

async fn cast_end(State(app): State<AppHandle>, Extension(c): Extension<Conn>, Query(q): Query<CastQuery>, headers: HeaderMap) -> Response {
    match cast_hub(&app, c.ip, &headers) {
        Ok(hub) => {
            let id = q.id.unwrap_or_default();
            if hub.end(&id, Some(c.ip)) {
                crate::tools::casting::laptop_ended(&app, &id); // close its window on the PC
            }
            json_resp(StatusCode::OK, json!({ "ok": true }))
        }
        Err(r) => r,
    }
}

fn router(app: AppHandle) -> Router {
    Router::new()
        .route("/", get(page))
        .route("/manifest.webmanifest", get(page))
        .route("/icon-180.png", get(page))
        .route("/icon-512.png", get(page))
        .route("/cert", get(page))
        .route("/swiftlane.mobileconfig", get(page))
        .route("/swiftlane-ca.cer", get(page))
        .route("/cast", get(page))
        .route("/ping", get(ping))
        .route("/api/outbox", get(outbox))
        .route("/dl/{id}/{name}", get(download))
        .route("/upload", post(upload))
        .route("/api/remote", post(remote_action))
        .route("/api/cast/offer", post(cast_offer))
        .route("/api/cast/end", post(cast_end))
        .route("/api/cast/answer", get(cast_answer))
        .fallback(page)
        .layer(axum::extract::DefaultBodyLimit::disable()) // uploads check their own size limit
        .layer(axum::middleware::from_fn_with_state(app.clone(), guard))
        .with_state(app)
}

/// Claims the port exclusively: on Windows, ordinary address reuse would let another program
/// bind the same port and intercept uploads.
fn bind_exclusive(port: u16) -> std::io::Result<std::net::TcpListener> {
    bind_exclusive_on([0, 0, 0, 0], port)
}

pub fn bind_exclusive_on(ip: [u8; 4], port: u16) -> std::io::Result<std::net::TcpListener> {
    let sock = socket2::Socket::new(socket2::Domain::IPV4, socket2::Type::STREAM, Some(socket2::Protocol::TCP))?;
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawSocket;
        use windows_sys::Win32::Networking::WinSock::{setsockopt, SOL_SOCKET, SO_EXCLUSIVEADDRUSE};
        let on: i32 = 1;
        let rc = unsafe {
            setsockopt(sock.as_raw_socket() as usize, SOL_SOCKET, SO_EXCLUSIVEADDRUSE, &on as *const i32 as *const u8, 4)
        };
        if rc != 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    sock.bind(&SocketAddr::from((ip, port)).into())?;
    sock.listen(128)?;
    sock.set_nonblocking(true)?;
    Ok(sock.into())
}

/// Starts listening (if it isn't already). Errors are plain sentences.
pub fn start(app: &AppHandle) -> Result<(), String> {
    let st = app.state::<AppState>();
    if st.transfer.running.lock().unwrap().is_some() {
        return Ok(());
    }
    let (port, save_dir) = {
        let cfg = st.cfg.lock().unwrap();
        (cfg["port"].as_u64().unwrap_or(8765) as u16, PathBuf::from(cfg["save_dir"].as_str().unwrap_or("")))
    };
    let _ = std::fs::create_dir_all(&save_dir);
    // leftovers from uploads that were cut off last time
    if let Ok(entries) = std::fs::read_dir(&save_dir) {
        for e in entries.flatten() {
            let n = e.file_name().to_string_lossy().to_string();
            if n.starts_with('.') && n.ends_with(".part") {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
    let std_listener = bind_exclusive(port).map_err(|e| {
        let msg = if e.kind() == std::io::ErrorKind::AddrInUse || e.raw_os_error() == Some(10048) || e.raw_os_error() == Some(10013) {
            format!("Port {port} is already in use by another program.")
        } else {
            format!("Couldn't start phone transfer ({e}).")
        };
        *st.transfer.error.lock().unwrap() = Some(msg.clone());
        msg
    })?;
    let (stop_tx, mut stop_rx) = tokio::sync::oneshot::channel::<()>();
    *st.transfer.running.lock().unwrap() = Some(stop_tx);
    *st.transfer.error.lock().unwrap() = None;
    let app2 = app.clone();
    tauri::async_runtime::spawn(async move {
        let listener = match tokio::net::TcpListener::from_std(std_listener) {
            Ok(l) => l,
            Err(_) => return,
        };
        let svc = router(app2.clone());
        loop {
            tokio::select! {
                _ = &mut stop_rx => break,
                accepted = listener.accept() => {
                    let Ok((stream, peer)) = accepted else { continue };
                    let Ok(local) = stream.local_addr() else { continue };
                    let svc = svc.clone();
                    let acceptor = app2.state::<AppState>().transfer.acceptor.lock().unwrap().clone();
                    tokio::spawn(async move {
                        // Encrypted and plain connections share the port: a TLS connection starts with byte 0x16.
                        if let Some(acceptor) = acceptor {
                            let mut first = [0u8; 1];
                            let peeked = tokio::time::timeout(Duration::from_secs(15), stream.peek(&mut first)).await;
                            if matches!(peeked, Ok(Ok(1))) && first[0] == 0x16 {
                                let Ok(Ok(tls)) = tokio::time::timeout(Duration::from_secs(15), acceptor.accept(stream)).await else { return };
                                serve(tls, svc.layer(Extension(Conn { ip: peer.ip(), secure: true, local }))).await;
                                return;
                            }
                        }
                        serve(stream, svc.layer(Extension(Conn { ip: peer.ip(), secure: false, local }))).await;
                    });
                }
            }
        }
        config::log("Phone transfer switched off; no longer listening on the network");
    });
    config::log(&format!("Swiftlane {} listening on port {port}, saving to {}", crate::api::VERSION, save_dir.display()));
    setup_tls(app);
    Ok(())
}

async fn serve<I>(io: I, svc: Router)
where
    I: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let service = hyper_util::service::TowerToHyperService::new(svc);
    let _ = hyper::server::conn::http1::Builder::new()
        .timer(hyper_util::rt::TokioTimer::new())
        .header_read_timeout(Duration::from_secs(30))
        .serve_connection(hyper_util::rt::TokioIo::new(io), service)
        .await;
}

/// Encrypted phone transfer: load (or make) Swiftlane's CA and a certificate for today's addresses.
pub fn setup_tls(app: &AppHandle) {
    let st = app.state::<AppState>();
    let addrs: Vec<String> = local_addresses().into_iter().map(|(_, ip)| ip).collect();
    let ca = match st.transfer.ca.lock().unwrap().clone() {
        Some(ca) => Ok(ca),
        None => tls::Authority::load_or_create().map(std::sync::Arc::new),
    };
    match ca.and_then(|ca| ca.acceptor(&addrs).map(|a| (ca, a))) {
        Ok((ca, acceptor)) => {
            *st.transfer.ca.lock().unwrap() = Some(ca);
            *st.transfer.acceptor.lock().unwrap() = Some(acceptor);
            *st.transfer.tls_addrs.lock().unwrap() = addrs;
        }
        Err(e) => {
            config::log(&format!("Encrypted phone transfer unavailable: {e}"));
            *st.transfer.ca.lock().unwrap() = None;
            *st.transfer.acceptor.lock().unwrap() = None;
        }
    }
}

/// A new network address (other Wi-Fi, Tailscale on/off) needs a new certificate.
pub fn watch_addresses(app: AppHandle) {
    loop {
        std::thread::sleep(Duration::from_secs(60));
        let st = app.state::<AppState>();
        if st.transfer.running.lock().unwrap().is_none() {
            continue;
        }
        let now: Vec<String> = local_addresses().into_iter().map(|(_, ip)| ip).collect();
        if *st.transfer.tls_addrs.lock().unwrap() != now {
            setup_tls(&app);
        }
    }
}

pub fn stop(app: &AppHandle) {
    if let Some(tx) = app.state::<AppState>().transfer.running.lock().unwrap().take() {
        let _ = tx.send(());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert_eq!(safe_name("../../evil.exe"), "evil.exe");
        assert_eq!(safe_name("C:\\x\\a:b?.jpg"), "a_b_.jpg");
        assert_eq!(safe_name("CON.txt"), "_CON.txt");
        assert_eq!(safe_name("com1.jpg"), "_com1.jpg");
        assert_eq!(safe_name(".hidden"), "hidden");
        assert_eq!(category_for("IMG.HEIC"), "Photos");
        assert_eq!(category_for("noext"), "Other");
    }

    #[test]
    fn clients() {
        assert!(allowed_client("192.168.1.5".parse().unwrap()));
        assert!(allowed_client("100.70.1.1".parse().unwrap()));
        assert!(allowed_client("::ffff:10.0.0.2".parse().unwrap()));
        assert!(!allowed_client("8.8.8.8".parse().unwrap()));
        assert!(!allowed_client("100.128.0.1".parse().unwrap()));
    }
}
