//! The video trimmer (ported from Source/toolkit/trimmer.py and the matching methods in
//! Source/swiftlane.py): cut a clip fast (without re-encoding) or exactly, make it fit a size, or
//! keep only the sound.
//!
//! Page calls: trim_open(path), trim_frame(path, t), trim_save(path, start, end, mode, target_mb, height).
//! FFmpeg runs with an argument list (never a shell), so file names can't inject commands.

use base64::Engine;
use serde_json::{json, Value};
use std::io::{BufRead, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager};

use super::{arg, ask_save, command, program};
use crate::jobs::{Ctx, JobError};

const NO_FFMPEG: &str = "Can't open videos yet: that needs the FFmpeg helper program next to Swiftlane.";

pub fn dispatch(app: &AppHandle, method: &str, args: &[Value]) -> Option<Value> {
    Some(match method {
        "trim_open" => trim_open(arg(args, 0).as_str().unwrap_or("")),
        "trim_frame" => {
            let path = arg(args, 0).as_str().unwrap_or("");
            match (program("ffmpeg"), num(arg(args, 1))) {
                (Some(ff), Some(t)) => frame(&ff, Path::new(path), t, 360).map(Value::String).unwrap_or(Value::Null),
                _ => Value::Null,
            }
        }
        "trim_save" => trim_save(app, args),
        _ => return None,
    })
}

// ---------------------------------------------------------------- running FFmpeg (shared with the downloader)

/// Reads everything a pipe gives, on its own thread.
fn drain<R: Read + Send + 'static>(r: Option<R>) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut r) = r {
            let _ = r.read_to_end(&mut buf);
        }
        buf
    })
}

/// Stops a helper program. `tree`: also its own helpers (yt-dlp.exe starts a second copy of itself
/// and FFmpeg), via Windows' taskkill from the System32 folder.
pub(super) fn kill(child: &mut Child, tree: bool) {
    #[cfg(windows)]
    {
        if tree {
            let root = std::env::var_os("SystemRoot").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
            let _ = command(&root.join("System32").join("taskkill.exe"))
                .args(["/T", "/F", "/PID"])
                .arg(child.id().to_string())
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    }
    #[cfg(not(windows))]
    let _ = tree;
    let _ = child.kill();
    let _ = child.wait();
}

/// Runs a helper program to the end and returns what it printed. Stopped after `timeout`.
pub(super) fn capture(mut cmd: Command, timeout: Duration, tree: bool) -> Result<Output, String> {
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| format!("The helper program didn't start ({e})."))?;
    let out = drain(child.stdout.take());
    let err = drain(child.stderr.take());
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                kill(&mut child, tree);
                return Err("The helper program took too long, so Swiftlane stopped it.".into());
            }
        }
    };
    Ok(Output { status, stdout: out.join().unwrap_or_default(), stderr: err.join().unwrap_or_default() })
}

/// Runs a helper program in a job: hands every line it prints to `on_line` (true = from stderr)
/// and kills it as soon as the user presses Cancel. Returns (finished fine, the end of stderr).
pub(super) fn stream(mut cmd: Command, ctx: &Ctx, tree: bool, mut on_line: impl FnMut(bool, &str)) -> Result<(bool, String), JobError> {
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| JobError::Failed(format!("The helper program didn't start ({e}).")))?;
    let (tx, rx) = mpsc::channel::<(bool, String)>();
    fn lines<R: Read + Send + 'static>(r: Option<R>, is_err: bool, tx: mpsc::Sender<(bool, String)>) {
        if let Some(r) = r {
            std::thread::spawn(move || {
                for chunk in std::io::BufReader::new(r).split(b'\n') {
                    let Ok(chunk) = chunk else { break };
                    let text = String::from_utf8_lossy(&chunk).trim_end_matches('\r').to_string();
                    if tx.send((is_err, text)).is_err() {
                        break;
                    }
                }
            });
        }
    }
    lines(child.stdout.take(), false, tx.clone());
    lines(child.stderr.take(), true, tx);
    let mut tail = String::new();
    loop {
        if ctx.cancelled() {
            kill(&mut child, tree);
            return Err(JobError::Cancelled);
        }
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok((is_err, line)) => {
                if is_err {
                    tail.push_str(&line);
                    tail.push('\n');
                    if tail.len() > 16_000 {
                        let cut = tail.len() - 8_000;
                        let cut = (cut..tail.len()).find(|i| tail.is_char_boundary(*i)).unwrap_or(0);
                        tail.drain(..cut);
                    }
                }
                on_line(is_err, &line);
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    let status = loop {
        if ctx.cancelled() {
            kill(&mut child, tree);
            return Err(JobError::Cancelled);
        }
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(e) => {
                kill(&mut child, tree);
                return Err(JobError::Failed(e.to_string()));
            }
        }
    };
    Ok((status.success(), tail))
}

/// "file:" stops FFmpeg from reading a name as an option or a network address.
fn file_arg(path: &Path) -> std::ffi::OsString {
    let mut s = std::ffi::OsString::from("file:");
    s.push(path.as_os_str());
    s
}

fn ffmpeg(ff: &Path) -> Command {
    let mut c = command(ff);
    c.args(["-hide_banner", "-nostdin"]);
    c
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

// ---------------------------------------------------------------- reading a video

/// Leading ASCII digits of `s` (at most `max`), as (digits, rest).
fn digits(s: &str, max: usize) -> (&str, &str) {
    let n = s.bytes().take(max).take_while(u8::is_ascii_digit).count();
    s.split_at(n)
}

/// FFmpeg's "Duration: 00:01:02.50" in seconds (the first one that can be read).
fn parse_duration(text: &str) -> Option<f64> {
    text.match_indices("Duration: ").find_map(|(i, m)| {
        let s = &text[i + m.len()..];
        let (h, s) = digits(s, usize::MAX);
        let s = s.strip_prefix(':').filter(|_| !h.is_empty())?;
        let (m, s) = digits(s, usize::MAX);
        let s = s.strip_prefix(':').filter(|_| !m.is_empty())?;
        let (sec, s) = digits(s, usize::MAX);
        if sec.is_empty() {
            return None;
        }
        let mut secs = sec.to_string();
        if let Some(frac) = s.strip_prefix('.') {
            let (f, _) = digits(frac, usize::MAX);
            if !f.is_empty() {
                secs = format!("{sec}.{f}");
            }
        }
        Some(h.parse::<f64>().ok()? * 3600.0 + m.parse::<f64>().ok()? * 60.0 + secs.parse::<f64>().ok()?)
    })
}

/// The first "1920x1080" after "Video:" on the same line (like Python's r"Video:.*?(\d{2,5})x(\d{2,5})").
fn parse_size(text: &str) -> Option<(u32, u32)> {
    text.lines().filter_map(|l| l.find("Video:").map(|i| &l[i + 6..])).find_map(|rest| {
        let b = rest.as_bytes();
        for start in 0..b.len() {
            for len in (2..=5).rev() {
                if start + len >= b.len() || !b[start..start + len].iter().all(u8::is_ascii_digit) || b[start + len] != b'x' {
                    continue;
                }
                let (h, _) = digits(&rest[start + len + 1..], 5);
                if h.len() >= 2 {
                    return Some((rest[start..start + len].parse().ok()?, h.parse().ok()?));
                }
            }
        }
        None
    })
}

/// (duration, width, height, has sound) from what `ffmpeg -i` prints.
fn parse_probe(text: &str) -> Option<(f64, u32, u32, bool)> {
    let duration = parse_duration(text)?;
    if !text.contains("Video:") {
        return None;
    }
    let (w, h) = parse_size(text).unwrap_or((0, 0));
    Some((duration, w, h, text.contains("Audio:")))
}

fn probe(ff: &Path, path: &Path) -> Result<Value, String> {
    let mut c = ffmpeg(ff);
    c.arg("-i").arg(file_arg(path));
    let out = capture(c, Duration::from_secs(30), false)?;
    let err = String::from_utf8_lossy(&out.stderr);
    let (duration, width, height, audio) = parse_probe(&err).ok_or("Swiftlane can't read a video in this file.")?;
    let bytes = std::fs::metadata(path).map_err(|e| e.to_string())?.len();
    let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    Ok(json!({ "duration": duration, "width": width, "height": height, "audio": audio, "bytes": bytes, "name": name }))
}

/// One picture from the video at second t, as a data: URL.
fn frame(ff: &Path, path: &Path, t: f64, height: u32) -> Option<String> {
    let mut c = ffmpeg(ff);
    c.args(["-ss", &format!("{:.3}", t.max(0.0)), "-i"])
        .arg(file_arg(path))
        .args(["-frames:v", "1", "-vf", &format!("scale=-2:{height}"), "-f", "image2pipe", "-vcodec", "mjpeg", "-q:v", "5", "-"]);
    let out = capture(c, Duration::from_secs(30), false).ok()?;
    if out.stdout.is_empty() {
        return None;
    }
    Some(format!("data:image/jpeg;base64,{}", base64::engine::general_purpose::STANDARD.encode(&out.stdout)))
}

fn trim_open(path: &str) -> Value {
    let Some(ff) = program("ffmpeg") else { return json!({ "error": NO_FFMPEG }) };
    let p = Path::new(path);
    let mut info = match probe(&ff, p) {
        Ok(v) => v,
        Err(e) => return json!({ "error": if e.is_empty() { "Can't read this video.".to_string() } else { e } }),
    };
    let duration = info["duration"].as_f64().unwrap_or(0.0);
    let count = 10;
    let strip: Vec<Value> = (0..count)
        .map(|i| frame(&ff, p, duration * (i as f64 + 0.5) / count as f64, 64).map(Value::String).unwrap_or(Value::Null))
        .collect();
    info["path"] = json!(path);
    info["strip"] = json!(strip);
    info["preview"] = frame(&ff, p, 0.0, 360).map(Value::String).unwrap_or(Value::Null);
    info
}

// ---------------------------------------------------------------- cutting

/// Video bitrate (bits/s) and output height so the clip lands just under target_mb.
fn plan_size(duration: f64, target_mb: f64, height: f64) -> (i64, f64) {
    let total = target_mb * 8.0 * 1024.0 * 1024.0 * 0.95;
    let video = (total / duration.max(0.1) - 128_000.0).max(120_000.0);
    let mut out_h = height;
    if video < 700_000.0 && height > 480.0 {
        out_h = 480.0;
    } else if video < 1_800_000.0 && height > 720.0 {
        out_h = 720.0;
    }
    (video as i64, out_h)
}

/// FFmpeg's options for a cut (before the output name), or None for an unknown mode.
fn trim_args(input: &Path, start: f64, length: f64, mode: &str, target_mb: f64, height: f64) -> Option<Vec<std::ffi::OsString>> {
    let mut a: Vec<std::ffi::OsString> = Vec::new();
    let mut add = |items: &[&str]| a.extend(items.iter().map(std::ffi::OsString::from));
    add(&["-y", "-ss", &format!("{start:.3}"), "-i"]);
    drop(add);
    a.push(file_arg(input));
    let mut rest: Vec<String> = vec!["-t".into(), format!("{length:.3}")];
    let mut more = |items: &[&str]| rest.extend(items.iter().map(|s| s.to_string()));
    match mode {
        "fast" => more(&["-map", "0:v:0?", "-map", "0:a?", "-c", "copy", "-avoid_negative_ts", "make_zero"]),
        "exact" => more(&["-c:v", "libx264", "-preset", "medium", "-crf", "20", "-c:a", "aac", "-b:a", "160k", "-movflags", "+faststart"]),
        "size" => {
            let (rate, out_h) = plan_size(length, target_mb, height);
            more(&["-c:v", "libx264", "-preset", "medium", "-b:v", &rate.to_string(), "-maxrate", &((rate as f64 * 1.3) as i64).to_string(),
                "-bufsize", &(rate * 2).to_string(), "-c:a", "aac", "-b:a", "128k", "-movflags", "+faststart"]);
            if out_h > 0.0 && height > 0.0 && out_h < height {
                more(&["-vf", &format!("scale=-2:{}", out_h as i64)]);
            }
        }
        "audio" => more(&["-vn", "-c:a", "libmp3lame", "-b:a", "192k"]),
        _ => return None,
    }
    a.extend(rest.into_iter().map(std::ffi::OsString::from));
    Some(a)
}

/// "out_time_us=1234567" from FFmpeg's -progress output, in seconds.
fn parse_out_time(line: &str) -> Option<f64> {
    let (d, _) = digits(line.strip_prefix("out_time_us=")?, usize::MAX);
    if d.is_empty() { None } else { d.parse::<f64>().ok().map(|us| us / 1e6) }
}

/// The type of file a cut makes.
fn output_ext(mode: &str, src: &Path) -> String {
    if mode == "audio" {
        return "mp3".into();
    }
    let ext = if mode == "fast" { src.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default() } else { "mp4".into() };
    if ext.is_empty() { "mp4".into() } else { ext }
}

/// What the job is called, as in Python (target_mb printed as the page sent it).
fn label(mode: &str, target_mb: &Value) -> String {
    match mode {
        "fast" => "Cut".into(),
        "exact" => "Exact cut".into(),
        "size" => format!("Cut to {} MB", match target_mb {
            Value::String(s) => s.clone(),
            Value::Null => "10".into(),
            v => v.to_string(),
        }),
        "audio" => "Sound only".into(),
        _ => "Cut".into(),
    }
}

/// True if both names point at the same existing file.
fn same_file(a: &Path, b: &Path) -> bool {
    matches!((std::fs::canonicalize(a), std::fs::canonicalize(b)), (Ok(x), Ok(y)) if x == y)
}

/// Asks where to save, then cuts in the background. Returns the job id, or null if cancelled.
fn trim_save(app: &AppHandle, args: &[Value]) -> Value {
    let path = arg(args, 0).as_str().unwrap_or("").to_string();
    let mode = arg(args, 3).as_str().unwrap_or("").to_string();
    let target_v = if arg(args, 4).is_null() { json!(10) } else { arg(args, 4).clone() };
    let src = PathBuf::from(&path);
    let ext = output_ext(&mode, &src);
    let stem = src.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    let folder = src.parent().map(Path::to_path_buf).unwrap_or_default();
    let Some(out) = ask_save(app, &folder, &format!("{stem} (cut).{ext}"), &[ext.as_str()]) else { return Value::Null };
    let name = src.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    let title = format!("{} · {name}", label(&mode, &target_v));
    let start = num(arg(args, 1));
    let end = num(arg(args, 2));
    let target_mb = num(&target_v).unwrap_or(10.0);
    let height = num(arg(args, 5)).unwrap_or(0.0);
    let id = app.state::<crate::api::AppState>().jobs.start(app, "trim", &title, true, move |ctx| {
        let (Some(start), Some(end)) = (start, end) else { return Err(JobError::Failed("The start or end time can't be read.".into())) };
        trim(ctx, &src, start, end, &out, &mode, target_mb, height)
    });
    json!(id)
}

#[allow(clippy::too_many_arguments)]
fn trim(ctx: &Ctx, src: &Path, start: f64, end: f64, output: &Path, mode: &str, target_mb: f64, height: f64) -> Result<Vec<PathBuf>, JobError> {
    let start = start.max(0.0);
    let length = end - start;
    if length <= 0.05 {
        return Err(JobError::Failed("The cut is empty: move the end after the start.".into()));
    }
    let Some(args) = trim_args(src, start, length, mode, target_mb, height) else { return Err(JobError::Failed("Unknown mode.".into())) };
    // FFmpeg would refuse, and the clean-up below must never remove the original
    if same_file(src, output) {
        return Err(JobError::Failed("Pick another name: the cut can't replace the video it's cut from.".into()));
    }
    let ff = program("ffmpeg").ok_or_else(|| JobError::Failed(NO_FFMPEG.into()))?;
    let mut c = ffmpeg(&ff);
    c.args(["-loglevel", "error", "-progress", "pipe:1", "-nostats"]).args(&args).arg(file_arg(output));
    let result = stream(c, ctx, false, |is_err, line| {
        if !is_err {
            if let Some(t) = parse_out_time(line) {
                ctx.report(Some((t / length).min(1.0)), None);
            }
        }
    });
    let remove_partial = || {
        if !same_file(src, output) {
            let _ = std::fs::remove_file(output);
        }
    };
    let (ok, err) = match result {
        Ok(r) => r,
        Err(e) => {
            remove_partial(); // a half-written clip is no use
            return Err(e);
        }
    };
    if !ok || !output.exists() {
        remove_partial();
        let tail: Vec<char> = err.trim_end().chars().collect();
        let tail: String = tail[tail.len().saturating_sub(200)..].iter().collect();
        return Err(JobError::Failed(format!("FFmpeg couldn't cut this video: {tail}")));
    }
    Ok(vec![output.to_path_buf()])
}

#[cfg(test)]
mod tests {
    use super::*;

    const PROBE: &str = "Input #0, mov,mp4,m4a,3gp,3g2,mj2, from 'file:C:\\v\\clip.mp4':\n  Duration: 00:01:02.50, start: 0.000000, bitrate: 1234 kb/s\n  Stream #0:0[0x1](und): Video: h264 (High) (avc1 / 0x31637661), yuv420p(progressive), 1920x1080 [SAR 1:1 DAR 16:9], 30 fps\n  Stream #0:1[0x2](und): Audio: aac (LC) (mp4a / 0x6134706D), 48000 Hz, stereo\nAt least one output file must be specified\n";

    #[test]
    fn reads_ffmpeg_info() {
        let (d, w, h, audio) = parse_probe(PROBE).unwrap();
        assert!((d - 62.5).abs() < 1e-9);
        assert_eq!((w, h, audio), (1920, 1080, true));
        assert_eq!(parse_duration("Duration: 01:00:00, start"), Some(3600.0));
        assert_eq!(parse_duration("Duration: N/A, bitrate: N/A"), None);
        assert!(parse_probe("Duration: 00:00:05.00\n Stream #0:0: Audio: mp3").is_none(), "sound only isn't a video");
        assert_eq!(parse_size("Video: mjpeg, 123456x789"), Some((23456, 789)));
        assert_eq!(parse_size("Video: rawvideo, 640x48"), Some((640, 48)));
        assert_eq!(parse_size("Video: none\nOther: 640x480"), None, "only on the Video: line");
    }

    #[test]
    fn reads_progress() {
        assert_eq!(parse_out_time("out_time_us=2500000"), Some(2.5));
        assert_eq!(parse_out_time("out_time_us=N/A"), None);
        assert_eq!(parse_out_time("out_time_ms=2500000"), None);
        assert_eq!(parse_out_time("progress=continue"), None);
    }

    #[test]
    fn plans_sizes_like_python() {
        // 60 s into 10 MB: 10*8*1024*1024*0.95/60 - 128000 = 1200196.27
        assert_eq!(plan_size(60.0, 10.0, 1080.0), (1_200_196, 720.0));
        assert_eq!(plan_size(600.0, 10.0, 1080.0), (120_000, 480.0));
        assert_eq!(plan_size(10.0, 64.0, 1080.0).1, 1080.0);
        assert_eq!(plan_size(0.0, 10.0, 0.0).1, 0.0);
    }

    fn strs(v: &[std::ffi::OsString]) -> Vec<String> {
        v.iter().map(|s| s.to_string_lossy().to_string()).collect()
    }

    #[test]
    fn builds_arguments() {
        let src = Path::new("C:\\v\\-clip.mp4");
        let fast = strs(&trim_args(src, 1.0, 2.5, "fast", 10.0, 1080.0).unwrap());
        assert_eq!(&fast[..6], ["-y", "-ss", "1.000", "-i", "file:C:\\v\\-clip.mp4", "-t"]);
        assert!(fast.contains(&"copy".to_string()));
        let size = strs(&trim_args(src, 0.0, 600.0, "size", 10.0, 1080.0).unwrap());
        assert!(size.ends_with(&["-vf".to_string(), "scale=-2:480".to_string()]));
        assert!(size.contains(&"156000".to_string()), "maxrate is 1.3x the bitrate");
        assert!(strs(&trim_args(src, 0.0, 5.0, "audio", 10.0, 0.0).unwrap()).contains(&"libmp3lame".to_string()));
        assert!(trim_args(src, 0.0, 5.0, "bogus", 10.0, 0.0).is_none());
    }

    #[test]
    fn names_and_labels() {
        assert_eq!(output_ext("fast", Path::new("a.MOV")), "mov");
        assert_eq!(output_ext("fast", Path::new("a")), "mp4");
        assert_eq!(output_ext("exact", Path::new("a.mov")), "mp4");
        assert_eq!(output_ext("audio", Path::new("a.mov")), "mp3");
        assert_eq!(label("size", &json!(10)), "Cut to 10 MB");
        assert_eq!(label("size", &json!(7.5)), "Cut to 7.5 MB");
        assert_eq!(label("audio", &json!(10)), "Sound only");
        assert_eq!(label("??", &json!(10)), "Cut");
    }

    #[test]
    fn same_file_check() {
        let dir = std::env::temp_dir().join(format!("swl-trim-test-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir(&dir).unwrap();
        let a = dir.join("a.mp4");
        std::fs::write(&a, b"x").unwrap();
        assert!(same_file(&a, &dir.join(".").join("a.mp4")));
        assert!(!same_file(&a, &dir.join("b.mp4")));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
