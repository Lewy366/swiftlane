//! The image editor (ported from Source/toolkit/images.py and the matching methods in
//! Source/swiftlane.py): crop, straighten, rotate/flip, resize, adjust colours, compress, remove
//! metadata, change format, with a fast preview for the live editor.
//!
//! Edits are applied in this order (the page shows them the same way):
//!   upright (EXIF) -> rotate 90° steps -> flip -> straighten -> crop -> resize -> adjust -> save
//!
//! Page calls: image_preview(path), image_render(path, ops), edit_images(paths, ops).
//! Originals are never changed unless you pick the original's own name in the Save box (Windows
//! asks first); then the new picture replaces it in one step, so a failure can't leave it half-written.

use base64::Engine;
use image::{imageops, DynamicImage, ImageDecoder, ImageEncoder, ImageFormat, ImageReader, Rgba, RgbaImage};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::SystemTime;
use tauri::{AppHandle, Manager};

use super::{arg, ask_folder, ask_save, exif, webp};
use crate::jobs::JobError;

pub const PREVIEW_MAX: u32 = 1400;
const RATIOS: [(&str, f64); 7] = [("1:1", 1.0), ("4:3", 4.0 / 3.0), ("3:2", 3.0 / 2.0), ("16:9", 16.0 / 9.0), ("9:16", 9.0 / 16.0), ("3:4", 3.0 / 4.0), ("4:5", 4.0 / 5.0)];

pub fn dispatch(app: &AppHandle, method: &str, args: &[Value]) -> Option<Value> {
    let path = || PathBuf::from(arg(args, 0).as_str().unwrap_or(""));
    Some(match method {
        "image_preview" => preview_source(&path()).unwrap_or_else(|e| json!({ "error": e })),
        "image_render" => render_preview(&path(), arg(args, 1)).unwrap_or_else(|e| json!({ "error": format!("Preview failed: {e}") })),
        "edit_images" => edit_images(app, arg(args, 0), arg(args, 1)),
        _ => return None,
    })
}

// ---------------------------------------------------------------- opening pictures

/// A picture turned upright, as RGBA, plus what's needed to save it again.
pub struct Picture {
    pub img: RgbaImage,
    pub alpha: bool, // the original had transparency
    pub exif: Option<Vec<u8>>,
    pub icc: Option<Vec<u8>>,
    pub format: Option<&'static str>, // "JPEG", "PNG", "WEBP", ... (as the Python version named them)
}

fn format_name(f: ImageFormat) -> Option<&'static str> {
    Some(match f {
        ImageFormat::Jpeg => "JPEG",
        ImageFormat::Png => "PNG",
        ImageFormat::WebP => "WEBP",
        ImageFormat::Gif => "GIF",
        ImageFormat::Bmp => "BMP",
        ImageFormat::Tiff => "TIFF",
        ImageFormat::Ico => "ICO",
        ImageFormat::Avif => "AVIF",
        _ => return None,
    })
}

fn ext_of(path: &Path) -> String {
    path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default()
}

/// What kind of picture the file really is (from its first bytes), without decoding it.
fn sniff_format(path: &Path) -> Option<&'static str> {
    if matches!(ext_of(path).as_str(), "heic" | "heif") {
        return Some("HEIF");
    }
    ImageReader::open(path).ok()?.with_guessed_format().ok()?.format().and_then(format_name)
}

fn image_error(e: image::ImageError) -> String {
    match e {
        image::ImageError::Limits(_) => "This picture is too big to open.".into(),
        image::ImageError::Unsupported(_) => "Swiftlane can't open this kind of picture.".into(),
        image::ImageError::IoError(_) => "Swiftlane couldn't read this file.".into(),
        _ => "This picture seems to be damaged, so Swiftlane can't open it.".into(),
    }
}

pub fn open_upright(path: &Path) -> Result<Picture, String> {
    let ext = ext_of(path);
    if matches!(ext.as_str(), "heic" | "heif" | "avif") {
        return via_ffmpeg(path, &ext);
    }
    let reader = ImageReader::open(path).map_err(|_| "Swiftlane couldn't read this file.".to_string())?;
    let reader = reader.with_guessed_format().map_err(|_| "Swiftlane couldn't read this file.".to_string())?;
    let format = reader.format();
    if format == Some(ImageFormat::Avif) {
        return via_ffmpeg(path, "avif");
    }
    let Some(name) = format.and_then(format_name) else {
        return Err("This file isn't a picture Swiftlane can open.".into());
    };
    let mut dec = reader.into_decoder().map_err(image_error)?;
    let icc = dec.icc_profile().ok().flatten();
    let exif = dec.exif_metadata().ok().flatten().map(|e| exif::raw(&e).to_vec());
    let orientation = dec.orientation().unwrap_or(image::metadata::Orientation::NoTransforms);
    let mut img = DynamicImage::from_decoder(dec).map_err(image_error)?;
    img.apply_orientation(orientation);
    let alpha = img.color().has_alpha();
    Ok(Picture { img: img.into_rgba8(), alpha, exif, icc, format: Some(name) })
}

/// HEIC (iPhone photos) and AVIF: the image crate can't read them without C libraries, so the
/// FFmpeg helper program decodes them (it turns them upright too). Their EXIF isn't carried over.
fn via_ffmpeg(path: &Path, ext: &str) -> Result<Picture, String> {
    let heic = ext != "avif";
    let what = if heic { "HEIC photos" } else { "AVIF pictures" };
    let Some(ff) = super::program("ffmpeg") else {
        return Err(format!("Can't open {what} yet: that needs the FFmpeg helper program next to Swiftlane."));
    };
    if heic && !ffmpeg_reads_heic(&ff) {
        return Err("Can't open HEIC photos yet: the FFmpeg helper program next to Swiftlane is too old (it needs version 7.1 or newer).".into());
    }
    let out = super::command(&ff)
        .args(["-hide_banner", "-nostdin", "-v", "error", "-i"])
        .arg(file_arg(path))
        .args(["-frames:v", "1", "-f", "image2pipe", "-c:v", "png", "-pix_fmt", "rgba", "-"])
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|_| format!("Can't open {what}: the FFmpeg helper program didn't start."))?;
    if !out.status.success() || out.stdout.is_empty() {
        return Err(format!("FFmpeg couldn't open this picture, so Swiftlane can't edit it ({what})."));
    }
    let img = image::load_from_memory_with_format(&out.stdout, ImageFormat::Png).map_err(image_error)?.into_rgba8();
    let alpha = img.pixels().any(|p| p[3] < 255);
    Ok(Picture { img, alpha, exif: None, icc: None, format: Some(if heic { "HEIF" } else { "AVIF" }) })
}

/// "file:" stops FFmpeg from reading a name as an option or a network address.
fn file_arg(path: &Path) -> std::ffi::OsString {
    let mut s = std::ffi::OsString::from("file:");
    s.push(path.as_os_str());
    s
}

/// FFmpeg puts the tiles of an iPhone photo back together only from version 7.1 on; older
/// versions would show one tile instead of the photo.
fn ffmpeg_reads_heic(ff: &Path) -> bool {
    static OK: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *OK.get_or_init(|| {
        let Ok(out) = super::command(ff).arg("-version").stdin(std::process::Stdio::null()).output() else { return false };
        let text = String::from_utf8_lossy(&out.stdout);
        let version = text.split_whitespace().nth(2).unwrap_or("");
        ffmpeg_version_ok(version)
    })
}

fn ffmpeg_version_ok(version: &str) -> bool {
    let v = version.trim_start_matches('n');
    let mut parts = v.split(|c: char| !c.is_ascii_digit());
    let major: u32 = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    let minor: u32 = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    // development builds are named "N-12345-g..." or after their date ("2025-01-02-git-...")
    v.starts_with("N-") || major >= 2000 || (major, minor) >= (7, 1)
}

// ---------------------------------------------------------------- saving pictures

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Fmt {
    Jpeg,
    Png,
    Webp,
}

impl Fmt {
    pub fn ext(self) -> &'static str {
        match self {
            Fmt::Jpeg => "jpg",
            Fmt::Png => "png",
            Fmt::Webp => "webp",
        }
    }
    fn mime(self) -> &'static str {
        match self {
            Fmt::Jpeg => "image/jpeg",
            Fmt::Png => "image/png",
            Fmt::Webp => "image/webp",
        }
    }
    fn from_name(name: &str) -> Option<Fmt> {
        match name {
            "JPEG" => Some(Fmt::Jpeg),
            "PNG" => Some(Fmt::Png),
            "WEBP" => Some(Fmt::Webp),
            _ => None,
        }
    }
    fn from_ext(ext: &str) -> Option<Fmt> {
        match ext {
            "jpg" | "jpeg" => Some(Fmt::Jpeg),
            "png" => Some(Fmt::Png),
            "webp" => Some(Fmt::Webp),
            _ => None,
        }
    }
}

/// Transparent parts turned white (for JPG).
pub fn on_white(img: &RgbaImage) -> Vec<u8> {
    let mut out = Vec::with_capacity(img.width() as usize * img.height() as usize * 3);
    for p in img.pixels() {
        let a = p[3] as u32;
        for c in 0..3 {
            out.push(((p[c] as u32 * a + 255 * (255 - a) + 127) / 255) as u8);
        }
    }
    out
}

fn rgb_bytes(img: &RgbaImage) -> Vec<u8> {
    img.pixels().flat_map(|p| [p[0], p[1], p[2]]).collect()
}

/// The picture as a file in memory. `exif`/`icc`: metadata to keep (None to leave it out).
pub fn encode(img: &RgbaImage, alpha: bool, fmt: Fmt, quality: u8, exif_block: Option<&[u8]>, icc: Option<&[u8]>) -> Result<Vec<u8>, String> {
    let (w, h) = img.dimensions();
    let exif_block = exif_block.and_then(exif::for_copy);
    let mut buf = Vec::new();
    match fmt {
        Fmt::Jpeg => {
            if w > 65_535 || h > 65_535 {
                return Err("JPG pictures can be at most 65535 pixels wide and tall. Make it smaller or pick PNG.".into());
            }
            let mut enc = jpeg_encoder::Encoder::new(&mut buf, quality.clamp(1, 100));
            // No optimised Huffman tables: the image crate's own JPG reader decodes those files as
            // solid green, so Swiftlane couldn't reopen what it saved (test: saved_jpg_reopens_with_its_colours).
            enc.set_sampling_factor(jpeg_encoder::SamplingFactor::F_2_2);
            if let Some(icc) = icc {
                let _ = enc.add_icc_profile(icc);
            }
            if let Some(e) = exif_block.as_deref().filter(|e| e.len() <= 65_000) {
                let _ = enc.add_exif_metadata(e);
            }
            let rgb = if alpha { on_white(img) } else { rgb_bytes(img) };
            enc.encode(&rgb, w as u16, h as u16, jpeg_encoder::ColorType::Rgb).map_err(|_| "The picture couldn't be saved as JPG.".to_string())?;
        }
        Fmt::Png => {
            let mut enc = image::codecs::png::PngEncoder::new_with_quality(&mut buf, image::codecs::png::CompressionType::Best, image::codecs::png::FilterType::Adaptive);
            if let Some(icc) = icc {
                let _ = enc.set_icc_profile(icc.to_vec());
            }
            if let Some(e) = exif_block {
                let _ = enc.set_exif_metadata(e);
            }
            let result = if alpha {
                enc.write_image(img.as_raw(), w, h, image::ExtendedColorType::Rgba8)
            } else {
                enc.write_image(&rgb_bytes(img), w, h, image::ExtendedColorType::Rgb8)
            };
            result.map_err(|_| "The picture couldn't be saved as PNG.".to_string())?;
        }
        Fmt::Webp => {
            let pixels = if alpha { img.as_raw().clone() } else { rgb_bytes(img) };
            let plain = webp::encode(&pixels, w, h, alpha, quality.clamp(1, 100) as f32)?;
            buf = webp::add_metadata(&plain, w, h, exif_block.as_deref(), icc);
        }
    }
    Ok(buf)
}

fn io_message(e: &std::io::Error) -> String {
    match e.kind() {
        std::io::ErrorKind::PermissionDenied => "Swiftlane isn't allowed to save in that folder. Pick another one.".into(),
        std::io::ErrorKind::StorageFull => "The disk is full, so the picture couldn't be saved.".into(),
        _ => format!("The picture couldn't be saved ({e})."),
    }
}

/// folder/name, or "name (1).ext" etc. if taken, created empty so nothing else can take it.
/// Never overwrites anything.
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
                return Ok(candidate);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(io_message(&e)),
        }
    }
    Err("There's no free file name left in that folder.".into())
}

/// Writes exactly `path` (the file picked in the Save box; Windows already asked before replacing
/// one). Written next to it first, then swapped in, so the old file survives any failure.
fn save_exact(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let folder = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    std::fs::create_dir_all(folder).map_err(|e| io_message(&e))?;
    let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let tmp = folder.join(format!(".{name}.swiftlane-part"));
    let result = std::fs::write(&tmp, bytes).and_then(|_| std::fs::rename(&tmp, path));
    if let Err(e) = result {
        let _ = std::fs::remove_file(&tmp);
        return Err(io_message(&e));
    }
    Ok(())
}

// ---------------------------------------------------------------- the edit pipeline

/// Python's round(): halves go to the even number.
fn pyround(x: f64) -> f64 {
    x.round_ties_even()
}

/// A number from the page (numbers or numeric text, like Python's float()).
fn num(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse().ok(),
        Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        _ => None,
    }
    .filter(|x| x.is_finite())
}

fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64() != Some(0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

fn rotation(ops: &Value) -> i64 {
    (num(&ops["rotate"]).unwrap_or(0.0).trunc() as i64).rem_euclid(360)
}

/// How much to zoom in after rotating by `angle_deg` so no empty corners show.
pub fn straighten_scale(angle_deg: f64, w: f64, h: f64) -> f64 {
    let a = angle_deg.abs().to_radians();
    if a == 0.0 {
        return 1.0;
    }
    f64::max((w * a.cos() + h * a.sin()) / w, (w * a.sin() + h * a.cos()) / h)
}

/// Final pixel size for these ops, without touching any pixels.
pub fn output_size(full_w: u32, full_h: u32, ops: &Value) -> (u32, u32) {
    let (mut w, mut h) = if matches!(rotation(ops), 90 | 270) { (full_h as f64, full_w as f64) } else { (full_w as f64, full_h as f64) };
    let s = straighten_scale(num(&ops["straighten"]).unwrap_or(0.0), w, h);
    w /= s;
    h /= s;
    if let Some((_, _, bw, bh)) = crop_fractions(ops, w, h) {
        w *= bw;
        h *= bh;
    }
    let (w, h) = (pyround(w).max(1.0) as u32, pyround(h).max(1.0) as u32);
    resize_target(w, h, &ops["resize"])
}

/// [x, y, w, h] as fractions of the (rotated, straightened) image, or None.
fn crop_fractions(ops: &Value, w: f64, h: f64) -> Option<(f64, f64, f64, f64)> {
    if let Some(b) = ops["crop_box"].as_array().filter(|b| b.len() == 4) {
        let v: Vec<f64> = b.iter().map(|x| num(x).unwrap_or(0.0).clamp(0.0, 1.0)).collect();
        let (x, y) = (v[0], v[1]);
        let (bw, bh) = (v[2].min(1.0 - x), v[3].min(1.0 - y));
        if bw > 0.001 && bh > 0.001 && (bw, bh) != (1.0, 1.0) {
            return Some((x, y, bw, bh));
        }
        return None;
    }
    let ratio = RATIOS.iter().find(|(k, _)| Some(*k) == ops["crop"].as_str()).map(|(_, r)| *r)?;
    if w / h > ratio {
        let bw = ratio * h / w;
        Some(((1.0 - bw) / 2.0, 0.0, bw, 1.0))
    } else {
        let bh = (w / ratio) / h;
        Some((0.0, (1.0 - bh) / 2.0, 1.0, bh))
    }
}

fn resize_target(w: u32, h: u32, spec: &Value) -> (u32, u32) {
    if !truthy(spec) || !spec.is_object() {
        return (w, h);
    }
    let (fw, fh) = (w as f64, h as f64);
    if truthy(&spec["width"]) || truthy(&spec["height"]) {
        // an exact size from the editor (the page keeps the aspect already)
        let mut tw = num(&spec["width"]).unwrap_or(0.0).trunc().max(0.0);
        let mut th = num(&spec["height"]).unwrap_or(0.0).trunc().max(0.0);
        if tw > 0.0 && th == 0.0 {
            th = pyround(fh * tw / fw);
        }
        if th > 0.0 && tw == 0.0 {
            tw = pyround(fw * th / fh);
        }
        return (cap(tw), cap(th));
    }
    let value = num(&spec["value"]).unwrap_or(0.0);
    if value <= 0.0 {
        return (w, h);
    }
    let scale = match spec["mode"].as_str() {
        Some("percent") => value / 100.0,
        Some("longest") => value / fw.max(fh),
        Some("width") => value / fw,
        Some("height") => value / fh,
        _ => return (w, h),
    };
    (cap(pyround(fw * scale)), cap(pyround(fh * scale)))
}

/// At least 1 pixel; at most 30000 (anything bigger can't be held in memory anyway).
fn cap(v: f64) -> u32 {
    v.clamp(1.0, 30_000.0) as u32
}

/// Geometry and colour edits on `img` (any scale). final_size: resize to this at the end.
pub fn transform(mut img: RgbaImage, ops: &Value, final_size: Option<(u32, u32)>) -> RgbaImage {
    img = match rotation(ops) {
        90 => imageops::rotate90(&img), // clockwise, like the page
        180 => imageops::rotate180(&img),
        270 => imageops::rotate270(&img),
        _ => img,
    };
    let flip = ops["flip"].as_str();
    if truthy(&ops["flip_h"]) || flip == Some("horizontal") {
        imageops::flip_horizontal_in_place(&mut img);
    }
    if truthy(&ops["flip_v"]) || flip == Some("vertical") {
        imageops::flip_vertical_in_place(&mut img);
    }
    let angle = num(&ops["straighten"]).unwrap_or(0.0);
    if angle != 0.0 {
        img = straighten(&img, angle);
    }
    let (w, h) = img.dimensions();
    if let Some((x, y, bw, bh)) = crop_fractions(ops, w as f64, h as f64) {
        let (x0, y0, x1, y1) = (pyround(x * w as f64), pyround(y * h as f64), pyround((x + bw) * w as f64), pyround((y + bh) * h as f64));
        let x0 = (x0.max(0.0) as u32).min(w - 1);
        let y0 = (y0.max(0.0) as u32).min(h - 1);
        let cw = ((x1 as i64 - x0 as i64).max(1) as u32).min(w - x0);
        let ch = ((y1 as i64 - y0 as i64).max(1) as u32).min(h - y0);
        img = imageops::crop_imm(&img, x0, y0, cw, ch).to_image();
    }
    if let Some((fw, fh)) = final_size {
        if (fw, fh) != img.dimensions() {
            img = imageops::resize(&img, fw.max(1), fh.max(1), imageops::FilterType::Lanczos3);
        }
    }
    adjust(&mut img, &ops["adjust"]);
    img
}

/// Turns the picture clockwise by `angle` degrees and keeps the largest middle part with no
/// empty corners (the same size Python cut out).
fn straighten(img: &RgbaImage, angle: f64) -> RgbaImage {
    let (w, h) = (img.width() as f64, img.height() as f64);
    let s = straighten_scale(angle, w, h);
    let (cw, ch) = (w / s, h / s);
    let x0 = pyround((w - cw) / 2.0);
    let y0 = pyround((h - ch) / 2.0);
    let ow = (pyround((w + cw) / 2.0) - x0).max(1.0) as u32;
    let oh = (pyround((h + ch) / 2.0) - y0).max(1.0) as u32;
    let (sin, cos) = angle.to_radians().sin_cos();
    RgbaImage::from_fn(ow, oh, |ox, oy| {
        let dx = x0 + ox as f64 + 0.5 - w / 2.0;
        let dy = y0 + oy as f64 + 0.5 - h / 2.0;
        let sx = dx * cos + dy * sin + w / 2.0 - 0.5;
        let sy = -dx * sin + dy * cos + h / 2.0 - 0.5;
        bicubic(img, sx, sy)
    })
}

fn cubic(t: f64) -> f64 {
    // Keys' kernel with a = -0.5, as Pillow uses
    let t = t.abs();
    if t < 1.0 {
        (1.5 * t - 2.5) * t * t + 1.0
    } else if t < 2.0 {
        ((-0.5 * t + 2.5) * t - 4.0) * t + 2.0
    } else {
        0.0
    }
}

fn bicubic(img: &RgbaImage, x: f64, y: f64) -> Rgba<u8> {
    let (w, h) = (img.width() as i64, img.height() as i64);
    let (fx, fy) = (x.floor(), y.floor());
    let mut acc = [0.0f64; 4];
    for j in -1..=2 {
        let wy = cubic(y - (fy + j as f64));
        let py = (fy as i64 + j).clamp(0, h - 1) as u32;
        for i in -1..=2 {
            let wgt = wy * cubic(x - (fx + i as f64));
            let px = (fx as i64 + i).clamp(0, w - 1) as u32;
            let p = img.get_pixel(px, py);
            for c in 0..4 {
                acc[c] += wgt * p[c] as f64;
            }
        }
    }
    Rgba(acc.map(|v| v.round().clamp(0.0, 255.0) as u8))
}

/// Pillow's grey: (299 R + 587 G + 114 B) / 1000.
fn luma(p: &Rgba<u8>) -> f64 {
    ((p[0] as u32 * 19595 + p[1] as u32 * 38470 + p[2] as u32 * 7471 + 0x8000) >> 16) as f64
}

fn mix(base: f64, v: f64, f: f64) -> u8 {
    (base + (v - base) * f).round().clamp(0.0, 255.0) as u8
}

/// Brightness, contrast, saturation and sharpness, each like Pillow's ImageEnhance (1 = unchanged).
fn adjust(img: &mut RgbaImage, adjust: &Value) {
    let factor = |k: &str| num(&adjust[k]).unwrap_or(1.0);
    let changed = |f: f64| (f - 1.0).abs() > 1e-3;
    let b = factor("brightness");
    if changed(b) {
        for p in img.pixels_mut() {
            for c in 0..3 {
                p[c] = mix(0.0, p[c] as f64, b);
            }
        }
    }
    let k = factor("contrast");
    if changed(k) && img.width() * img.height() > 0 {
        let mean = img.pixels().map(luma).sum::<f64>() / (img.width() as f64 * img.height() as f64);
        let mean = (mean + 0.5).trunc();
        for p in img.pixels_mut() {
            for c in 0..3 {
                p[c] = mix(mean, p[c] as f64, k);
            }
        }
    }
    let s = factor("saturation");
    if changed(s) {
        for p in img.pixels_mut() {
            let grey = luma(p);
            for c in 0..3 {
                p[c] = mix(grey, p[c] as f64, s);
            }
        }
    }
    let sh = factor("sharpness");
    let (w, h) = img.dimensions();
    if changed(sh) && w > 2 && h > 2 {
        // Pillow blends with a softened copy (3x3 "smooth" filter); the outer border stays as it is.
        let src = img.clone();
        for y in 1..h - 1 {
            for x in 1..w - 1 {
                for c in 0..3 {
                    let mut sum = 0.0;
                    for (dy, dx) in [(0, 0), (0, 1), (0, 2), (1, 0), (1, 2), (2, 0), (2, 1), (2, 2)] {
                        sum += src.get_pixel(x + dx - 1, y + dy - 1)[c] as f64;
                    }
                    let v = src.get_pixel(x, y)[c] as f64;
                    let soft = ((sum + 5.0 * v) / 13.0).round();
                    img.get_pixel_mut(x, y)[c] = mix(soft, v, sh);
                }
            }
        }
    }
}

/// The output format: a name typed in the Save box ("photo.png") decides; otherwise the format
/// chosen on the page; otherwise the original's.
fn target_format(ops: &Value, src_format: Option<&str>, src: &Path) -> Fmt {
    let chosen = ops["output"].as_str().map(|o| ext_of(Path::new(o))).unwrap_or_default();
    if let Some(f) = Fmt::from_ext(&chosen) {
        return f;
    }
    let format = match ops["format"].as_str() {
        None | Some("") => "keep",
        Some(f) => f,
    };
    if let Some(f) = Fmt::from_ext(format) {
        return f;
    }
    same_format(src_format, src)
}

fn same_format(src_format: Option<&str>, src: &Path) -> Fmt {
    if let Some(f) = src_format.and_then(Fmt::from_name) {
        return f;
    }
    if matches!(ext_of(src).as_str(), "heic" | "heif" | "jpg" | "jpeg") { Fmt::Jpeg } else { Fmt::Png }
}

fn quality(ops: &Value) -> u8 {
    let q = num(&ops["quality"]).unwrap_or(0.0).trunc();
    let q = if q == 0.0 { 90.0 } else { q };
    q.clamp(1.0, 100.0) as u8
}

pub struct Edited {
    pub output: PathBuf,
    pub before: u64,
    pub after: u64,
}

/// The full-size result, saved as the file picked in the Save box (ops.output) or as
/// "<name> (edited).<ext>" in ops.folder / next to the original.
pub fn edit_image(src: &Path, ops: &Value) -> Result<Edited, String> {
    let pic = open_upright(src)?;
    let (w, h) = pic.img.dimensions();
    let final_size = output_size(w, h, ops);
    let img = transform(pic.img, ops, Some(final_size));
    let fmt = target_format(ops, pic.format, src);
    let keep = !truthy(&ops["strip_metadata"]);
    let bytes = encode(&img, pic.alpha, fmt, quality(ops), pic.exif.as_deref().filter(|_| keep), pic.icc.as_deref().filter(|_| keep))?;
    let out = if let Some(o) = ops["output"].as_str().filter(|o| !o.is_empty()) {
        let mut out = PathBuf::from(o);
        if Fmt::from_ext(&ext_of(&out)).is_none() {
            let mut name = out.file_name().unwrap_or_default().to_os_string();
            name.push(format!(".{}", fmt.ext()));
            out.set_file_name(name);
        }
        save_exact(&out, &bytes)?; // the picture is already in memory, so replacing the original is safe
        out
    } else {
        let folder = match ops["folder"].as_str().filter(|f| !f.is_empty()) {
            Some(f) => PathBuf::from(f),
            None => src.parent().map(Path::to_path_buf).unwrap_or_default(),
        };
        let stem = src.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        save_new(&folder, &format!("{stem} (edited).{}", fmt.ext()), &bytes)?
    };
    let before = std::fs::metadata(src).map(|m| m.len()).unwrap_or(0);
    Ok(Edited { output: out, before, after: bytes.len() as u64 })
}

// ---------------------------------------------------------------- live preview

struct Cached {
    mtime: SystemTime,
    small: Arc<RgbaImage>,
    alpha: bool,
    full: (u32, u32),
    exif_len: usize,
    format: Option<&'static str>,
}

static CACHE: LazyLock<Mutex<HashMap<PathBuf, Cached>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// Pillow's thumbnail() size: fits in max x max, keeps the shape as closely as whole pixels allow.
fn thumb_size(w: u32, h: u32, max: u32) -> (u32, u32) {
    if w <= max && h <= max {
        return (w, h);
    }
    let (x, y) = (max as f64, max as f64);
    let aspect = w as f64 / h as f64;
    let round_aspect = |number: f64, key: &dyn Fn(f64) -> f64| -> f64 {
        let (lo, hi) = (number.floor(), number.ceil());
        (if key(hi) < key(lo) { hi } else { lo }).max(1.0)
    };
    if x / y >= aspect {
        let nx = round_aspect(y * aspect, &|n| (aspect - n / y).abs());
        (nx as u32, y as u32)
    } else {
        let ny = round_aspect(x / aspect, &|n| if n == 0.0 { 0.0 } else { (aspect - x / n).abs() });
        (x as u32, ny as u32)
    }
}

/// A small upright copy, kept so dragging sliders doesn't decode a 12 MP photo each time.
fn preview_base(src: &Path) -> Result<(Arc<RgbaImage>, bool, (u32, u32), usize, Option<&'static str>), String> {
    let mtime = std::fs::metadata(src).and_then(|m| m.modified()).map_err(|_| "Swiftlane couldn't read this file.".to_string())?;
    if let Some(c) = CACHE.lock().unwrap().get(src).filter(|c| c.mtime == mtime) {
        return Ok((c.small.clone(), c.alpha, c.full, c.exif_len, c.format));
    }
    let pic = open_upright(src)?;
    let full = pic.img.dimensions();
    let (tw, th) = thumb_size(full.0, full.1, PREVIEW_MAX);
    let small = Arc::new(if (tw, th) == full { pic.img } else { imageops::thumbnail(&pic.img, tw, th) });
    let exif_len = pic.exif.as_deref().and_then(exif::for_copy).map(|e| e.len()).unwrap_or(0);
    let mut cache = CACHE.lock().unwrap();
    if cache.len() > 8 {
        cache.clear();
    }
    cache.insert(src.to_path_buf(), Cached { mtime, small: small.clone(), alpha: pic.alpha, full, exif_len, format: pic.format });
    Ok((small, pic.alpha, full, exif_len, pic.format))
}

fn data_url(fmt: Fmt, bytes: &[u8]) -> String {
    format!("data:{};base64,{}", fmt.mime(), base64::engine::general_purpose::STANDARD.encode(bytes))
}

/// The editor's starting picture: upright, at most 1400 px, plus the real size.
pub fn preview_source(src: &Path) -> Result<Value, String> {
    let (small, alpha, full, _, format) = preview_base(src)?;
    let fmt = if alpha { Fmt::Png } else { Fmt::Jpeg };
    let bytes = if alpha {
        let mut buf = Vec::new();
        image::codecs::png::PngEncoder::new_with_quality(&mut buf, image::codecs::png::CompressionType::Fast, image::codecs::png::FilterType::Adaptive)
            .write_image(small.as_raw(), small.width(), small.height(), image::ExtendedColorType::Rgba8)
            .map_err(|_| "The preview couldn't be made.".to_string())?;
        buf
    } else {
        encode(&small, false, Fmt::Jpeg, 90, None, None)?
    };
    let format = format.map(String::from).unwrap_or_else(|| ext_of(src).to_uppercase());
    Ok(json!({
        "data": data_url(fmt, &bytes),
        "width": full.0, "height": full.1,
        "preview": [small.width(), small.height()],
        "bytes": std::fs::metadata(src).map(|m| m.len()).unwrap_or(0),
        "format": format,
    }))
}

/// The result as it will look (compression included), at preview size, plus an estimate of the
/// file size.
pub fn render_preview(src: &Path, ops: &Value) -> Result<Value, String> {
    let (small, alpha, full, exif_len, format) = preview_base(src)?;
    let final_size = output_size(full.0, full.1, ops);
    let scale = small.width() as f64 / full.0 as f64;
    let preview_final = (pyround(final_size.0 as f64 * scale).max(1.0) as u32, pyround(final_size.1 as f64 * scale).max(1.0) as u32);
    let img = transform((*small).clone(), ops, Some(preview_final));
    let fmt = target_format(ops, format, src);
    let encoded = encode(&img, alpha, fmt, quality(ops), None, None)?;
    // file size grows roughly with the number of pixels: scale the preview's size up
    let mut estimate = encoded.len() as f64 * (final_size.0 as f64 * final_size.1 as f64) / (img.width() as f64 * img.height() as f64).max(1.0);
    if !truthy(&ops["strip_metadata"]) {
        estimate += exif_len as f64;
    }
    Ok(json!({
        "data": data_url(fmt, &encoded),
        "width": final_size.0, "height": final_size.1,
        "estimate": estimate as u64,
        "format": fmt.ext().to_uppercase(),
    }))
}

// ---------------------------------------------------------------- saving from the page

fn kb(n: u64) -> String {
    if n < 1024 * 1024 { format!("{:.0} KB", n as f64 / 1024.0) } else { format!("{:.1} MB", n as f64 / 1024.0 / 1024.0) }
}

/// Asks where to save (a file for one image, a folder for several), then saves in the background.
/// Returns the job id, or null if the dialog was cancelled.
fn edit_images(app: &AppHandle, paths: &Value, ops: &Value) -> Value {
    let paths: Vec<PathBuf> = paths.as_array().map(|a| a.iter().filter_map(|p| p.as_str()).map(PathBuf::from).collect()).unwrap_or_default();
    if paths.is_empty() {
        return Value::Null;
    }
    let mut ops = if ops.is_object() { ops.clone() } else { json!({}) };
    let parent = |p: &Path| p.parent().map(Path::to_path_buf).unwrap_or_default();
    if paths.len() == 1 {
        let src = &paths[0];
        let ext = target_format(&ops, sniff_format(src), src).ext();
        let exts: Vec<&str> = std::iter::once(ext).chain(["jpg", "png", "webp"].into_iter().filter(|e| *e != ext)).collect();
        let stem = src.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        let Some(out) = ask_save(app, &parent(src), &format!("{stem} (edited).{ext}"), &exts) else { return Value::Null };
        ops["output"] = json!(out.to_string_lossy());
    } else {
        let Some(folder) = ask_folder(app, &parent(&paths[0])) else { return Value::Null };
        ops["folder"] = json!(folder.to_string_lossy());
    }
    let n = paths.len();
    let title = format!("Edit {n} image{}", if n != 1 { "s" } else { "" });
    let id = app.state::<crate::api::AppState>().jobs.start(app, "images", &title, true, move |ctx| {
        let mut outs = Vec::new();
        for (i, p) in paths.iter().enumerate() {
            ctx.check()?;
            let name = p.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
            let r = edit_image(p, &ops).map_err(|e| JobError::Failed(if n > 1 { format!("{name}: {e}") } else { e }))?;
            ctx.report(Some((i + 1) as f64 / n as f64), Some(&format!("{} of {n} · {name}: {} → {}", i + 1, kb(r.before), kb(r.after))));
            outs.push(r.output);
        }
        Ok(outs)
    });
    json!(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh folder for one test, removed afterwards. Nothing is written anywhere else.
    struct TempDir(PathBuf);
    impl TempDir {
        fn new(tag: &str) -> Self {
            let mut r = [0u8; 8];
            getrandom::getrandom(&mut r).unwrap();
            let dir = std::env::temp_dir().join(format!("swl-img-{tag}-{}", hex::encode(r)));
            std::fs::create_dir(&dir).unwrap();
            TempDir(dir)
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// JPGs Swiftlane saves must open again in Swiftlane with the right colours (not solid green).
    #[test]
    fn saved_jpg_reopens_with_its_colours() {
        let img = RgbaImage::from_fn(64, 48, |x, y| image::Rgba([(x * 4) as u8, (y * 5) as u8, 200, 255]));
        let bytes = encode(&img, false, Fmt::Jpeg, 90, None, None).unwrap();
        let back = image::load_from_memory(&bytes).unwrap().to_rgba8();
        for (x, y) in [(2, 2), (60, 2), (2, 45), (60, 45), (32, 24)] {
            let (a, b) = (img.get_pixel(x, y), back.get_pixel(x, y));
            for c in 0..3 {
                assert!((a[c] as i32 - b[c] as i32).abs() <= 12, "pixel ({x},{y}): saved {a:?}, read back {b:?}");
            }
        }
    }

    fn contains(hay: &[u8], needle: &[u8]) -> bool {
        hay.windows(needle.len()).any(|w| w == needle)
    }

    /// A 40x20 JPEG (left half red, right half blue) carrying the phone-like EXIF from exif.rs.
    fn photo_with_exif(dir: &Path, orientation: u16) -> PathBuf {
        let img = RgbaImage::from_fn(40, 20, |x, _| if x < 20 { Rgba([255, 0, 0, 255]) } else { Rgba([0, 0, 255, 255]) });
        let mut buf = Vec::new();
        let mut enc = jpeg_encoder::Encoder::new(&mut buf, 95);
        enc.add_exif_metadata(&exif::tests::sample(orientation)).unwrap();
        enc.encode(&rgb_bytes(&img), 40, 20, jpeg_encoder::ColorType::Rgb).unwrap();
        let p = dir.join("photo.jpg");
        std::fs::write(&p, buf).unwrap();
        p
    }

    #[test]
    fn sizes_follow_the_python_rules() {
        assert_eq!(output_size(4000, 3000, &json!({})), (4000, 3000));
        assert_eq!(output_size(4000, 3000, &json!({"rotate": 90})), (3000, 4000));
        assert_eq!(output_size(4000, 3000, &json!({"rotate": -90})), (3000, 4000));
        assert_eq!(output_size(4000, 3000, &json!({"crop": "1:1"})), (3000, 3000));
        assert_eq!(output_size(4000, 3000, &json!({"crop": "16:9"})), (4000, 2250));
        assert_eq!(output_size(4000, 3000, &json!({"crop_box": [0.25, 0.25, 0.5, 0.5]})), (2000, 1500));
        assert_eq!(output_size(4000, 3000, &json!({"crop_box": [0.5, 0.5, 0.9, 0.9]})), (2000, 1500)); // kept inside
        assert_eq!(output_size(4000, 3000, &json!({"resize": {"mode": "percent", "value": 50}})), (2000, 1500));
        assert_eq!(output_size(4000, 3000, &json!({"resize": {"mode": "longest", "value": 1000}})), (1000, 750));
        assert_eq!(output_size(4000, 3000, &json!({"resize": {"width": 800}})), (800, 600));
        assert_eq!(output_size(4000, 3000, &json!({"resize": {"height": "300"}})), (400, 300));
        // straightening zooms in so no corners are empty
        let s = straighten_scale(10.0, 4000.0, 3000.0);
        assert!((s - 1.2163).abs() < 1e-3, "{s}");
        assert_eq!(output_size(4000, 3000, &json!({"straighten": 10})), (pyround(4000.0 / s) as u32, pyround(3000.0 / s) as u32));
        assert_eq!(straighten_scale(0.0, 10.0, 10.0), 1.0);
        // Python's round(): halves to even
        assert_eq!(output_size(5, 5, &json!({"resize": {"mode": "percent", "value": 50}})), (2, 2));
    }

    #[test]
    fn pixels_match_the_sizes() {
        let img = RgbaImage::from_pixel(300, 200, Rgba([10, 200, 30, 255]));
        for ops in [
            json!({"rotate": 90, "flip_h": true, "straighten": 7.5, "crop_box": [0.1, 0.2, 0.5, 0.6], "resize": {"mode": "percent", "value": 70}}),
            json!({"crop": "4:5", "straighten": -3}),
            json!({"rotate": 270, "adjust": {"brightness": 1.2, "contrast": 0.8, "saturation": 1.5, "sharpness": 2}}),
        ] {
            let want = output_size(300, 200, &ops);
            let out = transform(img.clone(), &ops, Some(want));
            assert_eq!(out.dimensions(), want, "{ops}");
        }
    }

    #[test]
    fn rotate_and_flip_move_pixels_the_right_way() {
        let mut img = RgbaImage::from_pixel(2, 1, Rgba([0, 0, 0, 255]));
        img.put_pixel(0, 0, Rgba([255, 255, 255, 255])); // white on the left
        let r = transform(img.clone(), &json!({"rotate": 90}), None);
        assert_eq!(r.dimensions(), (1, 2));
        assert_eq!(r.get_pixel(0, 0)[0], 255); // clockwise: left goes to the top
        let f = transform(img, &json!({"flip_h": true}), None);
        assert_eq!(f.get_pixel(1, 0)[0], 255);
    }

    #[test]
    fn colour_adjustments() {
        let img = RgbaImage::from_pixel(4, 4, Rgba([100, 50, 200, 128]));
        let b = transform(img.clone(), &json!({"adjust": {"brightness": 1.5}}), None);
        assert_eq!(*b.get_pixel(0, 0), Rgba([150, 75, 255, 128])); // alpha untouched
        let grey = transform(img.clone(), &json!({"adjust": {"saturation": 0}}), None);
        let p = grey.get_pixel(1, 1);
        assert!(p[0] == p[1] && p[1] == p[2]);
        let flat = transform(img, &json!({"adjust": {"contrast": 0}}), None);
        let p = flat.get_pixel(2, 2);
        assert!(p[0] == p[1] && p[1] == p[2]);
    }

    #[test]
    fn formats() {
        let src = Path::new("C:/x/photo.HEIC");
        assert_eq!(target_format(&json!({}), None, src), Fmt::Jpeg);
        assert_eq!(target_format(&json!({}), Some("WEBP"), src), Fmt::Webp);
        assert_eq!(target_format(&json!({"format": "png"}), Some("JPEG"), src), Fmt::Png);
        assert_eq!(target_format(&json!({"format": "png", "output": "C:/y/out.webp"}), Some("JPEG"), src), Fmt::Webp);
        assert_eq!(target_format(&json!({"format": "keep"}), Some("GIF"), Path::new("a.gif")), Fmt::Png);
        assert_eq!(quality(&json!({})), 90);
        assert_eq!(quality(&json!({"quality": 250})), 100);
        assert_eq!(thumb_size(4000, 3000, 1400), (1400, 1050));
        assert_eq!(thumb_size(3000, 4000, 1400), (1050, 1400));
        assert_eq!(thumb_size(800, 600, 1400), (800, 600));
        assert!(ffmpeg_version_ok("7.1.1-full_build-www.gyan.dev"));
        assert!(ffmpeg_version_ok("n8.0"));
        assert!(ffmpeg_version_ok("N-118000-g1234"));
        assert!(ffmpeg_version_ok("2025-01-02-git-abc"));
        assert!(!ffmpeg_version_ok("6.1.1-3ubuntu5"));
        assert!(!ffmpeg_version_ok("7.0"));
    }

    #[test]
    fn opening_turns_the_photo_upright() {
        let dir = TempDir::new("upright");
        let p = photo_with_exif(&dir.0, 6); // "turn 90° clockwise to view"
        let pic = open_upright(&p).unwrap();
        assert_eq!(pic.img.dimensions(), (20, 40));
        assert_eq!(pic.format, Some("JPEG"));
        assert!(!pic.alpha);
        assert!(pic.exif.is_some());
        // red (the left half) is now at the top
        let top = pic.img.get_pixel(10, 2);
        assert!(top[0] > 200 && top[2] < 60, "{top:?}");
    }

    #[test]
    fn removing_metadata_really_removes_it_and_keeps_the_original() {
        let dir = TempDir::new("strip");
        let p = photo_with_exif(&dir.0, 6);
        let original = std::fs::read(&p).unwrap();
        assert!(contains(&original, b"SecretPhone"));

        // kept: camera and location stay, orientation becomes normal, the hidden thumbnail goes
        let kept = edit_image(&p, &json!({"format": "keep"})).unwrap();
        assert_eq!(kept.output, dir.0.join("photo (edited).jpg"));
        let bytes = std::fs::read(&kept.output).unwrap();
        assert!(contains(&bytes, b"SecretPhone"));
        assert!(!contains(&bytes, b"THUMBNAIL-SECRET"));
        let mut dec = image::codecs::jpeg::JpegDecoder::new(std::io::Cursor::new(&bytes)).unwrap();
        assert_eq!(dec.orientation().unwrap(), image::metadata::Orientation::NoTransforms);
        assert_eq!(image::load_from_memory(&bytes).unwrap().into_rgba8().dimensions(), (20, 40));

        // removed, in every format: no EXIF at all
        for (format, ext) in [("jpg", "jpg"), ("png", "png"), ("webp", "webp")] {
            let r = edit_image(&p, &json!({"format": format, "strip_metadata": true, "quality": 80})).unwrap();
            assert_eq!(r.output.extension().unwrap(), ext);
            let bytes = std::fs::read(&r.output).unwrap();
            assert!(!contains(&bytes, b"SecretPhone"), "{format}");
            assert!(!contains(&bytes, b"Exif") && !contains(&bytes, b"eXIf") && !contains(&bytes, b"EXIF"), "{format}");
            let back = image::load_from_memory(&bytes).unwrap();
            assert_eq!((back.width(), back.height()), (20, 40), "{format}");
        }
        // the original is untouched, and nothing overwrote anything
        assert_eq!(std::fs::read(&p).unwrap(), original);
        assert!(dir.0.join("photo (edited) (1).jpg").exists());
    }

    #[test]
    fn webp_and_png_keep_metadata_when_asked() {
        let dir = TempDir::new("keep");
        let p = photo_with_exif(&dir.0, 1);
        for format in ["png", "webp"] {
            let r = edit_image(&p, &json!({"format": format})).unwrap();
            let bytes = std::fs::read(&r.output).unwrap();
            assert!(contains(&bytes, b"SecretPhone"), "{format}");
            let mut dec = ImageReader::new(std::io::Cursor::new(&bytes)).with_guessed_format().unwrap().into_decoder().unwrap();
            let e = dec.exif_metadata().unwrap().expect(format);
            assert!(contains(&e, b"SecretPhone"));
        }
    }

    #[test]
    fn exact_output_and_transparency() {
        let dir = TempDir::new("exact");
        let p = dir.0.join("logo.png");
        let mut img = RgbaImage::from_pixel(10, 10, Rgba([0, 0, 0, 0]));
        img.put_pixel(5, 5, Rgba([255, 0, 0, 255]));
        img.save(&p).unwrap();
        // a name without a picture extension gets one; transparency survives PNG/WebP
        let out = dir.0.join("sub").join("chosen");
        let r = edit_image(&p, &json!({"output": out.to_string_lossy(), "format": "webp"})).unwrap();
        assert_eq!(r.output, dir.0.join("sub").join("chosen.webp"));
        let back = image::open(&r.output).unwrap().into_rgba8();
        assert!(back.get_pixel(0, 0)[3] < 10);
        // JPG puts it on white
        let r = edit_image(&p, &json!({"output": dir.0.join("w.jpg").to_string_lossy()})).unwrap();
        let back = image::open(&r.output).unwrap().into_rgb8();
        assert!(back.get_pixel(0, 0).0.iter().all(|c| *c > 240));
        // saving over the original (picked in the Save box) works and leaves no temp file
        let r = edit_image(&p, &json!({"output": p.to_string_lossy(), "rotate": 90})).unwrap();
        assert_eq!(r.output, p);
        let names: Vec<_> = std::fs::read_dir(&dir.0).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert!(!names.iter().any(|n| n.to_string_lossy().contains("swiftlane-part")));
    }

    #[test]
    fn preview_and_render() {
        let dir = TempDir::new("preview");
        let p = dir.0.join("big.png");
        image::RgbImage::from_fn(2000, 1000, |x, y| image::Rgb([(x % 256) as u8, (y % 256) as u8, 90])).save(&p).unwrap();
        let v = preview_source(&p).unwrap();
        assert_eq!((v["width"].as_u64(), v["height"].as_u64()), (Some(2000), Some(1000)));
        assert_eq!(v["preview"], json!([1400, 700]));
        assert_eq!(v["format"], "PNG");
        assert!(v["data"].as_str().unwrap().starts_with("data:image/jpeg;base64,"));
        let r = render_preview(&p, &json!({"straighten": 5, "crop_box": [0, 0, 0.5, 0.5], "format": "webp", "quality": 70})).unwrap();
        assert_eq!(r["format"], "WEBP");
        assert!(r["data"].as_str().unwrap().starts_with("data:image/webp;base64,"));
        let want = output_size(2000, 1000, &json!({"straighten": 5, "crop_box": [0, 0, 0.5, 0.5]}));
        assert_eq!((r["width"].as_u64().unwrap() as u32, r["height"].as_u64().unwrap() as u32), want);
        assert!(r["estimate"].as_u64().unwrap() > 0);
        assert!(preview_source(&dir.0.join("missing.png")).is_err());
        std::fs::write(dir.0.join("fake.jpg"), b"not a picture").unwrap();
        assert!(preview_source(&dir.0.join("fake.jpg")).is_err());
    }

    #[test]
    fn avif_opens_through_ffmpeg_when_it_is_there() {
        let Some(ff) = crate::tools::program("ffmpeg") else { return };
        let dir = TempDir::new("avif");
        let png = dir.0.join("a.png");
        image::RgbImage::from_pixel(64, 32, image::Rgb([200, 40, 40])).save(&png).unwrap();
        let avif = dir.0.join("a.avif");
        let made = crate::tools::command(&ff).args(["-v", "error", "-y", "-i"]).arg(&png).arg(&avif).status();
        if !made.is_ok_and(|s| s.success()) {
            return; // this FFmpeg can't write AVIF
        }
        let pic = open_upright(&avif).unwrap();
        assert_eq!(pic.img.dimensions(), (64, 32));
        assert_eq!(pic.format, Some("AVIF"));
        assert!(!pic.alpha);
    }
}
