//! Shrink to size: make a video or picture small enough for Discord, WhatsApp, email or a size of
//! your own, then put the result on the clipboard as a file (Ctrl+V pastes it).
//!
//! Page calls: shrink_presets(), shrink_open(path), shrink_plan(info, mb), shrink_start(path, mb),
//! shrink_copy(path), shrink_save_as(path, source).
//!
//! Safety rules: the original is only read, never written; results live in a private folder
//! (%LOCALAPPDATA%\Swiftlane\shrink, wiped at start and exit) and only files from there can be
//! copied or saved by the page; FFmpeg gets an argument list ("file:" names), never a shell; the
//! output is measured and must be at or under the target, or the job fails instead of lying.

use image::{imageops, RgbaImage};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tauri::{AppHandle, Manager};

use super::images::{self, Fmt};
use super::trimmer::{capture, ffmpeg, file_arg, parse_out_time, parse_probe, stream};
use super::{arg, ask_save, program};
use crate::jobs::{Ctx, JobError};

const NO_FFMPEG: &str = "Can't shrink videos yet: that needs the FFmpeg helper program next to Swiftlane.";
const PRESETS: &str = include_str!("shrink_presets.json");

pub fn dispatch(app: &AppHandle, method: &str, args: &[Value]) -> Option<Value> {
    Some(match method {
        "shrink_presets" => presets_json(),
        "shrink_open" => open(Path::new(arg(args, 0).as_str().unwrap_or(""))).unwrap_or_else(|e| json!({ "error": e })),
        "shrink_plan" => plan_json(arg(args, 0), arg(args, 1)).unwrap_or_else(|e| json!({ "error": e })),
        "shrink_start" => start(app, arg(args, 0).as_str().unwrap_or(""), arg(args, 1)),
        "shrink_copy" => {
            let p = PathBuf::from(arg(args, 0).as_str().unwrap_or(""));
            match in_shrink_dir(&p).and_then(|p| copy_to_clipboard(&p).map(|_| p)) {
                Ok(p) => json!({ "ok": true, "name": file_name(&p), "bytes": std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0) }),
                Err(e) => json!({ "error": e }),
            }
        }
        "shrink_save_as" => save_as(app, arg(args, 0).as_str().unwrap_or(""), arg(args, 1).as_str().unwrap_or("")),
        _ => return None,
    })
}

// ---------------------------------------------------------------- presets and the target

#[derive(Debug, Clone, PartialEq)]
pub struct Preset {
    pub id: String,
    pub name: String,
    pub mb: f64,
}

/// (min MB, max MB, presets) from shrink_presets.json. Entries that make no sense are skipped.
pub fn parse_presets(text: &str) -> (f64, f64, Vec<Preset>) {
    let v: Value = serde_json::from_str(text).unwrap_or(Value::Null);
    let min = v["min_mb"].as_f64().filter(|x| *x > 0.0).unwrap_or(0.5);
    let max = v["max_mb"].as_f64().filter(|x| *x >= min).unwrap_or(2000.0);
    let list = v["presets"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|p| {
                    let (id, name, mb) = (p["id"].as_str()?, p["name"].as_str()?, p["mb"].as_f64()?);
                    (!id.is_empty() && !name.is_empty() && mb >= min && mb <= max).then(|| Preset { id: id.into(), name: name.into(), mb })
                })
                .collect()
        })
        .unwrap_or_default();
    (min, max, list)
}

fn presets_json() -> Value {
    let (min, max, list) = parse_presets(PRESETS);
    json!({ "min_mb": min, "max_mb": max, "presets": list.iter().map(|p| json!({ "id": p.id, "name": p.name, "mb": p.mb })).collect::<Vec<_>>() })
}

/// The size limit in bytes from what the page sent (MB, as a number or numeric text).
pub fn target_bytes(mb: &Value) -> Result<u64, String> {
    let (min, max, _) = parse_presets(PRESETS);
    let x = match mb {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().replace(',', ".").parse().ok(),
        _ => None,
    }
    .filter(|x| x.is_finite())
    .ok_or("Enter the size limit in MB.")?;
    if x < min || x > max {
        return Err(format!("The size limit must be between {min} and {max} MB."));
    }
    Ok((x * 1_000_000.0) as u64)
}

// ---------------------------------------------------------------- the size math (pure)

/// Share of the limit the encoder may use; the rest absorbs container overhead and rate drift.
const MARGIN: f64 = 0.93;
/// Bits per pixel per frame that x264 still looks clean at.
const BPP_GOOD: f64 = 0.07;
const LADDER: [u32; 4] = [720, 480, 360, 240];

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VideoInfo {
    pub duration: f64,
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub audio: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Quality {
    Good,
    Fair,
    Poor,
    Bad,
}

impl Quality {
    fn name(self) -> &'static str {
        match self {
            Quality::Good => "good",
            Quality::Fair => "fair",
            Quality::Poor => "poor",
            Quality::Bad => "bad",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Plan {
    pub video_bps: u64,
    pub audio_bps: u64,
    pub out_width: u32,
    pub out_height: u32,
    pub downscaled: bool,
    pub quality: Quality,
    /// For poor/bad results: about how long a clip would still look good at this size.
    pub good_seconds: Option<f64>,
}

fn even_width(w: u32, h: u32, new_h: u32) -> u32 {
    if h == 0 {
        return w;
    }
    ((w as f64 * new_h as f64 / h as f64 / 2.0).round() as u32 * 2).max(2)
}

pub fn plan_video(info: &VideoInfo, target_bytes: u64) -> Plan {
    let duration = info.duration.max(0.1);
    let budget_bits = target_bytes as f64 * 8.0 * MARGIN;
    let total_bps = budget_bits / duration;
    let audio_bps: u64 = if !info.audio {
        0
    } else if total_bps >= 800_000.0 {
        128_000
    } else if total_bps >= 400_000.0 {
        96_000
    } else if total_bps >= 200_000.0 {
        64_000
    } else {
        32_000
    };
    let video_bps = (total_bps - audio_bps as f64).max(50_000.0);
    let fps = if info.fps > 0.0 { info.fps.min(30.0) } else { 30.0 };
    let bpp = |w: u32, h: u32| video_bps / (w as f64 * h as f64 * fps);
    // never bigger than the original; the largest size that still gets enough bits wins
    let (sw, sh) = (info.width, info.height);
    let mut rungs: Vec<u32> = Vec::new();
    if sh > 0 {
        rungs.push(sh);
        rungs.extend(LADDER.iter().copied().filter(|r| *r < sh));
    }
    let (out_w, out_h) = if rungs.is_empty() {
        (sw, sh)
    } else {
        let pick = rungs.iter().copied().find(|h| bpp(even_width(sw, sh, *h), *h) >= BPP_GOOD).unwrap_or(*rungs.last().unwrap());
        (if pick == sh { sw } else { even_width(sw, sh, pick) }, pick)
    };
    let downscaled = sh > 0 && out_h < sh;
    let density = if out_w > 0 && out_h > 0 { bpp(out_w, out_h) } else { f64::INFINITY };
    let quality = if video_bps < 250_000.0 || density < 0.03 {
        Quality::Bad
    } else if video_bps < 500_000.0 || density < 0.05 {
        Quality::Poor
    } else if downscaled && out_h < sh.min(720) {
        Quality::Fair
    } else {
        Quality::Good
    };
    let good_seconds = if matches!(quality, Quality::Poor | Quality::Bad) && sh > 0 {
        let h = sh.min(720);
        let need = BPP_GOOD * even_width(sw, sh, h) as f64 * h as f64 * fps + audio_bps as f64;
        let secs = budget_bits / need;
        (secs < duration).then_some(secs)
    } else {
        None
    };
    Plan { video_bps: video_bps as u64, audio_bps, out_width: out_w, out_height: out_h, downscaled, quality, good_seconds }
}

fn num(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
    .filter(|x| x.is_finite())
}

fn info_from(v: &Value) -> VideoInfo {
    VideoInfo {
        duration: num(&v["duration"]).unwrap_or(0.0).clamp(0.0, 1e7),
        width: num(&v["width"]).unwrap_or(0.0).clamp(0.0, 20_000.0) as u32,
        height: num(&v["height"]).unwrap_or(0.0).clamp(0.0, 20_000.0) as u32,
        fps: num(&v["fps"]).unwrap_or(0.0).clamp(0.0, 1000.0),
        audio: v["audio"].as_bool().unwrap_or(false),
    }
}

fn plan_json(info: &Value, mb: &Value) -> Result<Value, String> {
    let target = target_bytes(mb)?;
    let bytes = num(&info["bytes"]).unwrap_or(0.0).max(0.0) as u64;
    let mut out = json!({ "target_bytes": target, "fits": bytes > 0 && bytes <= target });
    if info["kind"].as_str() == Some("video") {
        let p = plan_video(&info_from(info), target);
        out["video_kbps"] = json!(p.video_bps / 1000);
        out["audio_kbps"] = json!(p.audio_bps / 1000);
        out["out_width"] = json!(p.out_width);
        out["out_height"] = json!(p.out_height);
        out["downscaled"] = json!(p.downscaled);
        out["quality"] = json!(p.quality.name());
        out["good_seconds"] = json!(p.good_seconds);
    }
    Ok(out)
}

// ---------------------------------------------------------------- reading the file

fn file_name(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()
}

/// "image" or "video" for a regular file Swiftlane can shrink.
fn input_kind(path: &Path) -> Result<&'static str, String> {
    let meta = std::fs::metadata(path).map_err(|_| "Swiftlane can't find that file.".to_string())?;
    if !meta.is_file() {
        return Err("That isn't a file.".into());
    }
    match crate::api::file_info(path)["kind"].as_str() {
        Some("image") => Ok("image"),
        Some("video") => Ok("video"),
        _ => Err("Shrink to size works on videos and pictures.".into()),
    }
}

/// "29.97 fps" on the Video line of what `ffmpeg -i` prints.
pub fn parse_fps(text: &str) -> Option<f64> {
    text.lines().filter(|l| l.contains("Video:")).find_map(|l| {
        l.split(',').find_map(|part| part.trim().strip_suffix(" fps").and_then(|n| n.trim().parse::<f64>().ok()))
    })
}

/// A phone video turned sideways ("rotation of -90.00 degrees"): the picture is shown the other way round.
pub fn parse_rotation(text: &str) -> f64 {
    text.find("rotation of ")
        .and_then(|i| text[i + 12..].split_whitespace().next()?.parse::<f64>().ok())
        .unwrap_or(0.0)
}

fn open(path: &Path) -> Result<Value, String> {
    let kind = input_kind(path)?;
    let bytes = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let mut v = json!({ "kind": kind, "path": path.to_string_lossy(), "name": file_name(path), "bytes": bytes });
    if kind == "image" {
        let dims = image::ImageReader::open(path).ok().and_then(|r| r.with_guessed_format().ok()).and_then(|r| r.into_dimensions().ok());
        let (w, h) = dims.unwrap_or((0, 0));
        v["width"] = json!(w);
        v["height"] = json!(h);
        return Ok(v);
    }
    let ff = program("ffmpeg").ok_or(NO_FFMPEG)?;
    let mut c = ffmpeg(&ff);
    c.arg("-i").arg(file_arg(path));
    let out = capture(c, Duration::from_secs(30), false)?;
    let text = String::from_utf8_lossy(&out.stderr).to_string();
    let (duration, mut w, mut h, audio) = parse_probe(&text).ok_or("Swiftlane can't read a video in this file.")?;
    let rot = parse_rotation(&text).abs().round() as i64 % 180;
    if rot == 90 {
        std::mem::swap(&mut w, &mut h);
    }
    v["duration"] = json!(duration);
    v["width"] = json!(w);
    v["height"] = json!(h);
    v["fps"] = json!(parse_fps(&text).unwrap_or(0.0));
    v["audio"] = json!(audio);
    Ok(v)
}

// ---------------------------------------------------------------- the private output folder

fn shrink_dir() -> PathBuf {
    dirs::data_local_dir().unwrap_or_else(std::env::temp_dir).join("Swiftlane").join("shrink")
}

/// Removes a folder named "shrink" and everything in it. Refuses any other folder name.
pub fn wipe_dir(dir: &Path) {
    if dir.file_name().is_some_and(|n| n == "shrink") {
        let _ = std::fs::remove_dir_all(dir);
    }
}

/// Wipes the private folder (called when Swiftlane starts and when it closes).
pub fn wipe() {
    wipe_dir(&shrink_dir());
}

/// The path, if it is a file inside the private folder (so the page can't copy or save other files).
fn in_shrink_dir(path: &Path) -> Result<PathBuf, String> {
    let dir = std::fs::canonicalize(shrink_dir()).map_err(|_| "That result isn't available any more.".to_string())?;
    let p = std::fs::canonicalize(path).map_err(|_| "That result isn't available any more.".to_string())?;
    if p.starts_with(&dir) && p.is_file() { Ok(p) } else { Err("That isn't a result file from Shrink to size.".into()) }
}

/// "name (shrunk).ext" in `dir`, or "name (shrunk) (2).ext" if taken. Nothing is overwritten.
pub fn unique_name(dir: &Path, stem: &str, ext: &str) -> PathBuf {
    let base = format!("{stem} (shrunk)");
    for n in 1..10_000 {
        let name = if n == 1 { format!("{base}.{ext}") } else { format!("{base} ({n}).{ext}") };
        let p = dir.join(name);
        if !p.exists() {
            return p;
        }
    }
    dir.join(format!("{base} {}.{ext}", std::process::id()))
}

fn copy_to_clipboard(path: &Path) -> Result<(), String> {
    arboard::Clipboard::new()
        .and_then(|mut c| c.set().file_list(&[path]))
        .map_err(|_| "Windows wouldn't let Swiftlane use the clipboard just now. Use Save as… instead.".to_string())
}

// ---------------------------------------------------------------- the job

fn start(app: &AppHandle, path: &str, mb: &Value) -> Value {
    let target = match target_bytes(mb) {
        Ok(t) => t,
        Err(e) => return json!({ "error": e }),
    };
    let src = PathBuf::from(path);
    if let Err(e) = input_kind(&src) {
        return json!({ "error": e });
    }
    let mb_text = target as f64 / 1e6;
    let title = format!("Shrink to {mb_text} MB · {}", file_name(&src));
    let id = app.state::<crate::api::AppState>().jobs.start(app, "shrink", &title, true, move |ctx| {
        // no outputs are returned: they live in the private folder, so they are not remembered for the uninstaller
        let done = run(ctx, &src, target)?;
        let copied = copy_to_clipboard(&done.path).is_ok();
        crate::api::push(
            ctx.app(),
            "shrink-done",
            json!({
                "path": done.path.to_string_lossy(), "name": file_name(&done.path), "bytes": done.bytes, "orig_bytes": done.orig_bytes,
                "width": done.width, "height": done.height, "target_bytes": target, "copied": copied, "kind": done.kind,
            }),
        );
        Ok(vec![])
    });
    json!(id)
}

struct Done {
    path: PathBuf,
    bytes: u64,
    orig_bytes: u64,
    width: u32,
    height: u32,
    kind: &'static str,
}

fn run(ctx: &Ctx, src: &Path, target: u64) -> Result<Done, JobError> {
    let kind = input_kind(src).map_err(JobError::Failed)?;
    let orig = std::fs::metadata(src)?.len();
    let dir = shrink_dir();
    std::fs::create_dir_all(&dir).map_err(|e| JobError::Failed(format!("Swiftlane couldn't make its private folder ({e}).")))?;
    let stem = src.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "file".into());
    let src_ext = src.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    // already small enough: hand it over untouched, quality stays exactly as it was
    if orig <= target {
        let out = unique_name(&dir, &stem, if src_ext.is_empty() { "bin" } else { &src_ext });
        std::fs::copy(src, &out)?;
        let (w, h) = if kind == "image" { image::image_dimensions(&out).unwrap_or((0, 0)) } else { (0, 0) };
        return Ok(Done { path: out, bytes: orig, orig_bytes: orig, width: w, height: h, kind });
    }
    if kind == "image" {
        ctx.report(None, Some("Shrinking the picture…"));
        let pic = images::open_upright(src).map_err(JobError::Failed)?;
        let keep_png = pic.format == Some("PNG");
        let s = shrink_picture(&pic.img, pic.alpha, keep_png, target, &|| ctx.check())?;
        let out = unique_name(&dir, &stem, if s.fmt == Fmt::Png { "png" } else { "jpg" });
        std::fs::write(&out, &s.bytes)?;
        return Ok(Done { path: out, bytes: s.bytes.len() as u64, orig_bytes: orig, width: s.width, height: s.height, kind });
    }
    let info_v = open(src).map_err(JobError::Failed)?;
    let info = info_from(&info_v);
    let ff = program("ffmpeg").ok_or_else(|| JobError::Failed(NO_FFMPEG.into()))?;
    let out = unique_name(&dir, &stem, "mp4");
    let log = dir.join(format!("pass-{}", std::process::id()));
    let result = encode_video(ctx, &ff, src, &out, &log, &info, target);
    clean_pass_logs(&dir, &log);
    match result {
        Ok((w, h)) => {
            let bytes = std::fs::metadata(&out)?.len();
            Ok(Done { path: out, bytes, orig_bytes: orig, width: w, height: h, kind })
        }
        Err(e) => {
            let _ = std::fs::remove_file(&out);
            Err(e)
        }
    }
}

fn clean_pass_logs(dir: &Path, prefix: &Path) {
    let Some(stem) = prefix.file_name().map(|n| n.to_string_lossy().to_string()) else { return };
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            if e.file_name().to_string_lossy().starts_with(&stem) {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
}

/// FFmpeg's options for one pass (before the output name).
pub fn pass_args(src: &Path, plan: &Plan, info: &VideoInfo, pass: u8, log: &Path, video_bps: u64) -> Vec<std::ffi::OsString> {
    let mut a: Vec<std::ffi::OsString> = vec!["-y".into(), "-loglevel".into(), "error".into(), "-progress".into(), "pipe:1".into(), "-nostats".into(), "-i".into(), file_arg(src)];
    let mut s = |items: &[&str]| a.extend(items.iter().map(std::ffi::OsString::from));
    let mut filters: Vec<String> = Vec::new();
    if info.fps > 30.5 {
        filters.push("fps=30".into());
    }
    if plan.downscaled {
        filters.push(format!("scale={}:{}", plan.out_width, plan.out_height));
    }
    s(&["-map", "0:v:0", "-sn", "-dn", "-map_metadata", "-1"]);
    if !filters.is_empty() {
        s(&["-vf", &filters.join(",")]);
    }
    s(&["-c:v", "libx264", "-preset", "fast", "-pix_fmt", "yuv420p", "-b:v", &video_bps.to_string()]);
    s(&["-maxrate", &(video_bps * 3 / 2).to_string(), "-bufsize", &(video_bps * 2).to_string()]);
    s(&["-pass", &pass.to_string(), "-passlogfile"]);
    a.push(log.as_os_str().to_os_string());
    if pass == 1 {
        let mut s = |items: &[&str]| a.extend(items.iter().map(std::ffi::OsString::from));
        s(&["-an", "-f", "null", "-"]);
    } else {
        let mut s = |items: &[&str]| a.extend(items.iter().map(std::ffi::OsString::from));
        if plan.audio_bps > 0 {
            s(&["-map", "0:a:0?", "-c:a", "aac", "-b:a", &plan.audio_bps.to_string(), "-ac", "2"]);
        } else {
            s(&["-an"]);
        }
        s(&["-profile:v", "high", "-movflags", "+faststart"]);
    }
    a
}

/// Two passes, then measure. If it landed over the limit anyway, lower the bitrate and redo pass 2.
fn encode_video(ctx: &Ctx, ff: &Path, src: &Path, out: &Path, log: &Path, info: &VideoInfo, target: u64) -> Result<(u32, u32), JobError> {
    let plan = plan_video(info, target);
    let duration = info.duration.max(0.1);
    let run_pass = |pass: u8, bps: u64, from: f64, to: f64, label: &str| -> Result<(), JobError> {
        let mut c = ffmpeg(ff);
        c.args(pass_args(src, &plan, info, pass, log, bps));
        if pass == 2 {
            c.arg(file_arg(out));
        }
        ctx.report(Some(from), Some(label));
        let (ok, tail) = stream(c, ctx, false, |is_err, line| {
            if !is_err {
                if let Some(t) = parse_out_time(line) {
                    ctx.report(Some(from + (to - from) * (t / duration).min(1.0)), Some(label));
                }
            }
        })?;
        if !ok {
            let tail: Vec<char> = tail.trim_end().chars().collect();
            let tail: String = tail[tail.len().saturating_sub(200)..].iter().collect();
            return Err(JobError::Failed(format!("FFmpeg couldn't shrink this video: {tail}")));
        }
        Ok(())
    };
    let mut bps = plan.video_bps;
    run_pass(1, bps, 0.0, 0.4, "Looking at the video (1 of 2)…")?;
    for attempt in 0..3 {
        run_pass(2, bps, 0.4, 0.97, if attempt == 0 { "Encoding (2 of 2)…" } else { "A little over, trying again…" })?;
        let size = std::fs::metadata(out).map(|m| m.len()).unwrap_or(0);
        if size > 0 && size <= target {
            ctx.report(Some(1.0), None);
            let (w, h) = if plan.downscaled { (plan.out_width, plan.out_height) } else { (info.width, info.height) };
            return Ok((w, h));
        }
        if size == 0 {
            return Err(JobError::Failed("FFmpeg didn't make a file.".into()));
        }
        bps = ((bps as f64) * (target as f64 * 0.95 / size as f64)).max(30_000.0) as u64;
    }
    Err(JobError::Failed("Swiftlane couldn't get this video under the limit. Trim it shorter first and try again.".into()))
}

// ---------------------------------------------------------------- pictures

pub struct Shrunk {
    pub bytes: Vec<u8>,
    pub fmt: Fmt,
    pub width: u32,
    pub height: u32,
}

const MIN_QUALITY: u8 = 55;
const MAX_QUALITY: u8 = 92;

/// A JPEG (or the PNG, if it already fits) at or under `target` bytes: the best quality that fits
/// at full size, and if even quality 55 is too big, smaller pictures instead of uglier ones.
pub fn shrink_picture(img: &RgbaImage, alpha: bool, try_png: bool, target: u64, check: &dyn Fn() -> Result<(), JobError>) -> Result<Shrunk, JobError> {
    let fail = |e: String| JobError::Failed(e);
    let (w0, h0) = img.dimensions();
    if try_png {
        let png = images::encode(img, alpha, Fmt::Png, 100, None, None).map_err(fail)?;
        if png.len() as u64 <= target {
            return Ok(Shrunk { bytes: png, fmt: Fmt::Png, width: w0, height: h0 });
        }
    }
    let mut scale = 1.0f64;
    for _ in 0..16 {
        check()?;
        let (w, h) = (((w0 as f64 * scale).round() as u32).max(1), ((h0 as f64 * scale).round() as u32).max(1));
        if w.min(h) < 16 {
            return Err(JobError::Failed("This picture can't be made that small and still be a picture. Pick a bigger limit.".into()));
        }
        let scaled;
        let cur: &RgbaImage = if scale >= 1.0 {
            img
        } else {
            scaled = imageops::resize(img, w, h, imageops::FilterType::Lanczos3);
            &scaled
        };
        let enc = |q: u8| images::encode(cur, alpha, Fmt::Jpeg, q, None, None).map_err(JobError::Failed);
        let mut best = enc(MIN_QUALITY)?;
        if best.len() as u64 > target {
            let ratio = (target as f64 / best.len() as f64).sqrt() * 0.93;
            scale = (scale * ratio).min(scale * 0.92);
            continue;
        }
        let (mut lo, mut hi) = (MIN_QUALITY, MAX_QUALITY + 1);
        while hi - lo > 1 {
            check()?;
            let mid = (lo + hi) / 2;
            let bytes = enc(mid)?;
            if bytes.len() as u64 <= target {
                lo = mid;
                best = bytes;
            } else {
                hi = mid;
            }
        }
        return Ok(Shrunk { bytes: best, fmt: Fmt::Jpeg, width: w, height: h });
    }
    Err(JobError::Failed("Swiftlane couldn't get this picture under the limit.".into()))
}

// ---------------------------------------------------------------- Save as…

fn save_as(app: &AppHandle, result: &str, source: &str) -> Value {
    let from = match in_shrink_dir(Path::new(result)) {
        Ok(p) => p,
        Err(e) => return json!({ "error": e }),
    };
    let src = Path::new(source);
    let folder = src.parent().map(Path::to_path_buf).filter(|p| p.is_dir()).unwrap_or_else(|| dirs::document_dir().unwrap_or_default());
    let ext = from.extension().map(|e| e.to_string_lossy().to_string()).unwrap_or_default();
    let Some(to) = ask_save(app, &folder, &file_name(&from), &[ext.as_str()]) else { return Value::Null };
    // the original is never replaced (Windows already asked about any other file)
    if matches!((std::fs::canonicalize(src), std::fs::canonicalize(&to)), (Ok(a), Ok(b)) if a == b) {
        return json!({ "error": "Pick another name: the shrunk file can't replace the original." });
    }
    let name = file_name(&to);
    let tmp = to.with_file_name(format!(".{name}.swiftlane-part"));
    let res = std::fs::copy(&from, &tmp).and_then(|_| std::fs::rename(&tmp, &to));
    if let Err(e) = res {
        let _ = std::fs::remove_file(&tmp);
        return json!({ "error": format!("The file couldn't be saved ({e}).") });
    }
    json!(to.to_string_lossy())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Temp {
            let mut b = [0u8; 6];
            getrandom::getrandom(&mut b).unwrap();
            let d = std::env::temp_dir().join(format!("swl-shrink-test-{}", hex::encode(b)));
            std::fs::create_dir(&d).unwrap();
            Temp(d)
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn vid(duration: f64, w: u32, h: u32, fps: f64, audio: bool) -> VideoInfo {
        VideoInfo { duration, width: w, height: h, fps, audio }
    }

    #[test]
    fn shipped_presets_are_valid() {
        let (min, max, list) = parse_presets(PRESETS);
        assert!(min > 0.0 && max > min);
        let got: Vec<(&str, f64)> = list.iter().map(|p| (p.id.as_str(), p.mb)).collect();
        assert_eq!(got, [("discord", 10.0), ("whatsapp", 16.0), ("email", 25.0)]);
        assert_eq!(list[0].name, "Discord");
    }

    #[test]
    fn bad_preset_entries_are_skipped() {
        let (_, _, list) = parse_presets(r#"{"min_mb":1,"max_mb":100,"presets":[{"id":"a","name":"A","mb":5},{"id":"","name":"B","mb":5},{"id":"c","name":"C","mb":0.1},{"id":"d","name":"D","mb":500},{"id":"e","name":"E"},{"id":"f","name":"F","mb":"9"}]}"#);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, "a");
        assert!(parse_presets("not json").2.is_empty());
    }

    #[test]
    fn reads_the_size_limit() {
        assert_eq!(target_bytes(&json!(10)), Ok(10_000_000));
        assert_eq!(target_bytes(&json!("7,5")), Ok(7_500_000));
        assert_eq!(target_bytes(&json!(0.5)), Ok(500_000));
        assert!(target_bytes(&json!(0.1)).is_err());
        assert!(target_bytes(&json!(99999)).is_err());
        assert!(target_bytes(&json!(null)).is_err());
        assert!(target_bytes(&json!("abc")).is_err());
    }

    #[test]
    fn the_plan_stays_inside_the_limit() {
        for (dur, mb) in [(10.0, 10u64), (60.0, 10), (600.0, 10), (300.0, 25), (3600.0, 16)] {
            let p = plan_video(&vid(dur, 1920, 1080, 30.0, true), mb * 1_000_000);
            let bits = (p.video_bps + p.audio_bps) as f64 * dur;
            // the floor of 50 kbps only matters for absurd cases (an hour into 16 MB)
            if p.video_bps > 50_000 {
                assert!(bits <= mb as f64 * 1_000_000.0 * 8.0 * 0.931, "{dur}s into {mb} MB used {bits} bits");
            }
        }
    }

    #[test]
    fn short_clips_keep_their_size_and_long_ones_shrink() {
        let short = plan_video(&vid(10.0, 1920, 1080, 30.0, true), 25_000_000);
        assert_eq!((short.out_height, short.downscaled, short.quality), (1080, false, Quality::Good));
        let mid = plan_video(&vid(60.0, 1920, 1080, 30.0, true), 10_000_000);
        assert_eq!((mid.out_height, mid.out_width, mid.downscaled), (480, 854, true));
        assert_eq!(mid.quality, Quality::Fair);
        assert!(mid.good_seconds.is_none());
        // never upscales a small video
        let small = plan_video(&vid(5.0, 320, 240, 30.0, true), 25_000_000);
        assert_eq!((small.out_width, small.out_height, small.downscaled), (320, 240, false));
    }

    #[test]
    fn long_videos_get_a_warning_and_a_trim_suggestion() {
        let p = plan_video(&vid(900.0, 1920, 1080, 30.0, true), 10_000_000);
        assert!(matches!(p.quality, Quality::Poor | Quality::Bad));
        assert!(p.video_bps < 500_000);
        let good = p.good_seconds.expect("a shorter clip would look fine");
        assert!(good > 5.0 && good < 900.0);
        // trimming to that length really does give a good plan
        let again = plan_video(&vid(good * 0.98, 1920, 1080, 30.0, true), 10_000_000);
        assert!(matches!(again.quality, Quality::Good | Quality::Fair), "{again:?}");
    }

    #[test]
    fn sound_less_videos_spend_nothing_on_audio_and_high_fps_is_capped() {
        let p = plan_video(&vid(60.0, 1280, 720, 60.0, false), 10_000_000);
        assert_eq!(p.audio_bps, 0);
        let args: Vec<String> = pass_args(Path::new("C:\\v\\clip.mp4"), &p, &vid(60.0, 1280, 720, 60.0, false), 2, Path::new("C:\\t\\pass-1"), p.video_bps)
            .iter().map(|s| s.to_string_lossy().to_string()).collect();
        assert!(args.windows(2).any(|w| w == ["-vf".to_string(), if p.downscaled { format!("fps=30,scale={}:{}", p.out_width, p.out_height) } else { "fps=30".into() }]));
        assert!(args.contains(&"-an".to_string()));
    }

    #[test]
    fn ffmpeg_arguments_are_safe() {
        let info = vid(60.0, 1920, 1080, 30.0, true);
        let p = plan_video(&info, 10_000_000);
        let src = Path::new("-evil name.mp4");
        let one: Vec<String> = pass_args(src, &p, &info, 1, Path::new("C:\\t\\pass-1"), p.video_bps).iter().map(|s| s.to_string_lossy().to_string()).collect();
        assert_eq!(one[one.iter().position(|a| a == "-i").unwrap() + 1], "file:-evil name.mp4");
        assert!(one.ends_with(&["-an".to_string(), "-f".to_string(), "null".to_string(), "-".to_string()]));
        assert!(one.contains(&"-pass".to_string()) && one.contains(&"-map_metadata".to_string()));
        let two: Vec<String> = pass_args(src, &p, &info, 2, Path::new("C:\\t\\pass-1"), p.video_bps).iter().map(|s| s.to_string_lossy().to_string()).collect();
        assert!(two.contains(&"aac".to_string()) && two.contains(&"+faststart".to_string()));
    }

    #[test]
    fn reads_fps_and_rotation() {
        let t = "Stream #0:0: Video: h264 (High), yuv420p, 1920x1080, 17000 kb/s, 29.97 fps, 30 tbr\n    displaymatrix: rotation of -90.00 degrees";
        assert_eq!(parse_fps(t), Some(29.97));
        assert_eq!(parse_rotation(t), -90.0);
        assert_eq!(parse_fps("Audio: aac, 30 fps"), None);
        assert_eq!(parse_rotation("nothing"), 0.0);
    }

    #[test]
    fn plan_json_follows_the_page_input() {
        let info = json!({ "kind": "video", "bytes": 50_000_000, "duration": 60, "width": 1920, "height": 1080, "fps": 30, "audio": true });
        let v = plan_json(&info, &json!(10)).unwrap();
        assert_eq!(v["fits"], json!(false));
        assert_eq!(v["target_bytes"], json!(10_000_000));
        assert_eq!(v["out_height"], json!(480));
        assert_eq!(plan_json(&json!({ "kind": "image", "bytes": 5_000 }), &json!(10)).unwrap()["fits"], json!(true));
        assert!(plan_json(&info, &json!("x")).is_err());
    }

    /// A picture with detail (so JPEG can't squash it for free).
    fn busy(w: u32, h: u32) -> RgbaImage {
        let mut seed = 12345u32;
        RgbaImage::from_fn(w, h, |x, y| {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let n = (seed >> 24) as u8;
            image::Rgba([(x * 255 / w) as u8 ^ (n & 31), (y * 255 / h) as u8 ^ (n & 31), n, 255])
        })
    }

    #[test]
    fn pictures_land_under_the_limit() {
        let img = busy(640, 480);
        let go = || Ok(());
        for target in [200_000u64, 60_000, 15_000] {
            let s = shrink_picture(&img, false, false, target, &go).ok().expect("fits");
            assert!(s.bytes.len() as u64 <= target, "{} > {target}", s.bytes.len());
            assert!(matches!(s.fmt, Fmt::Jpeg));
            let back = image::load_from_memory(&s.bytes).unwrap();
            assert_eq!((back.width(), back.height()), (s.width, s.height));
        }
        // a tight limit means a smaller picture, a roomy one keeps the full size
        assert_eq!(shrink_picture(&img, false, false, 15_000, &go).ok().unwrap().width < 640, true);
        assert_eq!(shrink_picture(&img, false, false, 5_000_000, &go).ok().unwrap().width, 640);
    }

    #[test]
    fn a_flat_png_stays_a_png_and_impossible_limits_fail() {
        let flat = RgbaImage::from_pixel(200, 200, image::Rgba([10, 120, 200, 255]));
        let s = shrink_picture(&flat, false, true, 50_000, &|| Ok(())).ok().unwrap();
        assert!(matches!(s.fmt, Fmt::Png));
        assert!(shrink_picture(&busy(300, 300), false, false, 100, &|| Ok(())).is_err());
        assert!(matches!(shrink_picture(&busy(300, 300), false, false, 20_000, &|| Err(JobError::Cancelled)), Err(JobError::Cancelled)));
    }

    #[test]
    fn names_never_collide_and_wipe_only_touches_a_shrink_folder() {
        let t = Temp::new();
        let a = unique_name(&t.0, "holiday", "mp4");
        assert_eq!(file_name(&a), "holiday (shrunk).mp4");
        std::fs::write(&a, b"x").unwrap();
        assert_eq!(file_name(&unique_name(&t.0, "holiday", "mp4")), "holiday (shrunk) (2).mp4");

        let other = t.0.join("not-shrink");
        std::fs::create_dir(&other).unwrap();
        std::fs::write(other.join("keep.txt"), b"x").unwrap();
        wipe_dir(&other);
        assert!(other.join("keep.txt").exists(), "only a folder called shrink is wiped");

        let shrink = t.0.join("shrink");
        std::fs::create_dir(&shrink).unwrap();
        std::fs::write(shrink.join("a (shrunk).mp4"), b"x").unwrap();
        wipe_dir(&shrink);
        assert!(!shrink.exists());
        assert!(a.exists(), "files outside are untouched");
    }

    #[test]
    fn only_real_pictures_and_videos_are_accepted() {
        let t = Temp::new();
        assert!(input_kind(&t.0).is_err(), "a folder");
        assert!(input_kind(&t.0.join("missing.mp4")).is_err());
        let txt = t.0.join("notes.txt");
        std::fs::write(&txt, b"x").unwrap();
        assert!(input_kind(&txt).is_err());
        let jpg = t.0.join("a.jpg");
        std::fs::write(&jpg, b"x").unwrap();
        assert_eq!(input_kind(&jpg), Ok("image"));
        let mkv = t.0.join("a.mkv");
        std::fs::write(&mkv, b"x").unwrap();
        assert_eq!(input_kind(&mkv), Ok("video"));
    }
}
