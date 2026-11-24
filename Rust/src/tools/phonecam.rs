//! Phone camera & document scan. The phone page's "Camera / Scan" button opens the phone's own
//! camera and uploads each photo through the normal /upload route (same pairing code, lockout and
//! Content-Length rules; transfer.rs hands requests marked `X-Capture` to `receive` below). Captures
//! wait in a private temporary folder that is deleted when Swiftlane closes; nothing reaches the
//! save folder until you click a button here:
//! - "Use as photo": the photo goes to the normal save folder as a JPG (a JPG is copied as it
//!   arrived, like any other transfer; other formats are re-saved as JPG without camera data);
//! - "Scan document": you place four corners, Swiftlane flattens the page (perspective warp,
//!   bilinear sampling), optionally in grey or black and white, and saves JPG or PDF pages. Scans
//!   are re-encoded pictures, so they never carry camera or location data.
//! The corners are only ever applied when you save or preview; nothing is cropped automatically.

use axum::extract::Request;
use axum::http::{header, HeaderMap, StatusCode};
use base64::Engine;
use futures_util::StreamExt;
use image::imageops::{self, FilterType};
use image::{ImageReader, RgbaImage};
use serde_json::{json, Value};
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;
use tauri::{AppHandle, Manager};
use tauri_plugin_dialog::DialogExt;
use tokio::io::AsyncWriteExt;

use super::images::{self, Fmt};
use super::{arg, ask_folder, ask_save};
use crate::api::push;
use crate::jobs::JobError;

const MAX_CAPTURE_BYTES: u64 = 40 * 1024 * 1024; // one photo from the phone
const MAX_CAPTURES: usize = 60; // waiting at once
const MAX_STORE_BYTES: u64 = 600 * 1024 * 1024; // all waiting photos together
const MAX_SOURCE_PIXELS: u64 = 64_000_000; // decoded photo (decompression-bomb guard)
const MAX_SOURCE_SIDE: u32 = 20_000;
const MAX_SIDE: u32 = 4000; // a flattened page
const MAX_PAGES: usize = 200; // per save
const PREVIEW_SIDE: u32 = 1200;
const WORK_SIDE: u32 = 1800; // the picture shown while placing corners
const THUMB_SIDE: u32 = 220;
const IDLE: Duration = Duration::from_secs(120);
const DISK_HEADROOM: u64 = 256 * 1024 * 1024;

pub fn dispatch(app: &AppHandle, method: &str, args: &[Value]) -> Option<Value> {
    Some(match method {
        "cam_list" => list(),
        "cam_open" => open(arg(args, 0).as_str().unwrap_or("")),
        "cam_preview" => preview(arg(args, 0)),
        "cam_use_photo" => use_photo(app, arg(args, 0).as_str().unwrap_or("")),
        "cam_save_jpg" => save_jpg(app, arg(args, 0)),
        "cam_save_pdf" => save_pdf(app, arg(args, 0), arg(args, 1).as_str().unwrap_or("new")),
        "cam_pick" => pick(app),
        "cam_add" => {
            let paths: Vec<PathBuf> = arg(args, 0).as_array().map(|a| a.iter().filter_map(|v| v.as_str()).map(PathBuf::from).collect()).unwrap_or_default();
            // Only paths Windows handed to Rust in a drop on the window; the page can't name other files.
            let paths: Vec<PathBuf> = paths.into_iter().filter(|p| super::biosusb::was_offered(p)).collect();
            add_files(&paths)
        }
        "cam_remove" => {
            remove(arg(args, 0).as_str().unwrap_or(""));
            list()
        }
        "cam_clear" => {
            clear();
            list()
        }
        _ => return None,
    })
}

// ------------------------------------------------------------------ the waiting captures

struct Capture {
    id: String,
    n: u64,
    path: PathBuf,
    size: u64,
    time: f64,
    w: u32,
    h: u32,
    thumb: Option<String>,
}

static STORE: Mutex<Vec<Capture>> = Mutex::new(Vec::new());
static DIR: OnceLock<Option<PathBuf>> = OnceLock::new();
static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

fn token(bytes: usize) -> String {
    let mut b = vec![0u8; bytes];
    getrandom::getrandom(&mut b).expect("Windows' random number source failed");
    hex::encode(b)
}

/// The private folder for waiting captures: made when the first one arrives, removed on exit.
/// Left-overs of a crashed run (older than a day) are swept away when it is made.
fn private_dir() -> Option<&'static Path> {
    DIR.get_or_init(|| {
        let base = std::env::temp_dir();
        if let Ok(rd) = std::fs::read_dir(&base) {
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().to_string();
                let old = e.metadata().and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).is_some_and(|a| a > Duration::from_secs(24 * 3600));
                if name.starts_with("Swiftlane-camera-") && old && e.file_type().is_ok_and(|t| t.is_dir() && !t.is_symlink()) {
                    let _ = std::fs::remove_dir_all(e.path());
                }
            }
        }
        let p = base.join(format!("Swiftlane-camera-{}", token(8)));
        std::fs::create_dir(&p).ok()?;
        Some(p)
    })
    .as_deref()
}

/// Called when Swiftlane closes: waiting captures are deleted.
pub fn shutdown() {
    if let Some(Some(p)) = DIR.get() {
        let _ = std::fs::remove_dir_all(p);
    }
    STORE.lock().unwrap().clear();
}

fn entry(c: &Capture) -> Value {
    json!({ "id": c.id, "name": format!("Photo {}", c.n), "time": c.time, "size": c.size, "w": c.w, "h": c.h, "thumb": c.thumb })
}

fn list() -> Value {
    json!(STORE.lock().unwrap().iter().map(entry).collect::<Vec<_>>())
}

fn path_of(id: &str) -> Option<PathBuf> {
    STORE.lock().unwrap().iter().find(|c| c.id == id).map(|c| c.path.clone())
}

fn remove(id: &str) {
    let mut s = STORE.lock().unwrap();
    if let Some(i) = s.iter().position(|c| c.id == id) {
        let _ = std::fs::remove_file(&s[i].path);
        s.remove(i);
    }
}

fn clear() {
    let mut s = STORE.lock().unwrap();
    for c in s.iter() {
        let _ = std::fs::remove_file(&c.path);
    }
    s.clear();
}

// ------------------------------------------------------------------ receiving (called by transfer.rs)

/// A photo from the phone's camera. transfer.rs has already checked the pairing code and the
/// lockout; this checks the size, the number waiting and that it really is a picture.
pub async fn receive(app: &AppHandle, ip: IpAddr, headers: HeaderMap, req: Request) -> (StatusCode, Value) {
    let err = |code: StatusCode, msg: &str| (code, json!({ "error": msg }));
    if headers.get(header::TRANSFER_ENCODING).is_some() {
        return err(StatusCode::LENGTH_REQUIRED, "Content-Length required.");
    }
    let Some(length) = headers.get(header::CONTENT_LENGTH).and_then(|v| v.to_str().ok()).and_then(|v| v.parse::<u64>().ok()) else {
        return err(StatusCode::LENGTH_REQUIRED, "Content-Length required.");
    };
    if length == 0 || length > MAX_CAPTURE_BYTES {
        return err(StatusCode::PAYLOAD_TOO_LARGE, "That photo is too big for Phone camera (40 MB at most).");
    }
    let Some(dir) = private_dir() else {
        return err(StatusCode::INTERNAL_SERVER_ERROR, "Swiftlane couldn't make its temporary folder.");
    };
    {
        let s = STORE.lock().unwrap();
        if s.len() >= MAX_CAPTURES || s.iter().map(|c| c.size).sum::<u64>() + length > MAX_STORE_BYTES {
            return err(StatusCode::CONFLICT, "Too many photos are waiting on the PC. Remove some in Swiftlane's Phone camera page.");
        }
    }
    if fs4::available_space(dir).map(|free| free < length + DISK_HEADROOM).unwrap_or(true) {
        return err(StatusCode::INSUFFICIENT_STORAGE, "Not enough disk space on the PC.");
    }
    let tmp = dir.join(format!(".{}.part", token(8)));
    let written: Result<(), String> = async {
        let mut f = tokio::fs::OpenOptions::new().write(true).create_new(true).open(&tmp).await.map_err(|e| e.to_string())?;
        let mut body = req.into_body().into_data_stream();
        let mut done = 0u64;
        while let Some(chunk) = tokio::time::timeout(IDLE, body.next()).await.map_err(|_| "upload went silent".to_string())? {
            let chunk = chunk.map_err(|_| "upload interrupted".to_string())?;
            done += chunk.len() as u64;
            if done > length {
                return Err("more data than announced".into());
            }
            f.write_all(&chunk).await.map_err(|e| e.to_string())?;
        }
        if done != length {
            return Err("upload interrupted".into());
        }
        f.flush().await.map_err(|e| e.to_string())
    }
    .await;
    if written.is_err() {
        let _ = tokio::fs::remove_file(&tmp).await;
        crate::config::log(&format!("Failed receiving a camera photo from {ip}"));
        return err(StatusCode::BAD_REQUEST, "Upload failed.");
    }
    let t2 = tmp.clone();
    let registered = tokio::task::spawn_blocking(move || register(&t2, length)).await.unwrap_or_else(|_| Err("failed".into()));
    match registered {
        Ok(e) => {
            crate::config::log(&format!("Camera photo received from {ip} ({} bytes)", length));
            push(app, "camera-capture", e.clone());
            (StatusCode::OK, json!({ "saved": e["name"], "bytes": length }))
        }
        Err(m) => {
            let _ = tokio::fs::remove_file(&tmp).await;
            err(StatusCode::BAD_REQUEST, &m)
        }
    }
}

/// What kind of picture the first bytes say it is.
fn sniff(head: &[u8]) -> Option<&'static str> {
    if head.starts_with(b"\xff\xd8\xff") {
        Some("jpg")
    } else if head.starts_with(b"\x89PNG") {
        Some("png")
    } else if head.len() >= 12 && &head[..4] == b"RIFF" && &head[8..12] == b"WEBP" {
        Some("webp")
    } else if head.len() >= 12 && &head[4..8] == b"ftyp" && matches!(&head[8..12], b"heic" | b"heix" | b"mif1" | b"msf1") {
        Some("heic")
    } else {
        None
    }
}

/// Header-only size check, so a tiny file claiming a huge picture is refused before it is decoded.
fn check_dims(path: &Path) -> Result<(u32, u32), String> {
    let r = ImageReader::open(path).and_then(|r| r.with_guessed_format()).map_err(|_| "Swiftlane couldn't read this photo.".to_string())?;
    let (w, h) = r.into_dimensions().map_err(|_| "This photo seems to be damaged.".to_string())?;
    if w == 0 || h == 0 || w > MAX_SOURCE_SIDE || h > MAX_SOURCE_SIDE || w as u64 * h as u64 > MAX_SOURCE_PIXELS {
        return Err("This photo is too big to open.".into());
    }
    Ok((w, h))
}

/// The photo turned upright, after the size check.
fn load_upright(path: &Path) -> Result<RgbaImage, String> {
    let heic = path.extension().is_some_and(|e| e.eq_ignore_ascii_case("heic"));
    if !heic {
        check_dims(path)?;
    }
    let img = images::open_upright(path)?.img;
    if img.width() as u64 * img.height() as u64 > MAX_SOURCE_PIXELS {
        return Err("This photo is too big to open.".into());
    }
    Ok(img)
}

fn register(tmp: &Path, size: u64) -> Result<Value, String> {
    let mut head = [0u8; 16];
    if let Ok(mut f) = std::fs::File::open(tmp) {
        let _ = std::io::Read::read(&mut f, &mut head);
    }
    let Some(ext) = sniff(&head) else {
        return Err("That isn't a picture Swiftlane can use.".into());
    };
    let header_dims = if ext == "heic" { None } else { Some(check_dims(tmp)?) };
    let id = token(8);
    let path = tmp.with_file_name(format!("{id}.{ext}"));
    std::fs::rename(tmp, &path).map_err(|_| "Swiftlane couldn't keep the photo.".to_string())?;
    let (w, h, thumb) = match load_upright(&path) {
        Ok(img) => {
            let thumb = data_url(&shrink(&img, THUMB_SIDE), 78).ok();
            (img.width(), img.height(), thumb)
        }
        Err(_) => {
            let (w, h) = header_dims.unwrap_or((0, 0));
            (w, h, None)
        }
    };
    let c = Capture { id, n: COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed), path: path.clone(), size, time: now(), w, h, thumb };
    let mut s = STORE.lock().unwrap();
    if s.len() >= MAX_CAPTURES {
        let _ = std::fs::remove_file(&path);
        return Err("Too many photos are waiting on the PC.".into());
    }
    let e = entry(&c);
    s.push(c);
    Ok(e)
}

// ------------------------------------------------------------------ pictures already on the PC

/// "Add pictures from this PC": the file picker runs here, so the chosen paths never pass through the page.
fn pick(app: &AppHandle) -> Value {
    let picked: Vec<PathBuf> = app
        .dialog()
        .file()
        .add_filter("Pictures", &["jpg", "jpeg", "png", "webp", "heic"])
        .blocking_pick_files()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|p| p.into_path().ok())
        .collect();
    if picked.is_empty() {
        return json!({ "cancelled": true });
    }
    add_files(&picked)
}

/// Copies each picture into the private folder (the original is never touched) under the same
/// size, count and picture checks as photos from the phone.
fn add_files(paths: &[PathBuf]) -> Value {
    let mut added = Vec::new();
    let mut errors = Vec::new();
    for p in paths {
        let name = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        match copy_in(p) {
            Ok(e) => added.push(e),
            Err(m) => errors.push(format!("{name}: {m}")),
        }
    }
    json!({ "added": added, "errors": errors, "list": list() })
}

fn copy_in(src: &Path) -> Result<Value, String> {
    let s = src.to_string_lossy();
    if s.starts_with(r"\\") {
        return Err("Pictures on network or device paths can't be added.".into());
    }
    let meta = std::fs::metadata(src).map_err(|_| "Swiftlane couldn't read this file.".to_string())?;
    if !meta.is_file() {
        return Err("That isn't a file.".into());
    }
    let size = meta.len();
    if size == 0 || size > MAX_CAPTURE_BYTES {
        return Err("Pictures can be 40 MB at most.".into());
    }
    let dir = private_dir().ok_or("Swiftlane couldn't make its temporary folder.")?;
    {
        let s = STORE.lock().unwrap();
        if s.len() >= MAX_CAPTURES || s.iter().map(|c| c.size).sum::<u64>() + size > MAX_STORE_BYTES {
            return Err("Too many pictures are waiting. Remove some first.".into());
        }
    }
    if fs4::available_space(dir).map(|free| free < size + DISK_HEADROOM).unwrap_or(true) {
        return Err("Not enough disk space on the PC.".into());
    }
    let tmp = dir.join(format!(".{}.part", token(8)));
    let copied = (|| -> std::io::Result<u64> {
        let f = std::fs::File::open(src)?;
        let mut out = std::fs::OpenOptions::new().write(true).create_new(true).open(&tmp)?;
        // Read one byte past the limit, so a file that grew after the size check is still refused.
        std::io::copy(&mut std::io::Read::take(f, MAX_CAPTURE_BYTES + 1), &mut out)
    })();
    let n = match copied {
        Ok(n) if n > 0 && n <= MAX_CAPTURE_BYTES => n,
        Ok(_) => {
            let _ = std::fs::remove_file(&tmp);
            return Err("Pictures can be 40 MB at most.".into());
        }
        Err(_) => {
            let _ = std::fs::remove_file(&tmp);
            return Err("Swiftlane couldn't read this file.".into());
        }
    };
    register(&tmp, n).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

fn now() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

// ------------------------------------------------------------------ pictures for the page

/// The picture cut down so its longest side is at most `max` (never enlarged).
fn shrink(img: &RgbaImage, max: u32) -> RgbaImage {
    let (w, h) = img.dimensions();
    if w.max(h) <= max {
        return img.clone();
    }
    let s = max as f64 / w.max(h) as f64;
    imageops::resize(img, ((w as f64 * s).round() as u32).max(1), ((h as f64 * s).round() as u32).max(1), FilterType::Triangle)
}

fn data_url(img: &RgbaImage, quality: u8) -> Result<String, String> {
    let jpg = images::encode(img, false, Fmt::Jpeg, quality, None, None)?;
    Ok(format!("data:image/jpeg;base64,{}", base64::engine::general_purpose::STANDARD.encode(jpg)))
}

/// The photo for placing corners on, with corners that start just inside its edges.
fn open(id: &str) -> Value {
    let Some(path) = path_of(id) else { return json!({ "error": "That photo isn't there anymore." }) };
    let img = match load_upright(&path) {
        Ok(i) => i,
        Err(e) => return json!({ "error": e }),
    };
    match data_url(&shrink(&img, WORK_SIDE), 85) {
        Ok(image) => json!({ "id": id, "image": image, "w": img.width(), "h": img.height(),
                             "corners": [[0.04, 0.04], [0.96, 0.04], [0.96, 0.96], [0.04, 0.96]] }),
        Err(e) => json!({ "error": e }),
    }
}

// ------------------------------------------------------------------ the flattening

type P = (f64, f64);

/// The 3x3 matrix (row by row, last entry 1) taking each `from` corner to the matching `to` corner.
fn homography(from: [P; 4], to: [P; 4]) -> Option<[f64; 9]> {
    let mut m = [[0f64; 9]; 8];
    for i in 0..4 {
        let ((x, y), (u, v)) = (from[i], to[i]);
        m[2 * i] = [x, y, 1.0, 0.0, 0.0, 0.0, -u * x, -u * y, u];
        m[2 * i + 1] = [0.0, 0.0, 0.0, x, y, 1.0, -v * x, -v * y, v];
    }
    for col in 0..8 {
        let pivot = (col..8).max_by(|&a, &b| m[a][col].abs().total_cmp(&m[b][col].abs()))?;
        if m[pivot][col].abs() < 1e-12 {
            return None;
        }
        m.swap(col, pivot);
        for r in 0..8 {
            if r != col {
                let f = m[r][col] / m[col][col];
                for c in col..9 {
                    m[r][c] -= f * m[col][c];
                }
            }
        }
    }
    let mut h = [0f64; 9];
    for i in 0..8 {
        h[i] = m[i][8] / m[i][i];
    }
    h[8] = 1.0;
    h.iter().all(|v| v.is_finite()).then_some(h)
}

fn apply(h: &[f64; 9], x: f64, y: f64) -> P {
    let w = h[6] * x + h[7] * y + h[8];
    ((h[0] * x + h[1] * y + h[2]) / w, (h[3] * x + h[4] * y + h[5]) / w)
}

fn dist(a: P, b: P) -> f64 {
    ((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2)).sqrt()
}

/// True if the four corners (top left, top right, bottom right, bottom left) make a proper
/// four-sided shape: not crossing over itself, no side nearly zero.
fn valid_quad(q: &[P; 4]) -> bool {
    let mut sign = 0f64;
    for i in 0..4 {
        let (a, b, c) = (q[i], q[(i + 1) % 4], q[(i + 2) % 4]);
        if dist(a, b) < 8.0 {
            return false;
        }
        let cross = (b.0 - a.0) * (c.1 - b.1) - (b.1 - a.1) * (c.0 - b.0);
        if cross.abs() < 1e-6 || (sign != 0.0 && cross.signum() != sign) {
            return false;
        }
        sign = cross.signum();
    }
    true
}

/// The flattened page's size: from the longer of each pair of opposite edges, then scaled down
/// to fit `cap` pixels on each side (and the overall pixel limit).
fn output_size(q: &[P; 4], cap: u32) -> (u32, u32) {
    let w = dist(q[0], q[1]).max(dist(q[3], q[2]));
    let h = dist(q[0], q[3]).max(dist(q[1], q[2]));
    let cap = cap.clamp(1, MAX_SIDE) as f64;
    let mut s = (cap / w.max(h)).min(1.0);
    let pixels = (w * s) * (h * s);
    if pixels > 16_000_000.0 {
        s *= (16_000_000.0 / pixels).sqrt();
    }
    (((w * s).round() as u32).max(1), ((h * s).round() as u32).max(1))
}

fn sample(img: &RgbaImage, x: f64, y: f64) -> [u8; 4] {
    let (w, h) = (img.width() as i64, img.height() as i64);
    let (fx, fy) = (x - 0.5, y - 0.5);
    let (x0, y0) = (fx.floor(), fy.floor());
    let (tx, ty) = (fx - x0, fy - y0);
    let at = |xi: i64, yi: i64| img.get_pixel(xi.clamp(0, w - 1) as u32, yi.clamp(0, h - 1) as u32).0;
    let (x0, y0) = (x0 as i64, y0 as i64);
    let (p00, p10, p01, p11) = (at(x0, y0), at(x0 + 1, y0), at(x0, y0 + 1), at(x0 + 1, y0 + 1));
    let mut out = [0u8; 4];
    for c in 0..4 {
        let top = p00[c] as f64 * (1.0 - tx) + p10[c] as f64 * tx;
        let bottom = p01[c] as f64 * (1.0 - tx) + p11[c] as f64 * tx;
        out[c] = (top * (1.0 - ty) + bottom * ty).round().clamp(0.0, 255.0) as u8;
    }
    out
}

/// The part of `src` inside `quad` (pixel coordinates), flattened to `out_w` x `out_h` with
/// bilinear sampling.
fn warp(src: &RgbaImage, quad: &[P; 4], out_w: u32, out_h: u32) -> Option<RgbaImage> {
    let (ow, oh) = (out_w as f64, out_h as f64);
    let h = homography([(0.0, 0.0), (ow, 0.0), (ow, oh), (0.0, oh)], *quad)?;
    let mut out = RgbaImage::new(out_w, out_h);
    for y in 0..out_h {
        for x in 0..out_w {
            let (u, v) = apply(&h, x as f64 + 0.5, y as f64 + 0.5);
            out.put_pixel(x, y, image::Rgba(sample(src, u, v)));
        }
    }
    Some(out)
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Look {
    Colour,
    Grey,
    BlackWhite,
}

fn luma(p: &image::Rgba<u8>) -> f64 {
    0.299 * p[0] as f64 + 0.587 * p[1] as f64 + 0.114 * p[2] as f64
}

/// Grey: paper white and ink black stretched to the full range. Black and white: each pixel
/// against the average of the pixels around it, so shadows and uneven light don't turn grey
/// paper black.
fn apply_look(img: &mut RgbaImage, look: Look) {
    if look == Look::Colour {
        return;
    }
    let (w, h) = (img.width() as usize, img.height() as usize);
    let grey: Vec<f64> = img.pixels().map(luma).collect();
    let out: Vec<u8> = match look {
        Look::Grey => {
            let mut hist = [0usize; 256];
            for g in &grey {
                hist[g.round().clamp(0.0, 255.0) as usize] += 1;
            }
            let total = grey.len();
            let (mut lo, mut hi, mut acc) = (0usize, 255usize, 0usize);
            for (i, n) in hist.iter().enumerate() {
                acc += n;
                if acc * 100 >= total {
                    lo = i;
                    break;
                }
            }
            acc = 0;
            for i in (0..256).rev() {
                acc += hist[i];
                if acc * 100 >= total * 2 {
                    hi = i;
                    break;
                }
            }
            let span = (hi.max(lo + 1) - lo) as f64;
            grey.iter().map(|g| ((g - lo as f64) * 255.0 / span).round().clamp(0.0, 255.0) as u8).collect()
        }
        _ => {
            let stride = w + 1;
            let mut integral = vec![0f64; stride * (h + 1)];
            for y in 0..h {
                let mut row = 0.0;
                for x in 0..w {
                    row += grey[y * w + x];
                    integral[(y + 1) * stride + x + 1] = integral[y * stride + x + 1] + row;
                }
            }
            let r = (w.min(h) / 16).max(8);
            let mut out = Vec::with_capacity(w * h);
            for y in 0..h {
                let (y0, y1) = (y.saturating_sub(r), (y + r + 1).min(h));
                for x in 0..w {
                    let (x0, x1) = (x.saturating_sub(r), (x + r + 1).min(w));
                    let sum = integral[y1 * stride + x1] - integral[y0 * stride + x1] - integral[y1 * stride + x0] + integral[y0 * stride + x0];
                    let mean = sum / ((x1 - x0) * (y1 - y0)) as f64;
                    out.push(if grey[y * w + x] >= mean * 0.9 { 255 } else { 0 });
                }
            }
            out
        }
    };
    for (p, g) in img.pixels_mut().zip(out) {
        *p = image::Rgba([g, g, g, 255]);
    }
}

/// One scanned page as asked for by the page: which photo, the four corners as fractions of the
/// upright photo (top left, top right, bottom right, bottom left), a turn and a look.
struct Spec {
    id: String,
    corners: [P; 4],
    rotate: i64,
    look: Look,
}

fn parse_spec(v: &Value) -> Result<Spec, String> {
    let bad = || "A page couldn't be read. Open the photo again.".to_string();
    let id = v["id"].as_str().ok_or_else(bad)?.to_string();
    let cs = v["corners"].as_array().filter(|a| a.len() == 4).ok_or_else(bad)?;
    let mut corners = [(0.0, 0.0); 4];
    for (slot, c) in corners.iter_mut().zip(cs) {
        let (x, y) = (c[0].as_f64(), c[1].as_f64());
        match (x, y) {
            (Some(x), Some(y)) if x.is_finite() && y.is_finite() => *slot = (x.clamp(0.0, 1.0), y.clamp(0.0, 1.0)),
            _ => return Err(bad()),
        }
    }
    let rotate = (v["rotate"].as_f64().filter(|r| r.is_finite()).unwrap_or(0.0) as i64).rem_euclid(360) / 90 * 90;
    let look = match v["look"].as_str() {
        Some("grey") => Look::Grey,
        Some("bw") => Look::BlackWhite,
        _ => Look::Colour,
    };
    Ok(Spec { id, corners, rotate, look })
}

fn parse_specs(v: &Value) -> Result<Vec<Spec>, String> {
    let a = v.as_array().ok_or("There are no pages to save.")?;
    if a.is_empty() {
        return Err("There are no pages to save.".into());
    }
    if a.len() > MAX_PAGES {
        return Err("That's too many pages for one save.".into());
    }
    a.iter().map(parse_spec).collect()
}

/// The flattened page for a picture already loaded, at most `cap` pixels on each side.
fn flatten(src: &RgbaImage, corners: &[P; 4], rotate: i64, look: Look, cap: u32) -> Result<RgbaImage, String> {
    let (w, h) = (src.width() as f64, src.height() as f64);
    let quad: [P; 4] = corners.map(|(x, y)| (x * w, y * h));
    if !valid_quad(&quad) {
        return Err("Move the corners so they make a four-sided shape that doesn't cross over itself.".into());
    }
    let (ow, oh) = output_size(&quad, cap);
    // Much bigger than the result: shrink first, so thin lines don't break up.
    let factor = (dist(quad[0], quad[1]).max(dist(quad[3], quad[2])) / ow as f64).min(dist(quad[0], quad[3]).max(dist(quad[1], quad[2])) / oh as f64);
    let reduced;
    let (src, quad) = if factor >= 2.0 {
        reduced = imageops::resize(src, ((w / factor).round() as u32).max(2), ((h / factor).round() as u32).max(2), FilterType::Triangle);
        let (sx, sy) = (reduced.width() as f64 / w, reduced.height() as f64 / h);
        (&reduced, quad.map(|(x, y)| (x * sx, y * sy)))
    } else {
        (src, quad)
    };
    let mut page = warp(src, &quad, ow, oh).ok_or("Move the corners so they make a four-sided shape that doesn't cross over itself.")?;
    apply_look(&mut page, look);
    Ok(match rotate {
        90 => imageops::rotate90(&page),
        180 => imageops::rotate180(&page),
        270 => imageops::rotate270(&page),
        _ => page,
    })
}

fn render(spec: &Spec, cap: u32) -> Result<RgbaImage, String> {
    let path = path_of(&spec.id).ok_or("That photo isn't there anymore.")?;
    let img = load_upright(&path)?;
    flatten(&img, &spec.corners, spec.rotate, spec.look, cap)
}

fn preview(v: &Value) -> Value {
    let result = parse_spec(v).and_then(|s| render(&s, PREVIEW_SIDE)).and_then(|img| data_url(&img, 80).map(|u| (u, img.width(), img.height())));
    match result {
        Ok((image, w, h)) => json!({ "image": image, "w": w, "h": h }),
        Err(e) => json!({ "error": e }),
    }
}

// ------------------------------------------------------------------ saving

fn io_message(e: &std::io::Error) -> String {
    match e.kind() {
        std::io::ErrorKind::PermissionDenied => "Swiftlane isn't allowed to save in that folder. Pick another one.".into(),
        std::io::ErrorKind::StorageFull => "The disk is full, so the picture couldn't be saved.".into(),
        _ => format!("The picture couldn't be saved ({e})."),
    }
}

fn stamp() -> String {
    chrono::Local::now().format("%Y-%m-%d %H-%M-%S").to_string()
}

/// folder/name, or "name (1).ext" if taken; the name is claimed first so nothing is overwritten.
fn save_new(folder: &Path, name: &str, bytes: &[u8]) -> Result<PathBuf, String> {
    use std::io::Write;
    std::fs::create_dir_all(folder).map_err(|e| io_message(&e))?;
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    for n in 0..10_000 {
        let candidate = folder.join(if n == 0 { name.to_string() } else { format!("{stem} ({n}){ext}") });
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&candidate) {
            Ok(mut f) => {
                if let Err(e) = f.write_all(bytes).and_then(|_| f.sync_all()) {
                    drop(f);
                    let _ = std::fs::remove_file(&candidate);
                    return Err(io_message(&e));
                }
                crate::transfer::record_created(&candidate);
                return Ok(candidate);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(io_message(&e)),
        }
    }
    Err("There's no free file name left in that folder.".into())
}

/// Writes exactly `path` (picked in the Save box, which already asked before replacing), written
/// next to it first so the old file survives a failure.
fn save_exact(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let folder = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let tmp = folder.join(format!(".{name}.swiftlane-part"));
    let r = std::fs::write(&tmp, bytes).and_then(|_| std::fs::rename(&tmp, path));
    if let Err(e) = r {
        let _ = std::fs::remove_file(&tmp);
        return Err(io_message(&e));
    }
    crate::transfer::record_created(path);
    Ok(())
}

fn save_folder(app: &AppHandle) -> (PathBuf, bool) {
    let st = app.state::<crate::api::AppState>();
    let cfg = st.cfg.lock().unwrap();
    (PathBuf::from(cfg["save_dir"].as_str().unwrap_or("")), cfg["sort_files"].as_bool().unwrap_or(true))
}

/// The photo as a JPG in the normal save folder (Photos, if files are sorted into folders).
fn use_photo(app: &AppHandle, id: &str) -> Value {
    let Some(path) = path_of(id) else { return json!({ "error": "That photo isn't there anymore." }) };
    let (dir, sort) = save_folder(app);
    let folder = if sort { dir.join("Photos") } else { dir };
    let name = format!("Camera {}.jpg", stamp());
    let bytes = match std::fs::read(&path) {
        // A JPG is kept exactly as it arrived, like every other file from the phone.
        Ok(b) if b.starts_with(b"\xff\xd8\xff") => Ok(b),
        // Other kinds become a JPG without camera data.
        _ => load_upright(&path).and_then(|img| images::encode(&img, false, Fmt::Jpeg, 92, None, None)),
    };
    match bytes.and_then(|b| save_new(&folder, &name, &b)) {
        Ok(p) => json!({ "saved": p.file_name().map(|n| n.to_string_lossy().to_string()), "folder": p.parent().map(|f| f.to_string_lossy().to_string()) }),
        Err(e) => json!({ "error": e }),
    }
}

fn jpeg(img: &RgbaImage) -> Result<Vec<u8>, String> {
    images::encode(img, false, Fmt::Jpeg, 88, None, None) // no metadata: scans are plain pictures
}

fn save_jpg(app: &AppHandle, specs: &Value) -> Value {
    let specs = match parse_specs(specs) {
        Ok(s) => s,
        Err(e) => return json!({ "error": e }),
    };
    let (dir, _) = save_folder(app);
    let stamp = stamp();
    if specs.len() == 1 {
        let Some(out) = ask_save(app, &dir, &format!("Scan {stamp}.jpg"), &["jpg"]) else { return Value::Null };
        return match render(&specs[0], MAX_SIDE).and_then(|i| jpeg(&i)).and_then(|b| save_exact(&out, &b)) {
            Ok(()) => json!({ "saved": 1, "folder": out.parent().map(|f| f.to_string_lossy().to_string()) }),
            Err(e) => json!({ "error": e }),
        };
    }
    let Some(folder) = ask_folder(app, &dir) else { return Value::Null };
    for (i, s) in specs.iter().enumerate() {
        if let Err(e) = render(s, MAX_SIDE).and_then(|img| jpeg(&img)).and_then(|b| save_new(&folder, &format!("Scan {stamp} page {}.jpg", i + 1), &b)) {
            return json!({ "error": e, "saved": i });
        }
    }
    json!({ "saved": specs.len(), "folder": folder.to_string_lossy() })
}

fn job_message(e: JobError) -> String {
    match e {
        JobError::Failed(m) => m,
        JobError::Cancelled => "Cancelled.".into(),
    }
}

/// One new PDF with the scanned pages in the order given. "append": after the pages of an existing
/// PDF you pick. The existing PDF is never changed; the result is a new file from the Save box.
fn save_pdf(app: &AppHandle, specs: &Value, mode: &str) -> Value {
    let specs = match parse_specs(specs) {
        Ok(s) => s,
        Err(e) => return json!({ "error": e }),
    };
    let (dir, _) = save_folder(app);
    let existing = if mode == "append" {
        let picked = app.dialog().file().set_directory(&dir).add_filter("PDF files", &["pdf"]).blocking_pick_file().and_then(|p| p.into_path().ok());
        match picked {
            Some(p) => Some(p),
            None => return Value::Null,
        }
    } else {
        None
    };
    let (start, name) = match &existing {
        Some(p) => (p.parent().map(Path::to_path_buf).unwrap_or(dir.clone()), format!("{} (with scans).pdf", p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default())),
        None => (dir.clone(), format!("Scan {}.pdf", stamp())),
    };
    let Some(out) = ask_save(app, &start, &name, &["pdf"]) else { return Value::Null };
    let result = (|| -> Result<(), String> {
        let mut pages = Vec::new();
        for s in &specs {
            let img = render(s, MAX_SIDE)?;
            pages.push((jpeg(&img)?, img.width(), img.height()));
        }
        match &existing {
            None => super::pdf::pdf_from_jpegs(&pages, &out).map_err(job_message),
            Some(old) => {
                let tmp_dir = private_dir().ok_or("Swiftlane couldn't make its temporary folder.")?;
                let scans = tmp_dir.join(format!(".scan-{}.pdf", token(6)));
                let r = super::pdf::pdf_from_jpegs(&pages, &scans)
                    .and_then(|()| super::pdf::pdf_append(old, &scans, &out, &|| Ok(())))
                    .map_err(job_message);
                let _ = std::fs::remove_file(&scans);
                r
            }
        }
    })();
    match result {
        Ok(()) => json!({ "saved": specs.len(), "file": out.file_name().map(|n| n.to_string_lossy().to_string()), "folder": out.parent().map(|f| f.to_string_lossy().to_string()) }),
        Err(e) => json!({ "error": e }),
    }
}

// ------------------------------------------------------------------ tests

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: P, b: P) -> bool {
        (a.0 - b.0).abs() < 1e-6 && (a.1 - b.1).abs() < 1e-6
    }

    #[test]
    fn square_maps_onto_its_quad() {
        let square = [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)];
        // a plain scaled and moved square
        let moved = [(10.0, 10.0), (30.0, 10.0), (30.0, 30.0), (10.0, 30.0)];
        let h = homography(square, moved).unwrap();
        for (a, b) in square.iter().zip(moved) {
            assert!(close(apply(&h, a.0, a.1), b));
        }
        assert!(close(apply(&h, 0.5, 0.5), (20.0, 20.0)));
        // a leaning, narrowing quad (a page photographed at an angle)
        let quad = [(12.0, 8.0), (88.0, 20.0), (70.0, 95.0), (5.0, 60.0)];
        let h = homography(square, quad).unwrap();
        for (a, b) in square.iter().zip(quad) {
            assert!(close(apply(&h, a.0, a.1), b), "{a:?}");
        }
        // the middle of the square lands where the quad's diagonals cross
        let (p, q, r, s) = (quad[0], quad[2], quad[1], quad[3]);
        let d = (p.0 - q.0) * (r.1 - s.1) - (p.1 - q.1) * (r.0 - s.0);
        let (a, b) = (p.0 * q.1 - p.1 * q.0, r.0 * s.1 - r.1 * s.0);
        let cross = ((a * (r.0 - s.0) - (p.0 - q.0) * b) / d, (a * (r.1 - s.1) - (p.1 - q.1) * b) / d);
        assert!(close(apply(&h, 0.5, 0.5), cross), "{:?} vs {cross:?}", apply(&h, 0.5, 0.5));
        // three corners in a line can't be flattened
        assert!(homography(square, [(0.0, 0.0), (1.0, 1.0), (2.0, 2.0), (3.0, 3.0)]).is_none());
    }

    #[test]
    fn output_size_follows_the_edges_and_is_capped() {
        let q = |w: f64, h: f64| [(0.0, 0.0), (w, 0.0), (w, h), (0.0, h)];
        assert_eq!(output_size(&q(1000.0, 2000.0), 4000), (1000, 2000));
        assert_eq!(output_size(&q(8000.0, 4000.0), 4000), (4000, 2000));
        assert_eq!(output_size(&q(3000.0, 6000.0), 1200), (600, 1200));
        // the longer of two opposite edges wins
        let trap = [(100.0, 0.0), (400.0, 0.0), (500.0, 300.0), (0.0, 300.0)];
        assert_eq!(output_size(&trap, 4000), (500, 316)); // slanted sides are longer than the height
        // never more than 16 million pixels
        let (w, h) = output_size(&q(4000.0, 4000.0), 4000);
        assert!(w as u64 * h as u64 <= 16_000_000 + 8000);
        assert!(valid_quad(&q(100.0, 100.0)));
        assert!(!valid_quad(&[(0.0, 0.0), (100.0, 100.0), (100.0, 0.0), (0.0, 100.0)]), "bow-tie");
    }

    #[test]
    fn warp_flattens_a_slanted_page() {
        // A white page with a red top-left corner mark, drawn slanted on a grey photo.
        let mut photo = RgbaImage::from_pixel(200, 200, image::Rgba([90, 90, 90, 255]));
        let quad: [P; 4] = [(40.0, 30.0), (160.0, 50.0), (150.0, 170.0), (30.0, 140.0)];
        let h = homography([(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)], quad).unwrap();
        for j in 0..400 {
            for i in 0..400 {
                let (u, v) = (i as f64 / 400.0, j as f64 / 400.0);
                let (x, y) = apply(&h, u, v);
                let red = u < 0.2 && v < 0.2;
                photo.put_pixel(x as u32, y as u32, if red { image::Rgba([220, 0, 0, 255]) } else { image::Rgba([250, 250, 250, 255]) });
            }
        }
        let out = warp(&photo, &quad, 100, 100).unwrap();
        assert_eq!(out.dimensions(), (100, 100));
        assert!(out.get_pixel(8, 8)[0] > 180 && out.get_pixel(8, 8)[1] < 80, "red mark top left");
        assert!(out.get_pixel(80, 80)[1] > 200, "page is white");
        assert!(out.get_pixel(80, 8)[1] > 200 && out.get_pixel(8, 80)[1] > 200);
        // flatten() does the same through the corner fractions, and turns it
        let fr: [P; 4] = quad.map(|(x, y)| (x / 200.0, y / 200.0));
        let flat = flatten(&photo, &fr, 0, Look::Colour, 4000).unwrap();
        assert!(flat.width() > 100 && flat.height() > 100);
        let turned = flatten(&photo, &fr, 90, Look::Colour, 4000).unwrap();
        assert_eq!((turned.width(), turned.height()), (flat.height(), flat.width()));
        let bw = flatten(&photo, &fr, 0, Look::BlackWhite, 4000).unwrap();
        assert!(bw.pixels().all(|p| p[0] == 0 || p[0] == 255));
        assert!(flatten(&photo, &[(0.1, 0.1), (0.9, 0.9), (0.9, 0.1), (0.1, 0.9)], 0, Look::Colour, 4000).is_err());
    }

    #[test]
    fn pictures_are_recognised_by_their_bytes() {
        assert_eq!(sniff(b"\xff\xd8\xff\xe0aaaaaaaaaaaa"), Some("jpg"));
        assert_eq!(sniff(b"\x89PNG\r\n\x1a\naaaaaaaa"), Some("png"));
        assert_eq!(sniff(b"MZ\x90\0\x03\0\0\0\x04\0\0\0\xff\xff\0\0"), None);
        assert_eq!(sniff(b"%PDF-1.7 aaaaaaaaaa"), None);
    }

    #[test]
    fn page_specs_are_checked() {
        let ok = json!({ "id": "ab", "corners": [[0, 0], [1, 0], [1, 1], [0, 1]], "rotate": -90, "look": "bw" });
        let s = parse_spec(&ok).unwrap();
        assert_eq!((s.rotate, s.look), (270, Look::BlackWhite));
        assert!(parse_spec(&json!({ "id": "ab", "corners": [[0, 0]] })).is_err());
        assert!(parse_spec(&json!({ "corners": [[0, 0], [1, 0], [1, 1], [0, 1]] })).is_err());
        assert!(parse_specs(&json!([])).is_err());
    }
}
