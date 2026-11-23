//! The file converter (ported from Source/toolkit/convert.py and Api.convert in
//! Source/swiftlane.py): pictures (the image crate, libwebp, FFmpeg for AVIF), video and audio
//! (the FFmpeg helper program), PDF pages to pictures (PDFium, shared with the PDF tools).
//!
//! Page call: convert(items, options) with items [{path, target}] and options {quality,
//! combine_pdf}. It asks where to save first (a file name for one file, a folder for several, a
//! file name for "all pictures in one PDF"), then starts one job per file. Returns the job ids,
//! or null if a dialog was cancelled.
//!
//! Outputs never replace anything unless you pick an existing name in the Save box (Windows asks
//! first); the original a file is made from can never be replaced. Without a Save box, outputs
//! go next to the original as "<name>.<ext>", "<name> (1).<ext>", ...
//! Converted files are made on this PC from your own files, so (like the Python version) they
//! get no "downloaded from the internet" mark.

use image::codecs::gif::{GifDecoder, GifEncoder, Repeat};
use image::codecs::webp::WebPDecoder;
use image::{imageops, AnimationDecoder, ExtendedColorType, ImageEncoder, ImageFormat, ImageReader, Rgba, RgbaImage};
use lopdf::{Dictionary, Document, Object, Stream};
use pdfium_render::prelude::PdfRenderConfig;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tauri::{AppHandle, Manager};

use super::images::{self, Fmt};
use super::{arg, ask_folder, ask_save, webp};
use crate::jobs::JobError;

pub const IMAGE_IN: [&str; 12] = ["jpg", "jpeg", "png", "webp", "gif", "bmp", "tif", "tiff", "ico", "avif", "heic", "heif"];
pub const VIDEO_IN: [&str; 12] = ["mp4", "mov", "mkv", "avi", "webm", "m4v", "wmv", "flv", "3gp", "mpg", "mpeg", "ts"];
pub const AUDIO_IN: [&str; 10] = ["mp3", "wav", "m4a", "aac", "flac", "ogg", "opus", "wma", "aiff", "aif"];
pub const PDF_IN: [&str; 1] = ["pdf"];

pub const IMAGE_OUT: [&str; 9] = ["jpg", "png", "webp", "gif", "bmp", "tiff", "ico", "avif", "pdf"];
pub const VIDEO_OUT: [&str; 6] = ["mp4", "mov", "mkv", "avi", "webm", "gif"];
pub const AUDIO_OUT: [&str; 7] = ["mp3", "wav", "m4a", "aac", "flac", "ogg", "opus"];
pub const PDF_OUT: [&str; 2] = ["jpg", "png"];

const NO_FFMPEG: &str = "Converting video and audio needs the FFmpeg helper program next to Swiftlane.";
const NO_FFMPEG_AVIF: &str = "Saving AVIF pictures needs the FFmpeg helper program next to Swiftlane.";
const SAME_FILE: &str = "Choose a different name: the converted file can't replace the original it's made from.";
const DAMAGED: &str = "This picture seems to be damaged, so Swiftlane can't open it.";
const PDF_SCALE: f32 = 2.0; // 144 dpi, as in Python
const MAX_RENDER_PIXELS: f32 = 25_000_000.0; // one drawn PDF page, whatever size the PDF claims
const MAX_ANIMATION_BYTES: usize = 1 << 30; // all frames of an animation, decoded

/// A quality preset: video CRF for x264, audio bitrate, JPEG/WebP quality.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Quality {
    pub crf: u32,
    pub abr: &'static str,
    pub image: u8,
}

pub fn quality(name: Option<&str>) -> Quality {
    match name {
        Some("high") => Quality { crf: 18, abr: "320k", image: 95 },
        Some("small") => Quality { crf: 28, abr: "128k", image: 75 },
        _ => Quality { crf: 23, abr: "192k", image: 88 },
    }
}

pub fn dispatch(app: &AppHandle, method: &str, args: &[Value]) -> Option<Value> {
    Some(match method {
        "convert" => convert(app, arg(args, 0), arg(args, 1)),
        _ => return None,
    })
}

// ---------------------------------------------------------------- formats

fn ext_of(path: &Path) -> String {
    path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default()
}

/// "image", "video", "audio", "pdf", or None, from the file's extension.
pub fn kind_of(path: &Path) -> Option<&'static str> {
    let ext = ext_of(path);
    let lists: [(&'static str, &[&str]); 4] = [("image", &IMAGE_IN), ("video", &VIDEO_IN), ("audio", &AUDIO_IN), ("pdf", &PDF_IN)];
    lists.iter().find(|(_, exts)| exts.contains(&ext.as_str())).map(|(k, _)| *k)
}

/// What a kind of file can become.
pub fn targets_for(kind: Option<&str>) -> Vec<&'static str> {
    match kind {
        Some("image") => IMAGE_OUT.to_vec(),
        Some("video") => VIDEO_OUT.iter().chain(AUDIO_OUT.iter()).copied().collect(),
        Some("audio") => AUDIO_OUT.to_vec(),
        Some("pdf") => PDF_OUT.to_vec(),
        _ => Vec::new(),
    }
}

/// The same lists as convert.supported_formats() (also in tool_lists.json for get_state).
#[allow(dead_code)]
pub fn supported_formats() -> Value {
    json!({
        "image": { "in": IMAGE_IN, "out": IMAGE_OUT },
        "video": { "in": VIDEO_IN, "out": targets_for(Some("video")) },
        "audio": { "in": AUDIO_IN, "out": AUDIO_OUT },
        "pdf": { "in": PDF_IN, "out": PDF_OUT },
    })
}

// ---------------------------------------------------------------- the page call

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

fn parent(p: &Path) -> PathBuf {
    p.parent().map(Path::to_path_buf).unwrap_or_default()
}

fn stem(p: &Path) -> String {
    p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default()
}

fn file_name(p: &Path) -> String {
    p.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default()
}

/// What one conversion needs besides the file and target.
#[derive(Clone, Debug)]
pub struct Opts {
    pub quality: Quality,
    pub folder: Option<PathBuf>, // the folder picked for several files
    pub output: Option<PathBuf>, // the exact file picked in the Save box
}

/// Asks where to save, then starts the jobs. Returns the job ids, or null if cancelled.
fn convert(app: &AppHandle, items: &Value, options: &Value) -> Value {
    let items: Vec<(String, String)> = items
        .as_array()
        .map(|a| a.iter().filter_map(|i| Some((i["path"].as_str()?.to_string(), i["target"].as_str().unwrap_or("").to_string()))).collect())
        .unwrap_or_default();
    let combine_pdf = truthy(&options["combine_pdf"]);
    let q = quality(options["quality"].as_str());
    let to_pdf: Vec<String> = items.iter().filter(|(p, t)| t == "pdf" && kind_of(Path::new(p)) == Some("image")).map(|(p, _)| p.clone()).collect();
    let combined = combine_pdf && to_pdf.len() > 1;
    let mut rest: Vec<(String, String, Option<PathBuf>)> = items.into_iter().filter(|(p, _)| !(combined && to_pdf.contains(p))).map(|(p, t)| (p, t, None)).collect();

    // Ask first, start converting only once every question is answered.
    let mut pdf_out = None;
    if combined {
        let first = PathBuf::from(&to_pdf[0]);
        let Some(out) = ask_save(app, &parent(&first), &combined_name(&first, to_pdf.len()), &["pdf"]) else { return Value::Null };
        pdf_out = Some(out);
    }
    let mut folder = None;
    let single = rest.len() == 1 && kind_of(Path::new(&rest[0].0)) != Some("pdf");
    if single {
        let src = PathBuf::from(&rest[0].0);
        let target = rest[0].1.clone();
        let Some(out) = ask_save(app, &parent(&src), &format!("{}.{target}", stem(&src)), &[target.as_str()]) else { return Value::Null };
        rest[0].2 = Some(out);
    } else if !rest.is_empty() {
        let Some(f) = ask_folder(app, &parent(Path::new(&rest[0].0))) else { return Value::Null };
        folder = Some(f);
    }

    let st = app.state::<crate::api::AppState>();
    let jobs = &st.jobs;
    let mut ids = Vec::new();
    if let Some(out) = pdf_out {
        let paths: Vec<PathBuf> = to_pdf.iter().map(PathBuf::from).collect();
        let title = format!("{} images → one PDF", paths.len());
        ids.push(jobs.start(app, "convert", &title, true, move |ctx| Ok(vec![images_to_pdf(&paths, &out, q.image, &|| ctx.check())?])));
    }
    for (src, target, output) in rest {
        let src = PathBuf::from(src);
        let title = format!("{} → {}", file_name(&src), target.to_uppercase());
        let opts = Opts { quality: q, folder: folder.clone(), output };
        ids.push(jobs.start(app, "convert", &title, true, move |ctx| convert_file(&src, &target, &opts, &|f| ctx.report(Some(f), None), &|| ctx.check())));
    }
    json!(ids)
}

fn combined_name(first: &Path, n: usize) -> String {
    if n > 1 { format!("{} + {} more.pdf", stem(first), n - 1) } else { format!("{}.pdf", stem(first)) }
}

// ---------------------------------------------------------------- converting one file

/// Converts `src` to `target`; returns the files made.
pub fn convert_file(src: &Path, target: &str, opts: &Opts, progress: &dyn Fn(f64), check: &dyn Fn() -> Result<(), JobError>) -> Result<Vec<PathBuf>, JobError> {
    let target = target.trim_start_matches('.').to_lowercase();
    let kind = kind_of(src);
    if !targets_for(kind).contains(&target.as_str()) {
        let suffix = src.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_else(|| "this file".into());
        return Err(JobError::Failed(format!("Can't convert {suffix} to .{target}")));
    }
    let out_dir = opts.folder.clone().unwrap_or_else(|| parent(src));
    if kind == Some("pdf") {
        return pdf_pages(src, &target, &out_dir, progress, check);
    }
    let out = match &opts.output {
        Some(o) => Some(exact_output(o, Some(src), &target)?),
        None => None,
    };
    if kind == Some("image") {
        return Ok(vec![image(src, &target, &out_dir, opts.quality.image, out)?]);
    }
    Ok(vec![media(src, &target, &out_dir, &opts.quality, out, progress, check)?])
}

/// The file picked in the Save box, with the target's extension added if it was left off. It
/// can't be the original itself.
pub fn exact_output(path: &Path, src: Option<&Path>, target: &str) -> Result<PathBuf, String> {
    let mut out = path.to_path_buf();
    if ext_of(&out) != target || out.extension().is_none() {
        let mut name = out.file_name().unwrap_or_default().to_os_string();
        name.push(format!(".{target}"));
        out.set_file_name(name);
    }
    if let Some(src) = src {
        if let (Ok(a), Ok(b)) = (std::fs::canonicalize(src), std::fs::canonicalize(&out)) {
            if a == b {
                return Err(SAME_FILE.into());
            }
        }
    }
    let folder = parent(&out);
    if !folder.as_os_str().is_empty() {
        std::fs::create_dir_all(&folder).map_err(|e| io_message(&e))?;
    }
    Ok(out)
}

fn io_message(e: &std::io::Error) -> String {
    match e.kind() {
        std::io::ErrorKind::PermissionDenied => "Swiftlane isn't allowed to save in that folder. Pick another one.".into(),
        std::io::ErrorKind::StorageFull => "The disk is full, so the converted file couldn't be saved.".into(),
        _ => format!("The converted file couldn't be saved ({e})."),
    }
}

/// "name", "stem (1).ext", "stem (2).ext", ... (Python's free_path).
pub fn numbered(name: &str, n: usize) -> String {
    if n == 0 {
        return name.to_string();
    }
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    format!("{stem} ({n}){ext}")
}

/// Claims folder/name (or a numbered name if it's taken) by creating it empty, so nothing else
/// can take it. Never replaces anything.
fn reserve(folder: &Path, name: &str) -> Result<(PathBuf, std::fs::File), String> {
    std::fs::create_dir_all(folder).map_err(|e| io_message(&e))?;
    for n in 0..10_000 {
        let candidate = folder.join(numbered(name, n));
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&candidate) {
            Ok(f) => return Ok((candidate, f)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(io_message(&e)),
        }
    }
    Err("There's no free file name left in that folder.".into())
}

/// Saves `bytes` under a new name in `folder` (see `reserve`).
fn save_new(folder: &Path, name: &str, bytes: &[u8]) -> Result<PathBuf, String> {
    let (path, mut f) = reserve(folder, name)?;
    if let Err(e) = f.write_all(bytes).and_then(|_| f.sync_all()) {
        drop(f);
        let _ = std::fs::remove_file(&path);
        return Err(io_message(&e));
    }
    Ok(path)
}

/// Writes exactly `path` (picked in the Save box; Windows already asked before replacing it).
/// Written next to it first, then swapped in, so the old file survives any failure.
fn save_exact(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut rnd = [0u8; 6];
    let _ = getrandom::getrandom(&mut rnd);
    let tmp = path.with_file_name(format!(".{}.{}.swiftlane-part", file_name(path), hex::encode(rnd)));
    let result = std::fs::write(&tmp, bytes).and_then(|_| std::fs::rename(&tmp, path));
    if let Err(e) = result {
        let _ = std::fs::remove_file(&tmp);
        return Err(io_message(&e));
    }
    Ok(())
}

fn save(out: Option<PathBuf>, folder: &Path, name: &str, bytes: &[u8]) -> Result<PathBuf, String> {
    match out {
        Some(o) => save_exact(&o, bytes).map(|_| o),
        None => save_new(folder, name, bytes),
    }
}

// ---------------------------------------------------------------- pictures

fn image(src: &Path, target: &str, out_dir: &Path, q: u8, out: Option<PathBuf>) -> Result<PathBuf, String> {
    let ext = match target {
        "jpeg" => "jpg",
        "tif" => "tiff",
        t => t,
    };
    let name = format!("{}.{ext}", stem(src));
    if matches!(ext, "gif" | "webp") {
        if let Some(frames) = animation(src)? {
            // keep animations
            let bytes = if ext == "gif" { animated_gif(frames)? } else { animated_webp(&frames, q as f32)? };
            return save(out, out_dir, &name, &bytes);
        }
    }
    let pic = images::open_upright(src)?;
    if ext == "avif" {
        return avif(&pic, out_dir, &name, out);
    }
    let bytes = encode_still(&pic, ext, q)?;
    save(out, out_dir, &name, &bytes)
}

/// A still picture in one of the formats written here (not AVIF).
fn encode_still(pic: &images::Picture, ext: &str, q: u8) -> Result<Vec<u8>, String> {
    let img = &pic.img;
    let (w, h) = img.dimensions();
    let exif = pic.exif.as_deref();
    let icc = pic.icc.as_deref();
    let fail = |what: &str| format!("The picture couldn't be saved as {what}.");
    Ok(match ext {
        // EXIF is kept (turned upright) for JPG, PNG and WebP, as Python did
        "jpg" => images::encode(img, pic.alpha, Fmt::Jpeg, q, exif, icc)?,
        "png" => images::encode(img, pic.alpha, Fmt::Png, q, exif, icc)?,
        "webp" => images::encode(img, pic.alpha, Fmt::Webp, q, exif, icc)?,
        "gif" => {
            if w > 65_535 || h > 65_535 {
                return Err("GIF pictures can be at most 65535 pixels wide and tall.".into());
            }
            let mut buf = Vec::new();
            GifEncoder::new_with_speed(&mut buf, 10).write_image(img.as_raw(), w, h, ExtendedColorType::Rgba8).map_err(|_| fail("GIF"))?;
            buf
        }
        "bmp" => {
            let mut buf = Vec::new();
            image::codecs::bmp::BmpEncoder::new(&mut buf).write_image(&flat_rgb(img, pic.alpha), w, h, ExtendedColorType::Rgb8).map_err(|_| fail("BMP"))?;
            buf
        }
        "tiff" => {
            let mut buf = std::io::Cursor::new(Vec::new());
            let enc = image::codecs::tiff::TiffEncoder::new(&mut buf);
            let result = if pic.alpha { enc.write_image(img.as_raw(), w, h, ExtendedColorType::Rgba8) } else { enc.write_image(&rgb(img), w, h, ExtendedColorType::Rgb8) };
            result.map_err(|_| fail("TIFF"))?;
            buf.into_inner()
        }
        "ico" => ico(img)?,
        "pdf" => {
            let jpeg = images::encode(img, pic.alpha, Fmt::Jpeg, q, None, None)?;
            pdf_of_pictures(vec![(jpeg, w, h)], 72.0)?
        }
        _ => return Err(format!("Can't save pictures as .{ext}")),
    })
}

fn rgb(img: &RgbaImage) -> Vec<u8> {
    img.pixels().flat_map(|p| [p[0], p[1], p[2]]).collect()
}

/// RGB with transparency turned white (for JPG/PDF/BMP).
fn flat_rgb(img: &RgbaImage, alpha: bool) -> Vec<u8> {
    if alpha { images::on_white(img) } else { rgb(img) }
}

/// The sizes an icon gets: 256, 128, 64, 48, 32, 16 (no bigger than the picture, but always 16).
pub fn ico_sizes(w: u32, h: u32) -> Vec<u32> {
    let side = w.max(h).max(16);
    [256, 128, 64, 48, 32, 16].into_iter().filter(|s| *s <= side).collect()
}

/// An icon: the picture centred on a transparent square, at each size.
fn ico(img: &RgbaImage) -> Result<Vec<u8>, String> {
    let (w, h) = img.dimensions();
    let side = w.max(h).max(1) as f64;
    let mut frames = Vec::new();
    for s in ico_sizes(w, h) {
        let fw = ((w as f64 * s as f64 / side).round() as u32).clamp(1, s);
        let fh = ((h as f64 * s as f64 / side).round() as u32).clamp(1, s);
        let small = imageops::resize(img, fw, fh, imageops::FilterType::Lanczos3);
        let mut square = RgbaImage::from_pixel(s, s, Rgba([0, 0, 0, 0]));
        imageops::overlay(&mut square, &small, ((s - fw) / 2) as i64, ((s - fh) / 2) as i64);
        frames.push(image::codecs::ico::IcoFrame::as_png(square.as_raw(), s, s, ExtendedColorType::Rgba8).map_err(|_| "The picture couldn't be saved as ICO.".to_string())?);
    }
    let mut buf = Vec::new();
    image::codecs::ico::IcoEncoder::new(&mut buf).encode_images(&frames).map_err(|_| "The picture couldn't be saved as ICO.".to_string())?;
    Ok(buf)
}

/// The frames of an animated GIF or WebP, or None if it's a still picture (or another format).
fn animation(src: &Path) -> Result<Option<Vec<image::Frame>>, String> {
    let reader = ImageReader::open(src).and_then(|r| r.with_guessed_format()).map_err(|_| "Swiftlane couldn't read this file.".to_string())?;
    let format = reader.format();
    let file = || std::fs::File::open(src).map(BufReader::new).map_err(|_| "Swiftlane couldn't read this file.".to_string());
    let frames = match format {
        Some(ImageFormat::Gif) => GifDecoder::new(file()?).map_err(|_| DAMAGED.to_string())?.into_frames(),
        Some(ImageFormat::WebP) => {
            let d = WebPDecoder::new(file()?).map_err(|_| DAMAGED.to_string())?;
            if !d.has_animation() {
                return Ok(None);
            }
            d.into_frames()
        }
        _ => return Ok(None),
    };
    let mut out = Vec::new();
    let mut bytes = 0usize;
    for f in frames {
        let f = f.map_err(|_| DAMAGED.to_string())?;
        bytes = bytes.saturating_add(f.buffer().as_raw().len());
        if bytes > MAX_ANIMATION_BYTES {
            return Err("This animation is too big to convert.".into());
        }
        out.push(f);
    }
    Ok(if out.len() > 1 { Some(out) } else { None })
}

/// How long a frame shows, in milliseconds (0 means "as fast as possible", shown as 100 ms like Pillow's default).
fn frame_ms(f: &image::Frame) -> i32 {
    let (n, d) = f.delay().numer_denom_ms();
    let ms = (n as f64 / d.max(1) as f64).round();
    if ms < 1.0 { 100 } else { ms.min(i32::MAX as f64) as i32 }
}

fn animated_gif(frames: Vec<image::Frame>) -> Result<Vec<u8>, String> {
    let fail = || "The animation couldn't be saved as GIF.".to_string();
    let (w, h) = frames[0].buffer().dimensions();
    if w > 65_535 || h > 65_535 {
        return Err("GIF pictures can be at most 65535 pixels wide and tall.".into());
    }
    let mut buf = Vec::new();
    {
        let mut enc = GifEncoder::new_with_speed(&mut buf, 10);
        enc.set_repeat(Repeat::Infinite).map_err(|_| fail())?;
        enc.encode_frames(frames).map_err(|_| fail())?;
    } // the encoder writes the end of the file when it's dropped
    Ok(buf)
}

/// An animated (lossy) WebP from full-canvas frames, with libwebp's animation encoder.
fn animated_webp(frames: &[image::Frame], quality: f32) -> Result<Vec<u8>, String> {
    use libwebp_sys as w;
    let fail = || "The animation couldn't be saved as WebP.".to_string();
    let (width, height) = frames[0].buffer().dimensions();
    if width == 0 || height == 0 || width > webp::MAX_SIDE || height > webp::MAX_SIDE {
        return Err(format!("WebP pictures can be at most {} pixels wide and tall. Make it smaller or pick another format.", webp::MAX_SIDE));
    }
    // SAFETY: every pointer handed to libwebp points at memory we own for the whole call (the
    // options, the config, each frame's pixels, checked to be width*height*4 bytes); pictures are
    // freed after each frame, the assembled data is copied and then freed with WebPDataClear, and
    // the encoder is deleted on every path.
    unsafe {
        let mut opts = std::mem::MaybeUninit::<w::WebPAnimEncoderOptions>::uninit();
        if w::WebPAnimEncoderOptionsInitInternal(opts.as_mut_ptr(), w::WEBP_MUX_ABI_VERSION as _) == 0 {
            return Err(fail());
        }
        let mut opts = opts.assume_init();
        opts.anim_params.loop_count = 0; // forever
        let enc = w::WebPAnimEncoderNewInternal(width as _, height as _, &opts, w::WEBP_MUX_ABI_VERSION as _);
        if enc.is_null() {
            return Err(fail());
        }
        let result = (|| -> Result<Vec<u8>, String> {
            let config = w::WebPConfig::new_with_preset(w::WebPPreset::WEBP_PRESET_DEFAULT, quality.clamp(1.0, 100.0)).map_err(|_| fail())?;
            let mut t: i32 = 0;
            for f in frames {
                let img = f.buffer();
                if img.dimensions() != (width, height) || img.as_raw().len() != width as usize * height as usize * 4 {
                    return Err(fail());
                }
                let mut pic = w::WebPPicture::new().map_err(|_| fail())?;
                pic.use_argb = 1;
                pic.width = width as _;
                pic.height = height as _;
                if w::WebPPictureImportRGBA(&mut pic, img.as_raw().as_ptr(), (width * 4) as _) == 0 {
                    w::WebPPictureFree(&mut pic);
                    return Err(fail());
                }
                let ok = w::WebPAnimEncoderAdd(enc, &mut pic, t, &config);
                w::WebPPictureFree(&mut pic);
                if ok == 0 {
                    return Err(fail());
                }
                t = t.saturating_add(frame_ms(f));
            }
            if w::WebPAnimEncoderAdd(enc, std::ptr::null_mut(), t, std::ptr::null()) == 0 {
                return Err(fail());
            }
            let mut data = w::WebPData::default();
            if w::WebPAnimEncoderAssemble(enc, &mut data) == 0 || data.bytes.is_null() {
                w::WebPDataClear(&mut data);
                return Err(fail());
            }
            let out = std::slice::from_raw_parts(data.bytes, data.size).to_vec();
            w::WebPDataClear(&mut data);
            Ok(out)
        })();
        w::WebPAnimEncoderDelete(enc);
        result
    }
}

/// AVIF through the FFmpeg helper program (the image crate can't write it without C libraries):
/// the upright picture goes in as PNG on its standard input.
fn avif(pic: &images::Picture, out_dir: &Path, name: &str, out: Option<PathBuf>) -> Result<PathBuf, String> {
    let Some(ff) = super::program("ffmpeg") else { return Err(NO_FFMPEG_AVIF.into()) };
    let png = images::encode(&pic.img, pic.alpha, Fmt::Png, 90, None, None)?;
    let (path, reserved) = match out {
        Some(o) => (o, false),
        None => (reserve(out_dir, name)?.0, true),
    };
    let fail = |detail: &str| {
        if reserved {
            let _ = std::fs::remove_file(&path);
        }
        format!("FFmpeg couldn't save this picture as AVIF ({detail}).")
    };
    let child = super::command(&ff)
        .args(["-hide_banner", "-v", "error", "-y", "-f", "png_pipe", "-i", "pipe:0", "-frames:v", "1"])
        .arg(file_arg(&path))
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn();
    let mut child = match child {
        Ok(c) => c,
        Err(_) => return Err(fail("the helper program didn't start")),
    };
    let mut stdin = child.stdin.take();
    let writer = std::thread::spawn(move || {
        if let Some(s) = stdin.as_mut() {
            let _ = s.write_all(&png);
        }
        drop(stdin); // end of input
    });
    let result = child.wait_with_output();
    let _ = writer.join();
    match result {
        Ok(o) if o.status.success() => Ok(path),
        Ok(o) => {
            let text = String::from_utf8_lossy(&o.stderr).to_string();
            Err(fail(&last_line(&text)))
        }
        Err(_) => Err(fail("unknown error")),
    }
}

// ---------------------------------------------------------------- PDF

/// A PDF with one page per JPEG picture, `dpi` pixels per inch.
fn pdf_of_pictures(pictures: Vec<(Vec<u8>, u32, u32)>, dpi: f32) -> Result<Vec<u8>, String> {
    let mut doc = Document::with_version("1.7");
    let pages_id = doc.new_object_id();
    let mut kids = Vec::with_capacity(pictures.len());
    for (jpeg, bw, bh) in pictures {
        let (w, h) = (bw as f32 * 72.0 / dpi, bh as f32 * 72.0 / dpi);
        let mut im = Dictionary::new();
        im.set("Type", Object::Name(b"XObject".to_vec()));
        im.set("Subtype", Object::Name(b"Image".to_vec()));
        im.set("Width", bw as i64);
        im.set("Height", bh as i64);
        im.set("ColorSpace", Object::Name(b"DeviceRGB".to_vec()));
        im.set("BitsPerComponent", 8);
        im.set("Filter", Object::Name(b"DCTDecode".to_vec()));
        let im_id = doc.add_object(Stream::new(im, jpeg).with_compression(false));
        let content = doc.add_object(Stream::new(Dictionary::new(), format!("q {w:.3} 0 0 {h:.3} 0 0 cm /Im0 Do Q").into_bytes()));
        let mut xo = Dictionary::new();
        xo.set("Im0", im_id);
        let mut res = Dictionary::new();
        res.set("XObject", xo);
        let mut p = Dictionary::new();
        p.set("Type", Object::Name(b"Page".to_vec()));
        p.set("Parent", pages_id);
        p.set("MediaBox", vec![Object::Integer(0), Object::Integer(0), Object::Real(w), Object::Real(h)]);
        p.set("Resources", res);
        p.set("Contents", content);
        kids.push(Object::Reference(doc.add_object(p)));
    }
    let count = kids.len() as i64;
    let mut pages = Dictionary::new();
    pages.set("Type", Object::Name(b"Pages".to_vec()));
    pages.set("Kids", kids);
    pages.set("Count", count);
    doc.objects.insert(pages_id, Object::Dictionary(pages));
    let mut catalog = Dictionary::new();
    catalog.set("Type", Object::Name(b"Catalog".to_vec()));
    catalog.set("Pages", pages_id);
    let catalog_id = doc.add_object(catalog);
    doc.trailer.set("Root", catalog_id);
    doc.compress();
    let mut buf = Vec::new();
    doc.save_to(&mut buf).map_err(|_| "The PDF couldn't be made.".to_string())?;
    Ok(buf)
}

/// Several pictures in one PDF (one page each, 150 dpi), saved as `output` (from the Save box).
pub fn images_to_pdf(paths: &[PathBuf], output: &Path, q: u8, check: &dyn Fn() -> Result<(), JobError>) -> Result<PathBuf, JobError> {
    let mut pages = Vec::with_capacity(paths.len());
    for p in paths {
        check()?;
        let pic = images::open_upright(p).map_err(|e| if paths.len() > 1 { format!("{}: {e}", file_name(p)) } else { e })?;
        let (w, h) = pic.img.dimensions();
        pages.push((images::encode(&pic.img, pic.alpha, Fmt::Jpeg, q, None, None)?, w, h));
    }
    if pages.is_empty() {
        return Err(JobError::Failed("There are no pictures to put in the PDF.".into()));
    }
    let out = exact_output(output, None, "pdf")?;
    let bytes = pdf_of_pictures(pages, 150.0)?;
    save_exact(&out, &bytes)?;
    Ok(out)
}

/// Every page of a PDF as a picture "<name> - page N.<jpg|png>" (144 dpi) in `out_dir`.
fn pdf_pages(src: &Path, target: &str, out_dir: &Path, progress: &dyn Fn(f64), check: &dyn Fn() -> Result<(), JobError>) -> Result<Vec<PathBuf>, JobError> {
    let pdfium = super::pdf::engine()?;
    let doc = super::pdf::open_pdfium(pdfium, src)?;
    let total = doc.pages().len().max(0) as usize;
    let mut outputs = Vec::with_capacity(total);
    let damaged = || JobError::Failed("A page couldn't be drawn.".into());
    for i in 0..total {
        check()?;
        let page = doc.pages().get(i as i32).map_err(|_| damaged())?;
        let (w, h) = (page.width().value.max(1.0), page.height().value.max(1.0));
        let mut scale = PDF_SCALE;
        if w * h * scale * scale > MAX_RENDER_PIXELS {
            scale = (MAX_RENDER_PIXELS / (w * h)).sqrt();
        }
        let bitmap = page.render_with_config(&PdfRenderConfig::new().scale_page_by_factor(scale)).map_err(|_| damaged())?;
        let (bw, bh) = (bitmap.width().max(0) as u32, bitmap.height().max(0) as u32);
        let rgba = bitmap.as_rgba_bytes();
        let img = RgbaImage::from_raw(bw, bh, rgba.to_vec()).filter(|_| bw > 0 && bh > 0).ok_or_else(damaged)?;
        let (fmt, quality) = if target == "jpg" { (Fmt::Jpeg, 92) } else { (Fmt::Png, 90) };
        let bytes = images::encode(&img, false, fmt, quality, None, None)?;
        outputs.push(save_new(out_dir, &format!("{} - page {}.{target}", stem(src), i + 1), &bytes)?);
        progress((i + 1) as f64 / total as f64);
    }
    Ok(outputs)
}

// ---------------------------------------------------------------- video and audio (FFmpeg)

/// "file:" stops FFmpeg from reading a name as an option or a network address.
fn file_arg(path: &Path) -> std::ffi::OsString {
    let mut s = std::ffi::OsString::from("file:");
    s.push(path.as_os_str());
    s
}

/// FFmpeg's settings for each target, exactly as the Python version.
pub fn media_args(target: &str, q: &Quality) -> Vec<String> {
    let crf = q.crf;
    let abr = q.abr;
    let x264 = ["-c:v", "libx264", "-preset", "medium", "-crf", &crf.to_string(), "-pix_fmt", "yuv420p"].map(String::from).to_vec();
    let aac = ["-c:a", "aac", "-b:a", abr].map(String::from).to_vec();
    let s = |a: &[&str]| a.iter().map(|x| x.to_string()).collect::<Vec<_>>();
    match target {
        "mp4" | "mov" => [x264, aac, s(&["-movflags", "+faststart"])].concat(),
        "mkv" => [x264, aac].concat(),
        "avi" => s(&["-c:v", "mpeg4", "-q:v", &(crf / 6).max(2).to_string(), "-c:a", "libmp3lame", "-b:a", abr]),
        "webm" => s(&["-c:v", "libvpx-vp9", "-crf", &(crf + 10).to_string(), "-b:v", "0", "-row-mt", "1", "-deadline", "good", "-cpu-used", "4", "-c:a", "libopus", "-b:a", "128k"]),
        "gif" => s(&["-vf", "fps=12,scale='min(540,iw)':-2:flags=lanczos,split[a][b];[a]palettegen[p];[b][p]paletteuse", "-an", "-loop", "0"]),
        "mp3" => s(&["-vn", "-c:a", "libmp3lame", "-b:a", abr]),
        "wav" => s(&["-vn", "-c:a", "pcm_s16le"]),
        "m4a" => s(&["-vn", "-c:a", "aac", "-b:a", abr]),
        "aac" => s(&["-vn", "-c:a", "aac", "-b:a", abr, "-f", "adts"]),
        "flac" => s(&["-vn", "-c:a", "flac"]),
        "ogg" => s(&["-vn", "-c:a", "libvorbis", "-q:a", if abr == "320k" { "6" } else if abr == "192k" { "5" } else { "3" }]),
        "opus" => s(&["-vn", "-c:a", "libopus", "-b:a", if abr != "128k" { "160k" } else { "96k" }]),
        _ => Vec::new(),
    }
}

/// "Duration: 00:01:02.50" in FFmpeg's header output, in seconds (0 if it isn't there).
pub fn parse_duration(text: &str) -> f64 {
    let Some(i) = text.find("Duration: ") else { return 0.0 };
    let rest = &text[i + "Duration: ".len()..];
    let mut parts = rest.splitn(3, ':');
    let (Some(h), Some(m), Some(s)) = (parts.next(), parts.next(), parts.next()) else { return 0.0 };
    let s: String = s.chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
    let all_digits = |x: &str| !x.is_empty() && x.chars().all(|c| c.is_ascii_digit());
    if !all_digits(h) || !all_digits(m) || s.is_empty() || !s.starts_with(|c: char| c.is_ascii_digit()) {
        return 0.0;
    }
    match (h.parse::<f64>(), m.parse::<f64>(), s.parse::<f64>()) {
        (Ok(h), Ok(m), Ok(s)) => h * 3600.0 + m * 60.0 + s,
        _ => 0.0,
    }
}

fn media_duration(ff: &Path, src: &Path) -> f64 {
    let out = super::command(ff).args(["-hide_banner", "-i"]).arg(file_arg(src)).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped()).output();
    out.map(|o| parse_duration(&String::from_utf8_lossy(&o.stderr))).unwrap_or(0.0)
}

/// The last line FFmpeg printed that isn't empty, cut to 160 characters.
fn last_line(text: &str) -> String {
    let line = text.lines().rev().map(str::trim).find(|l| !l.is_empty()).unwrap_or("unknown error");
    line.chars().take(160).collect()
}

fn media(src: &Path, target: &str, out_dir: &Path, q: &Quality, out: Option<PathBuf>, progress: &dyn Fn(f64), check: &dyn Fn() -> Result<(), JobError>) -> Result<PathBuf, JobError> {
    let Some(ff) = super::program("ffmpeg") else { return Err(JobError::Failed(NO_FFMPEG.into())) };
    let out = match out {
        Some(o) => o,
        None => reserve(out_dir, &format!("{}.{target}", stem(src)))?.0,
    };
    let duration = media_duration(&ff, src);
    let spawned = super::command(&ff)
        .args(["-hide_banner", "-nostdin", "-y", "-i"])
        .arg(file_arg(src))
        .args(media_args(target, q))
        .args(["-progress", "pipe:1", "-nostats"])
        .arg(file_arg(&out))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();
    let mut child = match spawned {
        Ok(c) => c,
        Err(_) => {
            let _ = std::fs::remove_file(&out);
            return Err(JobError::Failed("The FFmpeg helper program didn't start.".into()));
        }
    };
    let stderr = child.stderr.take();
    let errors = std::thread::spawn(move || {
        let mut text = String::new();
        if let Some(e) = stderr {
            for line in BufReader::new(e).split(b'\n') {
                let Ok(line) = line else { break };
                text.push_str(&String::from_utf8_lossy(&line));
                text.push('\n');
                if text.len() > 16_000 {
                    // only the end is shown, so keep the last ~8 KB
                    let cut = (text.len() - 8_000..text.len()).find(|i| text.is_char_boundary(*i)).unwrap_or(0);
                    text.drain(..cut);
                }
            }
        }
        text
    });
    if let Some(stdout) = child.stdout.take() {
        for line in BufReader::new(stdout).split(b'\n') {
            let Ok(line) = line else { break };
            if check().is_err() {
                let _ = child.kill();
                let _ = child.wait();
                let _ = errors.join();
                let _ = std::fs::remove_file(&out);
                return Err(JobError::Cancelled);
            }
            let line = String::from_utf8_lossy(&line);
            if let Some(v) = line.trim().strip_prefix("out_time_us=") {
                if duration > 0.0 {
                    if let Ok(us) = v.parse::<i64>() {
                        progress((us as f64 / 1e6 / duration).clamp(0.0, 0.999));
                    }
                }
            }
        }
    }
    let status = child.wait();
    let text = errors.join().unwrap_or_default();
    if !status.is_ok_and(|s| s.success()) {
        let _ = std::fs::remove_file(&out);
        return Err(JobError::Failed(format!("FFmpeg couldn't convert this file ({})", last_line(&text))));
    }
    progress(1.0);
    Ok(out)
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
            let dir = std::env::temp_dir().join(format!("swl-conv-{tag}-{}", hex::encode(r)));
            std::fs::create_dir(&dir).unwrap();
            TempDir(dir)
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn opts(folder: Option<&Path>, output: Option<PathBuf>) -> Opts {
        Opts { quality: quality(Some("balanced")), folder: folder.map(Path::to_path_buf), output }
    }

    fn ok<T>(r: Result<T, JobError>) -> T {
        match r {
            Ok(v) => v,
            Err(JobError::Cancelled) => panic!("cancelled"),
            Err(JobError::Failed(e)) => panic!("{e}"),
        }
    }

    fn err<T>(r: Result<T, JobError>) -> String {
        match r {
            Ok(_) => panic!("expected an error"),
            Err(JobError::Cancelled) => "cancelled".into(),
            Err(JobError::Failed(e)) => e,
        }
    }

    fn no_progress(_: f64) {}
    fn go() -> Result<(), JobError> {
        Ok(())
    }

    #[test]
    fn lists_match_python_and_tool_lists() {
        let lists: Value = serde_json::from_str(include_str!("../tool_lists.json")).unwrap();
        assert_eq!(lists["formats"], supported_formats());
        assert_eq!(kind_of(Path::new("C:/a/Photo.HEIC")), Some("image"));
        assert_eq!(kind_of(Path::new("clip.MP4")), Some("video"));
        assert_eq!(kind_of(Path::new("song.flac")), Some("audio"));
        assert_eq!(kind_of(Path::new("doc.pdf")), Some("pdf"));
        assert_eq!(kind_of(Path::new("notes.txt")), None);
        assert_eq!(kind_of(Path::new("noext")), None);
        assert_eq!(targets_for(Some("video")).len(), 13);
        assert!(targets_for(Some("video")).contains(&"mp3"));
        assert_eq!(targets_for(Some("pdf")), vec!["jpg", "png"]);
        assert!(targets_for(None).is_empty());
    }

    #[test]
    fn quality_presets() {
        assert_eq!(quality(Some("high")), Quality { crf: 18, abr: "320k", image: 95 });
        assert_eq!(quality(Some("small")), Quality { crf: 28, abr: "128k", image: 75 });
        assert_eq!(quality(Some("whatever")), quality(Some("balanced")));
        assert_eq!(quality(None).image, 88);
    }

    #[test]
    fn ffmpeg_settings_match_python() {
        let b = quality(Some("balanced"));
        let a = media_args("mp4", &b);
        assert_eq!(a.join(" "), "-c:v libx264 -preset medium -crf 23 -pix_fmt yuv420p -c:a aac -b:a 192k -movflags +faststart");
        assert_eq!(media_args("mkv", &b).join(" "), "-c:v libx264 -preset medium -crf 23 -pix_fmt yuv420p -c:a aac -b:a 192k");
        assert_eq!(media_args("avi", &quality(Some("high")))[3], "3"); // max(2, 18 // 6)
        assert_eq!(media_args("avi", &quality(Some("small")))[3], "4");
        assert_eq!(media_args("webm", &b)[3], "33");
        assert_eq!(media_args("ogg", &quality(Some("high"))).last().unwrap(), "6");
        assert_eq!(media_args("ogg", &b).last().unwrap(), "5");
        assert_eq!(media_args("ogg", &quality(Some("small"))).last().unwrap(), "3");
        assert_eq!(media_args("opus", &b).last().unwrap(), "160k");
        assert_eq!(media_args("opus", &quality(Some("small"))).last().unwrap(), "96k");
        assert_eq!(media_args("aac", &b).join(" "), "-vn -c:a aac -b:a 192k -f adts");
        for t in VIDEO_OUT.iter().chain(AUDIO_OUT.iter()) {
            assert!(!media_args(t, &b).is_empty(), "{t}");
        }
    }

    #[test]
    fn durations_and_errors() {
        assert_eq!(parse_duration("  Duration: 00:01:02.50, start: 0.000000, bitrate: 128 kb/s"), 62.5);
        assert_eq!(parse_duration("Duration: 01:00:00, start"), 3600.0);
        assert_eq!(parse_duration("Duration: N/A, bitrate: N/A"), 0.0);
        assert_eq!(parse_duration("nothing here"), 0.0);
        assert_eq!(last_line("a\nreal problem  \n\n  \n"), "real problem");
        assert_eq!(last_line(""), "unknown error");
        assert_eq!(last_line(&"x".repeat(500)).len(), 160);
    }

    #[test]
    fn names() {
        assert_eq!(numbered("photo.jpg", 0), "photo.jpg");
        assert_eq!(numbered("photo.jpg", 2), "photo (2).jpg");
        assert_eq!(numbered("a.b.png", 1), "a.b (1).png");
        assert_eq!(numbered(".hidden", 1), ".hidden (1)");
        assert_eq!(combined_name(Path::new("C:/x/one.png"), 3), "one + 2 more.pdf");
        assert_eq!(combined_name(Path::new("C:/x/one.png"), 1), "one.pdf");
        assert_eq!(ico_sizes(1000, 500), vec![256, 128, 64, 48, 32, 16]);
        assert_eq!(ico_sizes(50, 20), vec![48, 32, 16]);
        assert_eq!(ico_sizes(4, 4), vec![16]);
    }

    #[test]
    fn exact_output_rules() {
        let dir = TempDir::new("exact");
        let src = dir.0.join("photo.png");
        std::fs::write(&src, b"x").unwrap();
        assert_eq!(exact_output(&dir.0.join("out"), Some(&src), "jpg").unwrap(), dir.0.join("out.jpg"));
        assert_eq!(exact_output(&dir.0.join("out.JPG"), Some(&src), "jpg").unwrap(), dir.0.join("out.JPG"));
        assert_eq!(exact_output(&dir.0.join("out.png"), Some(&src), "jpg").unwrap(), dir.0.join("out.png.jpg"));
        assert_eq!(exact_output(&src, Some(&src), "png").unwrap_err(), SAME_FILE);
        // a missing folder is made
        assert_eq!(exact_output(&dir.0.join("sub").join("o.pdf"), None, "pdf").unwrap(), dir.0.join("sub").join("o.pdf"));
        assert!(dir.0.join("sub").is_dir());
    }

    #[test]
    fn refuses_what_it_cannot_convert() {
        let dir = TempDir::new("refuse");
        let o = opts(None, None);
        assert_eq!(err(convert_file(&dir.0.join("notes.txt"), "pdf", &o, &no_progress, &go)), "Can't convert .txt to .pdf");
        assert_eq!(err(convert_file(&dir.0.join("README"), "pdf", &o, &no_progress, &go)), "Can't convert this file to .pdf");
        assert_eq!(err(convert_file(&dir.0.join("song.MP3"), "mp4", &o, &no_progress, &go)), "Can't convert .MP3 to .mp4");
    }

    fn picture(dir: &Path, name: &str, alpha: bool) -> PathBuf {
        // left half transparent (if `alpha`), right half a colour gradient. (Not flat colours: with
        // only a handful of distinct values, JPGs from images::encode currently decode as solid
        // green; see the note in the hand-back report.)
        let img = RgbaImage::from_fn(40, 20, |x, y| if x < 20 && alpha { Rgba([255, 0, 0, 0]) } else { Rgba([(x * 6) as u8, (y * 12) as u8, 200, 255]) });
        let p = dir.join(name);
        if name.ends_with(".jpg") {
            image::DynamicImage::ImageRgba8(img).into_rgb8().save(&p).unwrap();
        } else {
            img.save(&p).unwrap();
        }
        p
    }

    #[test]
    fn pictures_convert_next_to_the_original_without_replacing() {
        let dir = TempDir::new("pics");
        let src = picture(&dir.0, "logo.png", true);
        let o = opts(None, None);
        for (target, fmt) in [("jpg", ImageFormat::Jpeg), ("webp", ImageFormat::WebP), ("gif", ImageFormat::Gif), ("bmp", ImageFormat::Bmp), ("tiff", ImageFormat::Tiff), ("ico", ImageFormat::Ico)] {
            let outs = ok(convert_file(&src, target, &o, &no_progress, &go));
            assert_eq!(outs, vec![dir.0.join(format!("logo.{target}"))], "{target}");
            let back = ImageReader::open(&outs[0]).unwrap().with_guessed_format().unwrap();
            assert_eq!(back.format(), Some(fmt), "{target}");
            let img = back.decode().unwrap();
            if target == "ico" {
                assert_eq!((img.width(), img.height()), (32, 32)); // the biggest of 32/16
            } else {
                assert_eq!((img.width(), img.height()), (40, 20), "{target}");
            }
        }
        // JPG puts transparency on white
        // (checked with FFmpeg: the image crate's own JPEG reader currently misreads JPGs that
        // jpeg-encoder writes with optimised Huffman tables, which images::encode uses)
        if let Some(ff) = crate::tools::program("ffmpeg") {
            let o = crate::tools::command(&ff).args(["-v", "error", "-i"]).arg(dir.0.join("logo.jpg")).args(["-f", "rawvideo", "-pix_fmt", "rgb24", "-"]).stdin(Stdio::null()).output().unwrap();
            let at = (10 * 40 + 2) * 3;
            assert_eq!(o.stdout.len(), 40 * 20 * 3);
            assert!(o.stdout[at..at + 3].iter().all(|c| *c > 240), "{:?}", &o.stdout[at..at + 3]);
        }
        // a second conversion gets a new name
        let again = ok(convert_file(&src, "jpg", &o, &no_progress, &go));
        assert_eq!(again, vec![dir.0.join("logo (1).jpg")]);
        // into a chosen folder / an exact file
        let folder = dir.0.join("out");
        let outs = ok(convert_file(&src, "png", &opts(Some(folder.as_path()), None), &no_progress, &go));
        assert_eq!(outs, vec![folder.join("logo.png")]);
        let outs = ok(convert_file(&src, "webp", &opts(None, Some(dir.0.join("chosen"))), &no_progress, &go));
        assert_eq!(outs, vec![dir.0.join("chosen.webp")]);
        // the original can't be the output
        assert_eq!(err(convert_file(&src, "png", &opts(None, Some(src.clone())), &no_progress, &go)), SAME_FILE);
        // and was never touched
        assert_eq!(image::open(&src).unwrap().dimensions_tuple(), (40, 20));
        let names: Vec<_> = std::fs::read_dir(&dir.0).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().to_string()).collect();
        assert!(!names.iter().any(|n| n.contains("swiftlane-part")));
    }

    trait Dims {
        fn dimensions_tuple(&self) -> (u32, u32);
    }
    impl Dims for image::DynamicImage {
        fn dimensions_tuple(&self) -> (u32, u32) {
            (self.width(), self.height())
        }
    }

    fn pdf_page_sizes(path: &Path) -> Vec<(f32, f32)> {
        let doc = Document::load(path).unwrap();
        doc.page_iter()
            .map(|id| {
                let page = doc.get_dictionary(id).unwrap();
                let mb = page.get(b"MediaBox").unwrap().as_array().unwrap();
                let num = |o: &Object| o.as_float().unwrap_or_else(|_| o.as_i64().unwrap() as f32);
                (num(&mb[2]), num(&mb[3]))
            })
            .collect()
    }

    #[test]
    fn pictures_to_pdf() {
        let dir = TempDir::new("pdf");
        let a = picture(&dir.0, "a.png", true);
        let b = picture(&dir.0, "b.jpg", false);
        // one picture: one page, a point per pixel (Pillow's default 72 dpi)
        let outs = ok(convert_file(&a, "pdf", &opts(None, None), &no_progress, &go));
        assert_eq!(outs, vec![dir.0.join("a.pdf")]);
        assert_eq!(pdf_page_sizes(&outs[0]), vec![(40.0, 20.0)]);
        // several in one: 150 dpi, saved as the chosen name with .pdf added
        let out = ok(images_to_pdf(&[a, b], &dir.0.join("both"), 88, &go));
        assert_eq!(out, dir.0.join("both.pdf"));
        let sizes = pdf_page_sizes(&out);
        assert_eq!(sizes.len(), 2);
        assert!((sizes[0].0 - 19.2).abs() < 0.01 && (sizes[0].1 - 9.6).abs() < 0.01, "{sizes:?}");
        // cancelling stops it
        assert_eq!(err(images_to_pdf(&[dir.0.join("a.png")], &dir.0.join("c.pdf"), 88, &|| Err(JobError::Cancelled))), "cancelled");
    }

    fn animated_gif_file(dir: &Path) -> PathBuf {
        let frames: Vec<image::Frame> = (0..3u8)
            .map(|i| image::Frame::from_parts(RgbaImage::from_pixel(16, 8, Rgba([i * 80, 0, 255 - i * 80, 255])), 0, 0, image::Delay::from_numer_denom_ms(120, 1)))
            .collect();
        let p = dir.join("anim.gif");
        std::fs::write(&p, animated_gif(frames).unwrap()).unwrap();
        p
    }

    fn frame_count(path: &Path) -> usize {
        let f = || BufReader::new(std::fs::File::open(path).unwrap());
        match ImageReader::open(path).unwrap().with_guessed_format().unwrap().format() {
            Some(ImageFormat::Gif) => GifDecoder::new(f()).unwrap().into_frames().count(),
            Some(ImageFormat::WebP) => WebPDecoder::new(f()).unwrap().into_frames().count(),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn animations_stay_animated() {
        let dir = TempDir::new("anim");
        let src = animated_gif_file(&dir.0);
        assert_eq!(animation(&src).unwrap().map(|f| f.len()), Some(3));
        let webp_out = ok(convert_file(&src, "webp", &opts(None, None), &no_progress, &go));
        assert_eq!(frame_count(&webp_out[0]), 3);
        let gif_again = ok(convert_file(&webp_out[0], "gif", &opts(None, None), &no_progress, &go));
        assert_eq!(gif_again, vec![dir.0.join("anim (1).gif")]);
        assert_eq!(frame_count(&gif_again[0]), 3);
        // other targets get the first frame
        let png = ok(convert_file(&src, "png", &opts(None, None), &no_progress, &go));
        assert_eq!(image::open(&png[0]).unwrap().dimensions_tuple(), (16, 8));
        // a still picture isn't an animation
        assert!(animation(&picture(&dir.0, "still.gif", false)).unwrap().is_none());
    }

    #[test]
    fn video_and_audio_when_ffmpeg_is_there() {
        let Some(ff) = crate::tools::program("ffmpeg") else { return };
        let dir = TempDir::new("media");
        let clip = dir.0.join("clip.mp4");
        let made = crate::tools::command(&ff)
            .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i", "testsrc=size=64x48:rate=10:duration=1", "-f", "lavfi", "-i", "sine=duration=1", "-shortest", "-c:v", "mpeg4", "-c:a", "aac"])
            .arg(&clip)
            .stdin(Stdio::null())
            .status();
        if !made.is_ok_and(|s| s.success()) {
            return;
        }
        assert!(media_duration(&ff, &clip) > 0.5);
        let seen = std::cell::Cell::new(0.0f64);
        let outs = ok(convert_file(&clip, "wav", &opts(None, None), &|f| seen.set(f), &go));
        assert_eq!(outs, vec![dir.0.join("clip.wav")]);
        assert!(std::fs::metadata(&outs[0]).unwrap().len() > 1000);
        assert_eq!(seen.get(), 1.0);
        // cancelled: nothing is left behind
        assert_eq!(err(convert_file(&clip, "mkv", &opts(None, None), &no_progress, &|| Err(JobError::Cancelled))), "cancelled");
        assert!(!dir.0.join("clip.mkv").exists());
        // a broken input fails plainly and leaves nothing
        let bad = dir.0.join("bad.mp4");
        std::fs::write(&bad, b"not a video").unwrap();
        assert!(err(convert_file(&bad, "mp3", &opts(None, None), &no_progress, &go)).starts_with("FFmpeg couldn't convert this file ("));
        assert!(!dir.0.join("bad.mp3").exists());
    }
}
