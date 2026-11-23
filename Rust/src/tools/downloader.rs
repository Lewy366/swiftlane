//! The video/audio downloader (ported from Source/toolkit/downloader.py and the matching methods in
//! Source/swiftlane.py), for YouTube and the many other sites yt-dlp supports.
//!
//! Page calls: video_info(url), download(url, choice, playlist, video_title).
//! The Python version used yt-dlp as a library; here the yt-dlp helper program (found with
//! program(), never from the current folder) runs with an argument list, never a shell. Links are
//! untrusted: only http(s) links are accepted and "--" stops one from being read as an option.
//! Uses the FFmpeg helper to merge video+audio and make MP3/M4A, and Node.js (if installed),
//! which YouTube now needs for some formats. Only download things you have the right to.

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;
use tauri::{AppHandle, Manager};

use super::trimmer::{capture, stream};
use super::{arg, ask_folder, ask_save, command, program};
use crate::jobs::{Ctx, JobError};

const NO_YTDLP: &str = "Downloading needs the yt-dlp helper program next to Swiftlane.";
const NO_FFMPEG: &str = "Downloading needs the FFmpeg helper program next to Swiftlane.";
const UNSUPPORTED: &str = "That site or link isn't supported.";

/// (key, label, yt-dlp options), as in Python's CHOICES.
const CHOICES: [(&str, &str, &[&str]); 6] = [
    ("best", "Best quality video", &["-f", "bv*+ba/b", "-S", "res,fps,vcodec:h264,acodec:m4a", "--merge-output-format", "mp4"]),
    ("1080", "1080p video", &["-f", "bv*+ba/b", "-S", "res:1080,fps,vcodec:h264,acodec:m4a", "--merge-output-format", "mp4"]),
    ("720", "720p video", &["-f", "bv*+ba/b", "-S", "res:720,fps,vcodec:h264,acodec:m4a", "--merge-output-format", "mp4"]),
    ("480", "480p video", &["-f", "bv*+ba/b", "-S", "res:480,fps,vcodec:h264,acodec:m4a", "--merge-output-format", "mp4"]),
    ("mp3", "Audio only (MP3)", &["-f", "ba/b", "-x", "--audio-format", "mp3", "--audio-quality", "0"]),
    ("m4a", "Audio only (M4A)", &["-f", "ba[ext=m4a]/ba/b", "-x", "--audio-format", "m4a"]),
];

// What yt-dlp prints for Swiftlane (progress templates and the saved file names).
const DL_TAG: &str = "swiftlane-dl|";
const PP_TAG: &str = "swiftlane-pp|";
const FILE_TAG: &str = "swiftlane-file|";

pub fn dispatch(app: &AppHandle, method: &str, args: &[Value]) -> Option<Value> {
    Some(match method {
        "video_info" => get_info(arg(args, 0).as_str().unwrap_or("")).unwrap_or_else(|e| json!({ "error": e })),
        "download" => download(app, args),
        _ => return None,
    })
}

/// The file type a choice produces (video is always merged into MP4).
fn extension(choice: &str) -> &'static str {
    match choice {
        "mp3" => "mp3",
        "m4a" => "m4a",
        _ => "mp4",
    }
}

/// A link the user pasted, checked: http(s) only ("youtube.com/..." gets https:// in front), no
/// spaces or control characters. Anything else (file:, ytsearch:, ...) isn't accepted.
fn check_url(raw: &str) -> Result<String, String> {
    let u = raw.trim();
    if u.is_empty() || u.len() > 4096 || u.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err(UNSUPPORTED.into());
    }
    let scheme_end = u.find("://").filter(|&i| i > 0 && u[..i].chars().all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c)));
    let (scheme, rest) = match scheme_end {
        Some(i) => (u[..i].to_ascii_lowercase(), &u[i + 3..]),
        None => {
            // "name:..." before any "/" is another kind of link (file:, ytsearch:, javascript:...)
            let head = &u[..u.find(['/', '?', '#']).unwrap_or(u.len())];
            if let Some(c) = head.find(':') {
                if !head[..c].contains('.') {
                    return Err(UNSUPPORTED.into());
                }
            }
            ("https".to_string(), u)
        }
    };
    if scheme != "http" && scheme != "https" {
        return Err(UNSUPPORTED.into());
    }
    let host = &rest[..rest.find(['/', '?', '#']).unwrap_or(rest.len())];
    let host = host.rsplit('@').next().unwrap_or("");
    if host.is_empty() || host.starts_with(':') || host.starts_with('-') {
        return Err(UNSUPPORTED.into());
    }
    Ok(format!("{scheme}://{rest}"))
}

/// yt-dlp's error, in plain words (like Python's _friendly_error).
fn friendly_error(stderr: &str) -> String {
    let line = stderr
        .lines()
        .rev()
        .find(|l| l.contains("ERROR:"))
        .or_else(|| stderr.lines().rev().find(|l| !l.trim().is_empty()))
        .unwrap_or("yt-dlp stopped without saying why.");
    let text = line.replace("ERROR: ", "");
    if text.contains("Unsupported URL") {
        return UNSUPPORTED.into();
    }
    if text.contains("Video unavailable") || text.contains("Private video") {
        return "That video isn't available (private, removed or region-locked).".into();
    }
    text.trim().chars().take(300).collect()
}

/// yt-dlp with the options every run gets.
fn ytdlp(exe: &Path, ffmpeg: Option<&Path>) -> Command {
    let mut c = command(exe);
    // run it from its own folder, so nothing it looks for is taken from wherever Swiftlane started
    if let Some(dir) = exe.parent() {
        c.current_dir(dir);
    }
    c.env("PYTHONIOENCODING", "utf-8").env("PYTHONUTF8", "1");
    if let Some(ff) = ffmpeg {
        c.arg("--ffmpeg-location").arg(ff);
    }
    // --no-cache-dir: don't leave a yt-dlp cache folder in the user profile; --ignore-config: no
    // settings files (the Python version, using yt-dlp as a library, didn't read them either)
    c.args(["--no-mtime", "--windows-filenames", "--no-colors", "--no-cache-dir", "--ignore-config", "--no-warnings", "--encoding", "utf-8"]);
    if let Some(node) = program("node") {
        let mut v = std::ffi::OsString::from("node:");
        v.push(node.as_os_str());
        c.arg("--js-runtimes").arg(v);
    }
    c
}

fn thumb(info: &Value) -> Value {
    let t = info["thumbnail"].as_str().filter(|s| !s.is_empty()).map(String::from).or_else(|| {
        info["thumbnails"].as_array().and_then(|a| a.last()).and_then(|t| t["url"].as_str()).map(String::from)
    });
    // only web pictures, shown as they are on the page
    match t {
        Some(u) if u.starts_with("https://") || u.starts_with("http://") => json!(u),
        _ => Value::Null,
    }
}

/// The page's view of yt-dlp's JSON.
fn info_from_json(info: &Value) -> Value {
    let site = info["extractor_key"].as_str().unwrap_or("");
    if info["_type"].as_str() == Some("playlist") {
        let title = info["title"].as_str().filter(|s| !s.is_empty()).unwrap_or("Playlist");
        return json!({ "title": title, "uploader": info["uploader"].as_str().filter(|s| !s.is_empty()).unwrap_or(""),
            "duration": null, "thumbnail": thumb(info), "playlist": true,
            "count": info["entries"].as_array().map(|a| a.len()).unwrap_or(0), "site": site });
    }
    let uploader = info["uploader"].as_str().filter(|s| !s.is_empty()).or(info["channel"].as_str().filter(|s| !s.is_empty())).unwrap_or("");
    json!({ "title": info["title"], "uploader": uploader, "duration": info["duration"], "thumbnail": thumb(info),
        "playlist": false, "count": 1, "site": site })
}

/// Title, channel, length and thumbnail, without downloading.
fn get_info(raw: &str) -> Result<Value, String> {
    let url = check_url(raw)?;
    let exe = program("yt-dlp").ok_or(NO_YTDLP)?;
    let ff = program("ffmpeg");
    let mut c = ytdlp(&exe, ff.as_deref());
    c.args(["--flat-playlist", "--skip-download", "--dump-single-json", "--", url.as_str()]);
    let out = capture(c, Duration::from_secs(120), true)?;
    let parsed = serde_json::from_slice::<Value>(&out.stdout).ok().filter(|v| v.is_object());
    match parsed {
        Some(info) if out.status.success() => Ok(info_from_json(&info)),
        _ => Err(friendly_error(&String::from_utf8_lossy(&out.stderr))),
    }
}

// ---------------------------------------------------------------- downloading

fn mb(n: f64) -> String {
    format!("{:.1} MB", n / 1024.0 / 1024.0)
}

/// Something yt-dlp printed that Swiftlane asked for.
#[derive(Debug, PartialEq)]
enum Event {
    Download { status: String, done: f64, total: f64, speed: f64 },
    Post { status: String, postprocessor: String },
    File(String),
}

fn field(s: Option<&str>) -> f64 {
    s.and_then(|x| x.trim().parse::<f64>().ok()).filter(|x| x.is_finite() && *x > 0.0).unwrap_or(0.0)
}

fn parse_line(line: &str) -> Option<Event> {
    if let Some(i) = line.find(FILE_TAG) {
        let path = line[i + FILE_TAG.len()..].trim_end();
        return (!path.is_empty()).then(|| Event::File(path.to_string()));
    }
    if let Some(i) = line.find(DL_TAG) {
        let mut p = line[i + DL_TAG.len()..].split('|');
        let status = p.next()?.trim().to_string();
        let done = field(p.next());
        let total = field(p.next());
        let estimate = field(p.next());
        let speed = field(p.next());
        return Some(Event::Download { status, done, total: if total > 0.0 { total } else { estimate }, speed });
    }
    if let Some(i) = line.find(PP_TAG) {
        let mut p = line[i + PP_TAG.len()..].split('|');
        let status = p.next()?.trim().to_string();
        let postprocessor = p.next().unwrap_or("").trim().to_string();
        return Some(Event::Post { status, postprocessor });
    }
    None
}

/// The progress to show for an event (None: nothing to show).
fn progress_for(e: &Event) -> Option<(Option<f64>, String)> {
    match e {
        Event::Download { status, done, total, speed } if status == "downloading" => {
            let mut text = if *total > 0.0 { format!("{} of {}", mb(*done), mb(*total)) } else { mb(*done) };
            if *speed > 0.0 {
                text += &format!(" · {}/s", mb(*speed));
            }
            Some((if *total > 0.0 { Some((done / total).min(1.0)) } else { None }, text))
        }
        Event::Download { status, .. } if status == "finished" => Some((None, "Processing…".into())),
        Event::Post { status, postprocessor } if status == "started" => Some((None, match postprocessor.as_str() {
            "Merger" => "Merging video and audio…",
            "ExtractAudio" => "Making audio file…",
            _ => "Processing…",
        }.into())),
        _ => None,
    }
}

/// yt-dlp's output name template: a playlist folder, the name picked in the Save box, or the title.
fn template(playlist: bool, name: Option<&str>) -> String {
    if playlist {
        "%(playlist_title)s/%(playlist_index)03d %(title)s.%(ext)s".into()
    } else if let Some(n) = name {
        format!("{}.%(ext)s", n.replace('%', "%%"))
    } else {
        "%(title)s.%(ext)s".into()
    }
}

/// Windows' Mark of the Web: Windows warns before opening it, Office uses Protected View and
/// SmartScreen checks programs, exactly as for a file downloaded from a website.
fn mark_from_internet(path: &Path) {
    if cfg!(windows) {
        let mut ads = path.as_os_str().to_owned();
        ads.push(":Zone.Identifier");
        let _ = std::fs::write(PathBuf::from(ads), "[ZoneTransfer]\r\nZoneId=3\r\nReferrerUrl=about:internet\r\n");
    }
}

/// Asks where to save (file name for one video, folder for a playlist), then downloads.
/// Returns the job id, or null if the user cancelled the dialog.
fn download(app: &AppHandle, args: &[Value]) -> Value {
    let url = arg(args, 0).as_str().unwrap_or("").trim().to_string();
    let Some(&(choice, label, options)) = arg(args, 1).as_str().and_then(|c| CHOICES.iter().find(|x| x.0 == c)) else { return Value::Null };
    let playlist = arg(args, 2).as_bool().unwrap_or(false);
    let chosen = {
        let st = app.state::<crate::api::AppState>();
        let cfg = st.cfg.lock().unwrap();
        cfg["download_dir"].as_str().filter(|s| !s.is_empty()).map(PathBuf::from)
    };
    let default_dir = chosen.unwrap_or_else(|| super::save_dir(app).join(if matches!(choice, "mp3" | "m4a") { "Audio" } else { "Videos" }));
    let _ = std::fs::create_dir_all(&default_dir);
    let ext = extension(choice);
    let (folder, name) = if playlist {
        let Some(folder) = ask_folder(app, &default_dir) else { return Value::Null };
        (folder, None)
    } else {
        let title = arg(args, 3).as_str().filter(|s| !s.is_empty()).unwrap_or("video");
        let suggested = crate::transfer::safe_name(title);
        let Some(out) = ask_save(app, &default_dir, &format!("{suggested}.{ext}"), &[ext]) else { return Value::Null };
        let folder = out.parent().map(Path::to_path_buf).unwrap_or_default();
        let same_ext = out.extension().is_some_and(|e| e.to_string_lossy().to_lowercase() == ext);
        let name = if same_ext { out.file_stem() } else { out.file_name() }.map(|n| n.to_string_lossy().to_string());
        (folder, name)
    };
    let title = format!("Download · {label}");
    let id = app.state::<crate::api::AppState>().jobs.start(app, "download", &title, true, move |ctx| {
        run_download(ctx, &url, options, &folder, playlist, name.as_deref())
    });
    json!(id)
}

fn run_download(ctx: &Ctx, url: &str, options: &[&str], folder: &Path, playlist: bool, name: Option<&str>) -> Result<Vec<PathBuf>, JobError> {
    let url = check_url(url).map_err(JobError::Failed)?;
    let exe = program("yt-dlp").ok_or_else(|| JobError::Failed(NO_YTDLP.into()))?;
    let ff = program("ffmpeg").ok_or_else(|| JobError::Failed(NO_FFMPEG.into()))?;
    std::fs::create_dir_all(folder)?;
    let mut c = ytdlp(&exe, Some(&ff));
    c.args(options)
        .arg("-P")
        .arg(folder)
        .arg("-o")
        .arg(template(playlist, name))
        .arg(if playlist { "--yes-playlist" } else { "--no-playlist" })
        .args(["--newline", "--progress", "--no-simulate"])
        .arg("--progress-template")
        .arg(format!("download:{DL_TAG}%(progress.status)s|%(progress.downloaded_bytes)s|%(progress.total_bytes)s|%(progress.total_bytes_estimate)s|%(progress.speed)s"))
        .arg("--progress-template")
        .arg(format!("postprocess:{PP_TAG}%(progress.status)s|%(progress.postprocessor)s"))
        .arg("--print")
        .arg(format!("after_move:{FILE_TAG}%(filepath)s"))
        .args(["--", url.as_str()]);
    let mut saved: Vec<PathBuf> = Vec::new();
    let (ok, err) = stream(c, ctx, true, |_, line| match parse_line(line) {
        Some(Event::File(p)) => {
            let p = PathBuf::from(p);
            if !saved.contains(&p) {
                saved.push(p);
            }
        }
        Some(e) => {
            if let Some((fraction, text)) = progress_for(&e) {
                ctx.report(fraction, Some(&text));
            }
        }
        None => {}
    })?;
    let saved: Vec<PathBuf> = saved.into_iter().filter(|p| p.is_file()).collect();
    // in a playlist, videos that fail are skipped (as in Python); only fail if nothing was saved
    if !ok && saved.is_empty() {
        return Err(JobError::Failed(friendly_error(&err)));
    }
    for p in &saved {
        mark_from_internet(p);
    }
    Ok(saved)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checks_links() {
        assert_eq!(check_url("  https://www.youtube.com/watch?v=jNQXAC9IVRw ").unwrap(), "https://www.youtube.com/watch?v=jNQXAC9IVRw");
        assert_eq!(check_url("HTTP://example.com/v").unwrap(), "http://example.com/v");
        assert_eq!(check_url("youtube.com/watch?v=x").unwrap(), "https://youtube.com/watch?v=x");
        assert_eq!(check_url("example.com:8080/v").unwrap(), "https://example.com:8080/v");
        assert_eq!(check_url("example.com/r?u=http://x").unwrap(), "https://example.com/r?u=http://x");
        for bad in ["", "file:///C:/Windows/win.ini", "file:C:/x", "ytsearch:cats", "javascript:alert(1)", "ftp://example.com/a",
            "--exec calc", "-o x", "https://", "https:///path", "https://a b", "https://a\nb", "https://-x.com/"] {
            assert!(check_url(bad).is_err(), "{bad:?} should be refused");
        }
    }

    #[test]
    fn explains_errors() {
        assert_eq!(friendly_error("WARNING: x\nERROR: Unsupported URL: https://a.b/\n"), UNSUPPORTED);
        assert_eq!(friendly_error("ERROR: [youtube] abc: Video unavailable"), "That video isn't available (private, removed or region-locked).");
        assert_eq!(friendly_error("ERROR: [x] y: Something else\n"), "[x] y: Something else");
        assert_eq!(friendly_error(""), "yt-dlp stopped without saying why.");
    }

    #[test]
    fn reads_progress_lines() {
        let e = parse_line("swiftlane-dl|downloading|5242880|10485760|NA|1048576.5").unwrap();
        assert_eq!(progress_for(&e), Some((Some(0.5), "5.0 MB of 10.0 MB · 1.0 MB/s".to_string())));
        let e = parse_line("swiftlane-dl|downloading|1048576|NA|2097152|NA").unwrap();
        assert_eq!(progress_for(&e), Some((Some(0.5), "1.0 MB of 2.0 MB".to_string())), "estimate when the size isn't known");
        let e = parse_line("swiftlane-dl|downloading|1048576|NA|NA|NA").unwrap();
        assert_eq!(progress_for(&e), Some((None, "1.0 MB".to_string())));
        let e = parse_line("swiftlane-dl|finished|10|10|NA|NA").unwrap();
        assert_eq!(progress_for(&e), Some((None, "Processing…".to_string())));
        let e = parse_line("swiftlane-pp|started|Merger").unwrap();
        assert_eq!(progress_for(&e), Some((None, "Merging video and audio…".to_string())));
        assert_eq!(progress_for(&parse_line("swiftlane-pp|started|ExtractAudio").unwrap()).unwrap().1, "Making audio file…");
        assert_eq!(progress_for(&parse_line("swiftlane-pp|finished|Merger").unwrap()), None);
        assert_eq!(parse_line("swiftlane-file|C:\\Videos\\a|b.mp4\r"), Some(Event::File("C:\\Videos\\a|b.mp4".into())));
        assert_eq!(parse_line("[youtube] abc: Downloading webpage"), None);
    }

    #[test]
    fn reads_info() {
        let v = info_from_json(&json!({ "title": "Me at the zoo", "channel": "jawed", "duration": 19, "thumbnails": [{ "url": "https://i/a.jpg" }, { "url": "https://i/b.jpg" }], "extractor_key": "Youtube" }));
        assert_eq!(v, json!({ "title": "Me at the zoo", "uploader": "jawed", "duration": 19, "thumbnail": "https://i/b.jpg", "playlist": false, "count": 1, "site": "Youtube" }));
        let p = info_from_json(&json!({ "_type": "playlist", "entries": [{}, {}, {}], "thumbnail": "javascript:x", "extractor_key": "YoutubeTab" }));
        assert_eq!(p, json!({ "title": "Playlist", "uploader": "", "duration": null, "thumbnail": null, "playlist": true, "count": 3, "site": "YoutubeTab" }));
    }

    #[test]
    fn names() {
        assert_eq!(template(false, Some("100% fun")), "100%% fun.%(ext)s");
        assert_eq!(template(false, None), "%(title)s.%(ext)s");
        assert!(template(true, Some("x")).starts_with("%(playlist_title)s/"));
        assert_eq!((extension("mp3"), extension("m4a"), extension("720")), ("mp3", "m4a", "mp4"));
        assert_eq!(CHOICES.iter().map(|c| c.0).collect::<Vec<_>>(), ["best", "1080", "720", "480", "mp3", "m4a"]);
    }

    #[test]
    fn marks_downloads() {
        let dir = std::env::temp_dir().join(format!("swl-dl-test-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir(&dir).unwrap();
        let f = dir.join("clip.mp4");
        std::fs::write(&f, b"x").unwrap();
        mark_from_internet(&f);
        if cfg!(windows) {
            let mut ads = f.as_os_str().to_owned();
            ads.push(":Zone.Identifier");
            assert!(std::fs::read_to_string(PathBuf::from(ads)).unwrap_or_default().contains("ZoneId=3"));
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
