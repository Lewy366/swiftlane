//! QR Studio: 100% offline Wi-Fi, link, contact, email and text QR code generator + offline
//! screenshot and image QR decoder. Zero network calls ever.

use base64::Engine;
use image::{DynamicImage, GrayImage, ImageEncoder, Rgb, RgbImage, RgbaImage};
use qrcode::{Color, EcLevel, QrCode};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::fmt::Write as FmtWrite;
use std::path::{Path, PathBuf};
use tauri::AppHandle;
use tauri_plugin_dialog::DialogExt;

use super::{arg, ask_save};

const QUIET: usize = 4;
const MAX_PAYLOAD_BYTES: usize = 2953;
const MAX_IMAGE_BYTES: usize = 32 * 1024 * 1024;
const MAX_DATA_BYTES: usize = 24 * 1024 * 1024;
const MAX_RGBA_BYTES: usize = 128 * 1024 * 1024;
const MAX_DECODE_DIM: u32 = 2400;
const DEFAULT_FG: &str = "#0e121c";
const DEFAULT_BG: &str = "#ffffff";
#[cfg_attr(not(windows), allow(dead_code))]
const CLIPBOARD_EMPTY_ERR: &str = "No image in your clipboard. Take a screenshot (Win+Shift+S) or copy an image first.";

pub fn dispatch(app: &AppHandle, method: &str, args: &[Value]) -> Option<Value> {
    Some(match method {
        "qr_generate" => {
            let payload = arg(args, 0).as_str().unwrap_or("");
            let fg = arg(args, 1).as_str();
            let bg = arg(args, 2).as_str();
            let ec = arg(args, 3).as_str();
            match render_qr_svg(payload, fg, bg, ec) {
                Ok((svg, modules)) => json!({ "svg": svg, "modules": modules, "payload": payload }),
                Err(e) => json!({ "error": e }),
            }
        }
        "qr_wifi_payload" => {
            let ssid = arg(args, 0).as_str().unwrap_or("");
            let password = arg(args, 1).as_str().unwrap_or("");
            let security = arg(args, 2).as_str().unwrap_or("WPA");
            let hidden = arg(args, 3).as_bool().unwrap_or(false);
            json!({ "payload": build_wifi_payload(ssid, password, security, hidden) })
        }
        "qr_save_png" => {
            let payload = arg(args, 0).as_str().unwrap_or("");
            let fg = arg(args, 1).as_str();
            let bg = arg(args, 2).as_str();
            let ec = arg(args, 3).as_str();
            let size = arg(args, 4).as_u64().unwrap_or(1024).min(u32::MAX as u64) as u32;
            save_png(app, payload, fg, bg, ec, size)
        }
        "qr_save_svg" => {
            let payload = arg(args, 0).as_str().unwrap_or("");
            let fg = arg(args, 1).as_str();
            let bg = arg(args, 2).as_str();
            let ec = arg(args, 3).as_str();
            save_svg(app, payload, fg, bg, ec)
        }
        "qr_copy_png" => {
            let payload = arg(args, 0).as_str().unwrap_or("");
            let fg = arg(args, 1).as_str();
            let bg = arg(args, 2).as_str();
            let ec = arg(args, 3).as_str();
            match copy_png(payload, fg, bg, ec) {
                Ok(()) => json!({ "ok": true }),
                Err(e) => json!({ "error": e }),
            }
        }
        "qr_decode_file" => {
            let path = arg(args, 0).as_str().unwrap_or("");
            decode_file(Path::new(path))
        }
        "qr_decode_pick" => decode_pick(app),
        "qr_decode_data" => {
            let data = arg(args, 0).as_str().unwrap_or("");
            decode_data_url(data)
        }
        "qr_decode_clipboard" => decode_clipboard(),
        "qr_open_url" => {
            let url = arg(args, 0).as_str().unwrap_or("");
            match open_http_url(url) {
                Ok(()) => json!({ "ok": true }),
                Err(e) => json!({ "error": e }),
            }
        }
        _ => return None,
    })
}

// ------------------------------------------------------------------ Wi-Fi & content parsing

fn escape_wifi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        if matches!(ch, '\\' | ';' | ',' | ':' | '"') {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

/// Builds a standard ZXing/Android/iOS `WIFI:` QR payload with proper backslash escaping.
pub fn build_wifi_payload(ssid: &str, password: &str, security: &str, hidden: bool) -> String {
    let sec = match security.trim().to_ascii_uppercase().as_str() {
        "WEP" => "WEP",
        "NOPASS" | "NONE" | "OPEN" | "" => "nopass",
        _ => "WPA",
    };
    let s = escape_wifi(ssid);
    let h = if hidden { "true" } else { "false" };
    if sec == "nopass" {
        format!("WIFI:T:nopass;S:{s};H:{h};;")
    } else {
        let p = escape_wifi(password);
        format!("WIFI:T:{sec};S:{s};P:{p};H:{h};;")
    }
}

fn parse_wifi_fields(rest: &str) -> Value {
    let mut ssid = String::new();
    let mut password = String::new();
    let mut security = String::new();
    let mut hidden = false;

    let mut chars = rest.chars().peekable();
    while chars.peek().is_some() {
        // Read key up to unescaped ':' or ';'
        let mut key = String::new();
        let mut hit_colon = false;
        for ch in chars.by_ref() {
            if ch == ':' {
                hit_colon = true;
                break;
            }
            if ch == ';' {
                break;
            }
            key.push(ch);
        }
        if !hit_colon {
            continue;
        }
        // Read value up to unescaped ';'
        let mut val = String::new();
        while let Some(ch) = chars.next() {
            if ch == ';' {
                break;
            }
            if ch == '\\' {
                if let Some(next) = chars.next() {
                    if matches!(next, '\\' | ';' | ',' | ':' | '"') {
                        val.push(next);
                    } else {
                        val.push('\\');
                        val.push(next);
                    }
                } else {
                    val.push('\\');
                }
            } else {
                val.push(ch);
            }
        }
        match key.trim().to_ascii_uppercase().as_str() {
            "S" => ssid = val,
            "P" => password = val,
            "T" => security = val,
            "H" => hidden = val.trim().eq_ignore_ascii_case("true"),
            _ => {}
        }
    }

    if security.is_empty() {
        security = if password.is_empty() { "nopass".into() } else { "WPA".into() };
    }

    json!({
        "ssid": ssid,
        "password": password,
        "security": security,
        "hidden": hidden,
    })
}

/// Classifies a decoded QR string into `wifi`, `url`, `email`, `vcard`, or `text`.
pub fn parse_decoded_content(text: &str) -> Value {
    let trimmed = text.trim_start();
    if trimmed.len() >= 5 && trimmed[..5].eq_ignore_ascii_case("WIFI:") {
        return json!({
            "kind": "wifi",
            "text": text,
            "wifi": parse_wifi_fields(&trimmed[5..]),
        });
    }
    if (trimmed.len() >= 7 && trimmed[..7].eq_ignore_ascii_case("http://"))
        || (trimmed.len() >= 8 && trimmed[..8].eq_ignore_ascii_case("https://"))
    {
        return json!({ "kind": "url", "text": text });
    }
    if trimmed.len() >= 7 && trimmed[..7].eq_ignore_ascii_case("mailto:") {
        return json!({ "kind": "email", "text": text });
    }
    if trimmed.len() >= 11 && trimmed[..11].eq_ignore_ascii_case("BEGIN:VCARD") {
        return json!({ "kind": "vcard", "text": text });
    }
    json!({ "kind": "text", "text": text })
}

// ------------------------------------------------------------------ QR rendering

/// Validates a `#RRGGBB` color string strictly so SVG output can never be injected.
pub fn sanitize_hex_color(raw: Option<&str>, fallback: &'static str) -> String {
    if let Some(s) = raw.map(str::trim) {
        if s.len() == 7 && s.starts_with('#') && s.as_bytes()[1..].iter().all(u8::is_ascii_hexdigit) {
            return s.to_ascii_lowercase();
        }
    }
    fallback.to_string()
}

fn parse_hex_rgb(hex: &str) -> [u8; 3] {
    let r = u8::from_str_radix(&hex[1..3], 16).unwrap_or(0);
    let g = u8::from_str_radix(&hex[3..5], 16).unwrap_or(0);
    let b = u8::from_str_radix(&hex[5..7], 16).unwrap_or(0);
    [r, g, b]
}

fn parse_ec(ec: Option<&str>) -> EcLevel {
    match ec.unwrap_or("M").trim().to_ascii_uppercase().as_str() {
        "L" => EcLevel::L,
        "Q" => EcLevel::Q,
        "H" => EcLevel::H,
        _ => EcLevel::M,
    }
}

fn encode_qr(payload: &str, ec: Option<&str>) -> Result<QrCode, String> {
    if payload.is_empty() {
        return Err("Enter some text or details to generate a QR code.".into());
    }
    if payload.len() > MAX_PAYLOAD_BYTES {
        return Err(format!(
            "Content is too long for a QR code ({} bytes; maximum is {MAX_PAYLOAD_BYTES} bytes).",
            payload.len()
        ));
    }
    QrCode::with_error_correction_level(payload.as_bytes(), parse_ec(ec)).map_err(|_| {
        "Content is too long for this error correction level. Try a lower error correction level or shorter text.".into()
    })
}

/// Renders a crisp, injection-proof SVG with a 4-module quiet zone.
pub fn render_qr_svg(
    payload: &str,
    fg: Option<&str>,
    bg: Option<&str>,
    ec: Option<&str>,
) -> Result<(String, usize), String> {
    let code = encode_qr(payload, ec)?;
    let fg_hex = sanitize_hex_color(fg, DEFAULT_FG);
    let bg_hex = sanitize_hex_color(bg, DEFAULT_BG);
    let n = code.width();
    let total = n + QUIET * 2;
    let colors = code.to_colors();

    let mut path = String::with_capacity(n * n * 6);
    for y in 0..n {
        let py = y + QUIET;
        for x in 0..n {
            if colors[y * n + x] == Color::Dark {
                let px = x + QUIET;
                let _ = write!(path, "M{px} {py}h1v1h-1z");
            }
        }
    }

    let svg = format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {total} {total}" shape-rendering="crispEdges"><rect width="{total}" height="{total}" fill="{bg_hex}"/><path fill="{fg_hex}" d="{path}"/></svg>"#
    );
    Ok((svg, n))
}

fn render_qr_rgb(
    payload: &str,
    fg: Option<&str>,
    bg: Option<&str>,
    ec: Option<&str>,
    size: u32,
) -> Result<RgbImage, String> {
    let code = encode_qr(payload, ec)?;
    let fg_rgb = parse_hex_rgb(&sanitize_hex_color(fg, DEFAULT_FG));
    let bg_rgb = parse_hex_rgb(&sanitize_hex_color(bg, DEFAULT_BG));
    let n = code.width();
    let total = n + QUIET * 2;
    let target = size.clamp(256, 2048) as usize;
    let scale = (target / total).max(1);
    let img_dim = (total * scale) as u32;

    let mut img = RgbImage::from_pixel(img_dim, img_dim, Rgb(bg_rgb));
    let colors = code.to_colors();
    let dark = Rgb(fg_rgb);

    for y in 0..n {
        let y0 = ((y + QUIET) * scale) as u32;
        for x in 0..n {
            if colors[y * n + x] == Color::Dark {
                let x0 = ((x + QUIET) * scale) as u32;
                for dy in 0..scale as u32 {
                    for dx in 0..scale as u32 {
                        img.put_pixel(x0 + dx, y0 + dy, dark);
                    }
                }
            }
        }
    }
    Ok(img)
}

fn encode_rgb_png(rgb: &RgbImage) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    image::codecs::png::PngEncoder::new(&mut out)
        .write_image(rgb.as_raw(), rgb.width(), rgb.height(), image::ExtendedColorType::Rgb8)
        .map_err(|e| format!("Couldn't encode PNG ({e})."))?;
    Ok(out)
}

/// Renders a crisp integer-scaled PNG with a 4-module quiet zone.
pub fn render_qr_png(
    payload: &str,
    fg: Option<&str>,
    bg: Option<&str>,
    ec: Option<&str>,
    size: u32,
) -> Result<Vec<u8>, String> {
    let rgb = render_qr_rgb(payload, fg, bg, ec, size)?;
    encode_rgb_png(&rgb)
}

fn default_save_dir(app: &AppHandle) -> PathBuf {
    let configured = super::save_dir(app);
    if !configured.as_os_str().is_empty() && configured.is_dir() {
        return configured;
    }
    dirs::picture_dir()
        .or_else(dirs::download_dir)
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn save_png(
    app: &AppHandle,
    payload: &str,
    fg: Option<&str>,
    bg: Option<&str>,
    ec: Option<&str>,
    size: u32,
) -> Value {
    let png = match render_qr_png(payload, fg, bg, ec, size) {
        Ok(b) => b,
        Err(e) => return json!({ "error": e }),
    };
    let dir = default_save_dir(app);
    let Some(path) = ask_save(app, &dir, "qr-code.png", &["png"]) else {
        return json!({ "cancelled": true });
    };
    match std::fs::write(&path, &png) {
        Ok(()) => json!({ "saved": path.to_string_lossy() }),
        Err(e) => json!({ "error": format!("Couldn't save PNG ({e}).") }),
    }
}

fn save_svg(
    app: &AppHandle,
    payload: &str,
    fg: Option<&str>,
    bg: Option<&str>,
    ec: Option<&str>,
) -> Value {
    let (svg, _) = match render_qr_svg(payload, fg, bg, ec) {
        Ok(v) => v,
        Err(e) => return json!({ "error": e }),
    };
    let dir = default_save_dir(app);
    let Some(path) = ask_save(app, &dir, "qr-code.svg", &["svg"]) else {
        return json!({ "cancelled": true });
    };
    match std::fs::write(&path, svg.as_bytes()) {
        Ok(()) => json!({ "saved": path.to_string_lossy() }),
        Err(e) => json!({ "error": format!("Couldn't save SVG ({e}).") }),
    }
}

fn copy_png(payload: &str, fg: Option<&str>, bg: Option<&str>, ec: Option<&str>) -> Result<(), String> {
    let rgb = render_qr_rgb(payload, fg, bg, ec, 1024)?;
    let png = encode_rgb_png(&rgb)?;
    clip_image::write_image(&rgb, &png)
}

// ------------------------------------------------------------------ offline QR decoder

fn scan_gray(luma: GrayImage, seen: &mut HashSet<String>, out: &mut Vec<Value>) {
    let mut prepared = rqrr::PreparedImage::prepare(luma);
    for grid in prepared.detect_grids() {
        if let Ok((_meta, content)) = grid.decode() {
            if !content.is_empty() && seen.insert(content.clone()) {
                out.push(parse_decoded_content(&content));
            }
        }
    }
}

fn to_luma_on_white(img: &DynamicImage) -> GrayImage {
    if !img.color().has_alpha() {
        return img.to_luma8();
    }
    let rgba = img.to_rgba8();
    let (w, h) = rgba.dimensions();
    let mut gray = GrayImage::new(w, h);
    for (dst, px) in gray.as_mut().iter_mut().zip(rgba.pixels()) {
        let [r, g, b, a] = px.0;
        let a = a as u32;
        let inv = 255 - a;
        let cr = (r as u32 * a + 255 * inv) / 255;
        let cg = (g as u32 * a + 255 * inv) / 255;
        let cb = (b as u32 * a + 255 * inv) / 255;
        *dst = ((cr * 299 + cg * 587 + cb * 114) / 1000) as u8;
    }
    gray
}

fn decode_dynamic_image(img: DynamicImage) -> Vec<Value> {
    let img = if img.width() > MAX_DECODE_DIM || img.height() > MAX_DECODE_DIM {
        img.resize(MAX_DECODE_DIM, MAX_DECODE_DIM, image::imageops::FilterType::Triangle)
    } else {
        img
    };

    let mut luma = to_luma_on_white(&img);
    let mut seen = HashSet::new();
    let mut out = Vec::new();

    scan_gray(luma.clone(), &mut seen, &mut out);
    if out.is_empty() {
        for p in luma.as_mut() {
            *p = 255 - *p;
        }
        scan_gray(luma, &mut seen, &mut out);
    }
    out
}

/// Decodes one or more QR codes from encoded image bytes (PNG, JPEG, WebP, BMP, GIF, TIFF, ICO).
pub fn decode_image_bytes(bytes: &[u8]) -> Result<Vec<Value>, String> {
    if bytes.is_empty() {
        return Err("That image is empty.".into());
    }
    if bytes.len() > MAX_IMAGE_BYTES {
        return Err("That image is larger than 32 MB.".into());
    }
    let img = image::load_from_memory(bytes)
        .map_err(|_| "Couldn't read that image. Try a PNG, JPEG, WebP, or BMP file.".to_string())?;
    Ok(decode_dynamic_image(img))
}

/// Decodes one or more QR codes from a raw RGBA pixel buffer.
pub fn decode_rgba(width: u32, height: u32, bytes: &[u8]) -> Result<Vec<Value>, String> {
    if width == 0 || height == 0 {
        return Err("Invalid image dimensions.".into());
    }
    let expected = (width as usize)
        .checked_mul(height as usize)
        .and_then(|px| px.checked_mul(4))
        .ok_or_else(|| "Image dimensions are too large.".to_string())?;
    if expected > MAX_RGBA_BYTES {
        return Err("Clipboard image is too large to decode.".into());
    }
    if bytes.len() != expected {
        return Err("Clipboard image data was incomplete.".into());
    }
    let rgba = RgbaImage::from_raw(width, height, bytes.to_vec())
        .ok_or_else(|| "Couldn't read clipboard image pixels.".to_string())?;
    Ok(decode_dynamic_image(DynamicImage::ImageRgba8(rgba)))
}

fn decode_file(path: &Path) -> Value {
    if path.as_os_str().is_empty() {
        return json!({ "error": "No image path provided." });
    }
    let meta = match std::fs::metadata(path) {
        Ok(m) if m.is_file() => m,
        _ => return json!({ "error": "Couldn't find that image file." }),
    };
    if meta.len() > MAX_IMAGE_BYTES as u64 {
        return json!({ "error": "That image is larger than 32 MB." });
    }
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(_) => return json!({ "error": "Couldn't read that image file." }),
    };
    let source = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "Image".into());
    match decode_image_bytes(&bytes) {
        Ok(items) => json!({ "items": items, "source": source }),
        Err(e) => json!({ "error": e }),
    }
}

fn decode_pick(app: &AppHandle) -> Value {
    let picked = app
        .dialog()
        .file()
        .add_filter("Images", &["png", "jpg", "jpeg", "webp", "bmp", "gif", "tiff"])
        .blocking_pick_file()
        .and_then(|p| p.into_path().ok());
    let Some(path) = picked else {
        return Value::Null;
    };
    decode_file(&path)
}

fn decode_data_url(raw: &str) -> Value {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return json!({ "error": "No image data to decode." });
    }
    let b64 = if trimmed.len() >= 5 && trimmed[..5].eq_ignore_ascii_case("data:") {
        match trimmed.find(";base64,") {
            Some(pos) => &trimmed[pos + 8..],
            None => return json!({ "error": "Unsupported image data format." }),
        }
    } else {
        trimmed
    };
    let max_b64 = (MAX_DATA_BYTES / 3 + 2) * 4;
    if b64.len() > max_b64 {
        return json!({ "error": "That pasted image is larger than 24 MB." });
    }
    let decoded = if b64.bytes().any(|b| b.is_ascii_whitespace()) {
        let compact: String = b64.chars().filter(|c| !c.is_ascii_whitespace()).collect();
        base64::engine::general_purpose::STANDARD.decode(compact)
    } else {
        base64::engine::general_purpose::STANDARD.decode(b64)
    };
    let bytes = match decoded {
        Ok(b) => b,
        Err(_) => return json!({ "error": "Couldn't decode the pasted image data." }),
    };
    if bytes.len() > MAX_DATA_BYTES {
        return json!({ "error": "That pasted image is larger than 24 MB." });
    }
    match decode_image_bytes(&bytes) {
        Ok(items) => json!({ "items": items, "source": "Pasted image" }),
        Err(e) => json!({ "error": e }),
    }
}

fn decode_clipboard() -> Value {
    match clip_image::read_image() {
        Ok(clip_image::ClipboardImage::Encoded(bytes)) => match decode_image_bytes(&bytes) {
            Ok(items) => json!({ "items": items, "source": "Clipboard" }),
            Err(e) => json!({ "error": e }),
        },
        Ok(clip_image::ClipboardImage::Rgba { width, height, bytes }) => match decode_rgba(width, height, &bytes) {
            Ok(items) => json!({ "items": items, "source": "Clipboard" }),
            Err(e) => json!({ "error": e }),
        },
        Err(e) => json!({ "error": e }),
    }
}

// ------------------------------------------------------------------ safe HTTP(S) URL opening

fn validate_http_url(url: &str) -> Result<&str, String> {
    if url.is_empty() || url.len() > 4096 {
        return Err("Only http:// and https:// links can be opened.".into());
    }
    if url.bytes().any(|b| b <= 0x20 || b == 0x7f || matches!(b, b'"' | b'<' | b'>')) {
        return Err("That link contains invalid characters.".into());
    }
    let rest = if url.len() >= 8 && url[..8].eq_ignore_ascii_case("https://") {
        &url[8..]
    } else if url.len() >= 7 && url[..7].eq_ignore_ascii_case("http://") {
        &url[7..]
    } else {
        return Err("Only http:// and https:// links can be opened.".into());
    };
    if rest.is_empty() || rest.starts_with('/') || rest.starts_with('\\') {
        return Err("That link is missing a host name.".into());
    }
    Ok(url)
}

fn open_http_url(url: &str) -> Result<(), String> {
    let url = validate_http_url(url)?;
    #[cfg(windows)]
    {
        let verb: Vec<u16> = "open".encode_utf16().chain(std::iter::once(0)).collect();
        let target: Vec<u16> = url.encode_utf16().chain(std::iter::once(0)).collect();
        let r = unsafe {
            windows_sys::Win32::UI::Shell::ShellExecuteW(
                std::ptr::null_mut(),
                verb.as_ptr(),
                target.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                1,
            )
        };
        if r as usize > 32 {
            Ok(())
        } else {
            Err("Windows couldn't open that link.".into())
        }
    }
    #[cfg(not(windows))]
    {
        if std::process::Command::new("xdg-open").arg(url).spawn().is_ok() {
            Ok(())
        } else {
            Err("Couldn't open that link.".into())
        }
    }
}

// ------------------------------------------------------------------ native clipboard image I/O
// Note: arboard is compiled with `default-features = false` in Cargo.toml (no `image-data` feature),
// so on Windows we read/write CF_DIB + PNG clipboard formats directly via user32/kernel32 with zero
// extra Cargo dependencies.

mod clip_image {
    pub enum ClipboardImage {
        Encoded(Vec<u8>),
        #[allow(dead_code)]
        Rgba { width: u32, height: u32, bytes: Vec<u8> },
    }

    #[cfg(windows)]
    mod imp {
        use super::ClipboardImage;
        use super::super::{CLIPBOARD_EMPTY_ERR, MAX_IMAGE_BYTES};
        use image::RgbImage;
        use std::ffi::c_void;

        const CF_DIB: u32 = 8;
        const CF_DIBV5: u32 = 17;
        const GMEM_MOVEABLE: u32 = 0x0002;

        #[link(name = "user32")]
        extern "system" {
            fn OpenClipboard(hwnd_new_owner: *mut c_void) -> i32;
            fn CloseClipboard() -> i32;
            fn EmptyClipboard() -> i32;
            fn GetClipboardData(u_format: u32) -> *mut c_void;
            fn SetClipboardData(u_format: u32, h_mem: *mut c_void) -> *mut c_void;
            fn IsClipboardFormatAvailable(format: u32) -> i32;
            fn RegisterClipboardFormatW(lpsz_format: *const u16) -> u32;
        }

        #[link(name = "kernel32")]
        extern "system" {
            fn GlobalAlloc(u_flags: u32, dw_bytes: usize) -> *mut c_void;
            fn GlobalLock(h_mem: *mut c_void) -> *mut c_void;
            fn GlobalUnlock(h_mem: *mut c_void) -> i32;
            fn GlobalSize(h_mem: *mut c_void) -> usize;
            fn GlobalFree(h_mem: *mut c_void) -> *mut c_void;
        }

        struct ClipGuard;

        impl ClipGuard {
            fn open() -> Result<Self, String> {
                for _ in 0..8 {
                    if unsafe { OpenClipboard(std::ptr::null_mut()) } != 0 {
                        return Ok(ClipGuard);
                    }
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                Err("Couldn't open the Windows clipboard right now.".into())
            }
        }

        impl Drop for ClipGuard {
            fn drop(&mut self) {
                unsafe {
                    CloseClipboard();
                }
            }
        }

        fn png_format() -> u32 {
            let name: [u16; 4] = [b'P' as u16, b'N' as u16, b'G' as u16, 0];
            unsafe { RegisterClipboardFormatW(name.as_ptr()) }
        }

        unsafe fn set_format(format: u32, data: &[u8]) -> Result<(), String> {
            let hmem = GlobalAlloc(GMEM_MOVEABLE, data.len());
            if hmem.is_null() {
                return Err("Not enough memory to copy the QR image.".into());
            }
            let ptr = GlobalLock(hmem) as *mut u8;
            if ptr.is_null() {
                GlobalFree(hmem);
                return Err("Couldn't lock clipboard memory.".into());
            }
            std::ptr::copy_nonoverlapping(data.as_ptr(), ptr, data.len());
            GlobalUnlock(hmem);
            if SetClipboardData(format, hmem).is_null() {
                GlobalFree(hmem);
                return Err("Couldn't write the QR image to the clipboard.".into());
            }
            Ok(())
        }

        unsafe fn read_format(format: u32) -> Result<Option<Vec<u8>>, String> {
            if format == 0 || IsClipboardFormatAvailable(format) == 0 {
                return Ok(None);
            }
            let hmem = GetClipboardData(format);
            if hmem.is_null() {
                return Ok(None);
            }
            let len = GlobalSize(hmem);
            if len == 0 {
                return Ok(None);
            }
            if len > MAX_IMAGE_BYTES {
                return Err("Clipboard image is larger than 32 MB.".into());
            }
            let ptr = GlobalLock(hmem) as *const u8;
            if ptr.is_null() {
                return Err("Couldn't read image bytes from the clipboard.".into());
            }
            let bytes = std::slice::from_raw_parts(ptr, len).to_vec();
            GlobalUnlock(hmem);
            Ok(Some(bytes))
        }

        fn build_cf_dib(rgb: &RgbImage) -> Vec<u8> {
            let (w, h) = rgb.dimensions();
            let pixel_bytes = (w as usize) * (h as usize) * 4;
            let mut dib = Vec::with_capacity(40 + pixel_bytes);
            dib.extend_from_slice(&40u32.to_le_bytes()); // biSize
            dib.extend_from_slice(&(w as i32).to_le_bytes()); // biWidth
            dib.extend_from_slice(&(h as i32).to_le_bytes()); // biHeight (positive = bottom-up)
            dib.extend_from_slice(&1u16.to_le_bytes()); // biPlanes
            dib.extend_from_slice(&32u16.to_le_bytes()); // biBitCount
            dib.extend_from_slice(&0u32.to_le_bytes()); // biCompression (BI_RGB)
            dib.extend_from_slice(&(pixel_bytes as u32).to_le_bytes()); // biSizeImage
            dib.extend_from_slice(&0i32.to_le_bytes()); // biXPelsPerMeter
            dib.extend_from_slice(&0i32.to_le_bytes()); // biYPelsPerMeter
            dib.extend_from_slice(&0u32.to_le_bytes()); // biClrUsed
            dib.extend_from_slice(&0u32.to_le_bytes()); // biClrImportant

            for y in (0..h).rev() {
                for x in 0..w {
                    let [r, g, b] = rgb.get_pixel(x, y).0;
                    dib.extend_from_slice(&[b, g, r, 255]);
                }
            }
            dib
        }

        fn wrap_dib_as_bmp(dib: &[u8]) -> Option<Vec<u8>> {
            if dib.len() < 40 {
                return None;
            }
            let bi_size = u32::from_le_bytes([dib[0], dib[1], dib[2], dib[3]]);
            if bi_size < 40 || (bi_size as usize) > dib.len() {
                return None;
            }
            let bit_count = u16::from_le_bytes([dib[14], dib[15]]);
            let compression = u32::from_le_bytes([dib[16], dib[17], dib[18], dib[19]]);
            let clr_used = u32::from_le_bytes([dib[32], dib[33], dib[34], dib[35]]);
            let palette_bytes = if clr_used > 0 {
                clr_used.checked_mul(4)?
            } else if bit_count <= 8 && bit_count > 0 {
                (1u32 << bit_count).checked_mul(4)?
            } else if bi_size == 40 && compression == 3 {
                12 // 3 DWORD bitfield masks follow BITMAPINFOHEADER
            } else {
                0
            };
            let off_bits = 14u32.checked_add(bi_size)?.checked_add(palette_bytes)?;
            let file_size = 14usize.checked_add(dib.len())?;
            let mut bmp = Vec::with_capacity(file_size);
            bmp.extend_from_slice(b"BM");
            bmp.extend_from_slice(&(file_size as u32).to_le_bytes());
            bmp.extend_from_slice(&0u32.to_le_bytes());
            bmp.extend_from_slice(&off_bits.to_le_bytes());
            bmp.extend_from_slice(dib);
            Some(bmp)
        }

        pub fn write_image(rgb: &RgbImage, png_bytes: &[u8]) -> Result<(), String> {
            let _guard = ClipGuard::open()?;
            if unsafe { EmptyClipboard() } == 0 {
                return Err("Couldn't clear the Windows clipboard.".into());
            }
            let dib = build_cf_dib(rgb);
            unsafe {
                set_format(CF_DIB, &dib)?;
                let png_fmt = png_format();
                if png_fmt != 0 {
                    let _ = set_format(png_fmt, png_bytes);
                }
            }
            Ok(())
        }

        pub fn read_image() -> Result<ClipboardImage, String> {
            let _guard = ClipGuard::open()?;
            let png_fmt = png_format();
            if let Some(png) = unsafe { read_format(png_fmt)? } {
                return Ok(ClipboardImage::Encoded(png));
            }
            for fmt in [CF_DIB, CF_DIBV5] {
                if let Some(dib) = unsafe { read_format(fmt)? } {
                    if let Some(bmp) = wrap_dib_as_bmp(&dib) {
                        return Ok(ClipboardImage::Encoded(bmp));
                    }
                }
            }
            Err(CLIPBOARD_EMPTY_ERR.into())
        }
    }

    #[cfg(not(windows))]
    mod imp {
        use super::ClipboardImage;
        use image::RgbImage;

        pub fn write_image(_rgb: &RgbImage, _png_bytes: &[u8]) -> Result<(), String> {
            Err("Clipboard image copy is only supported on Windows in this build.".into())
        }

        pub fn read_image() -> Result<ClipboardImage, String> {
            Err("Clipboard image paste is only supported on Windows in this build.".into())
        }
    }

    pub use imp::{read_image, write_image};
}

// ------------------------------------------------------------------ unit tests

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wifi_payload_escape_and_roundtrip() {
        let ssid = r#"Home;Net:Work\Guest,"5G""#;
        let pass = r#"p;a:s\s,w"d!99"#;
        let payload = build_wifi_payload(ssid, pass, "WPA", true);
        assert_eq!(
            payload,
            r#"WIFI:T:WPA;S:Home\;Net\:Work\\Guest\,\"5G\";P:p\;a\:s\\s\,w\"d!99;H:true;;"#
        );

        let parsed = parse_decoded_content(&payload);
        assert_eq!(parsed["kind"], "wifi");
        assert_eq!(parsed["text"], payload);
        assert_eq!(parsed["wifi"]["ssid"], ssid);
        assert_eq!(parsed["wifi"]["password"], pass);
        assert_eq!(parsed["wifi"]["security"], "WPA");
        assert_eq!(parsed["wifi"]["hidden"], true);

        let open_payload = build_wifi_payload("Cafe Free", "ignored", "nopass", false);
        assert!(!open_payload.contains("P:"));
        let open_parsed = parse_decoded_content(&open_payload);
        assert_eq!(open_parsed["kind"], "wifi");
        assert_eq!(open_parsed["wifi"]["ssid"], "Cafe Free");
        assert_eq!(open_parsed["wifi"]["password"], "");
        assert_eq!(open_parsed["wifi"]["security"], "nopass");
        assert_eq!(open_parsed["wifi"]["hidden"], false);
    }

    #[test]
    fn parse_decoded_content_kinds() {
        assert_eq!(parse_decoded_content("https://swiftlane.example/path")["kind"], "url");
        assert_eq!(parse_decoded_content("HTTP://example.com")["kind"], "url");
        assert_eq!(parse_decoded_content("mailto:ada@example.com?subject=Hi")["kind"], "email");
        assert_eq!(
            parse_decoded_content("BEGIN:VCARD\nVERSION:3.0\nFN:Ada\nEND:VCARD")["kind"],
            "vcard"
        );
        assert_eq!(parse_decoded_content("hello world")["kind"], "text");
        assert_eq!(parse_decoded_content("wifi:T:WEP;S:Lab;P:12345;;")["kind"], "wifi");
    }

    #[test]
    fn png_generate_and_decode_roundtrip() {
        // 1. URL QR
        let url = "https://example.org/swiftlane?qr=1&offline=true";
        let png = render_qr_png(url, Some("#0e121c"), Some("#ffffff"), Some("M"), 512).unwrap();
        let items = decode_image_bytes(&png).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["kind"], "url");
        assert_eq!(items[0]["text"], url);

        // 2. Wi-Fi QR with escaped characters
        let wifi = build_wifi_payload("Studio:2.4;GHz", r#"sec\ret,pw"#, "WPA", false);
        let wifi_png = render_qr_png(&wifi, None, None, Some("Q"), 512).unwrap();
        let wifi_items = decode_image_bytes(&wifi_png).unwrap();
        assert_eq!(wifi_items.len(), 1);
        assert_eq!(wifi_items[0]["kind"], "wifi");
        assert_eq!(wifi_items[0]["wifi"]["ssid"], "Studio:2.4;GHz");
        assert_eq!(wifi_items[0]["wifi"]["password"], r#"sec\ret,pw"#);

        // 3. Inverted (white-on-dark) QR
        let inv_text = "Inverted QR works offline!";
        let inv_png = render_qr_png(inv_text, Some("#ffffff"), Some("#0e121c"), Some("M"), 512).unwrap();
        let inv_items = decode_image_bytes(&inv_png).unwrap();
        assert_eq!(inv_items.len(), 1);
        assert_eq!(inv_items[0]["kind"], "text");
        assert_eq!(inv_items[0]["text"], inv_text);
    }

    #[test]
    fn decode_rgba_roundtrip() {
        let rgb = render_qr_rgb("rgba-clipboard-test", None, None, Some("M"), 384).unwrap();
        let (w, h) = rgb.dimensions();
        let mut rgba = Vec::with_capacity((w * h * 4) as usize);
        for px in rgb.pixels() {
            rgba.extend_from_slice(&[px.0[0], px.0[1], px.0[2], 255]);
        }
        let items = decode_rgba(w, h, &rgba).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["text"], "rgba-clipboard-test");
    }

    #[test]
    fn color_sanitization_and_error_cases() {
        assert_eq!(sanitize_hex_color(Some("#AbC123"), DEFAULT_FG), "#abc123");
        assert_eq!(sanitize_hex_color(Some(r##""/><script>alert(1)</script>"##), DEFAULT_FG), DEFAULT_FG);
        assert_eq!(sanitize_hex_color(Some("#123"), DEFAULT_BG), DEFAULT_BG);
        assert_eq!(sanitize_hex_color(Some("red"), DEFAULT_FG), DEFAULT_FG);

        let (svg, modules) = render_qr_svg("ok", Some("not-a-color"), Some("<bad>"), Some("M")).unwrap();
        assert!(modules >= 21);
        assert!(svg.contains(DEFAULT_FG));
        assert!(svg.contains(DEFAULT_BG));
        assert!(!svg.contains("<bad>"));

        assert!(render_qr_svg("", None, None, None).is_err());
        let huge = "a".repeat(MAX_PAYLOAD_BYTES + 1);
        assert!(render_qr_svg(&huge, None, None, Some("L")).is_err());
        assert!(decode_image_bytes(b"not a valid image at all").is_err());
        assert!(decode_image_bytes(&[]).is_err());

        assert!(validate_http_url("https://example.com/a?b=1").is_ok());
        assert!(validate_http_url("http://192.168.1.1").is_ok());
        assert!(validate_http_url("file:///C:/Windows/win.ini").is_err());
        assert!(validate_http_url("javascript:alert(1)").is_err());
        assert!(validate_http_url("cmd:/c calc").is_err());
        assert!(validate_http_url("https://example.com\nfoo").is_err());
    }
}
