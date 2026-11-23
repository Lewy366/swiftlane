//! The tools. Each module ports one or more of the Python version's tools (Source/toolkit and the
//! matching methods in Source/swiftlane.py) and offers
//!     pub fn dispatch(app: &AppHandle, method: &str, args: &[Value]) -> Option<Value>
//! answering the page's App.api.<method>(...) calls it knows (None for the ones it doesn't).
//! Shared helpers for all tools live here.

use serde_json::Value;
use std::path::{Path, PathBuf};
use tauri::AppHandle;
use tauri_plugin_dialog::DialogExt;

pub mod diskmap;
pub mod vault;
mod pdf;
mod exif;
pub mod images;
mod webp;
mod convert;
mod trimmer;
mod downloader;
mod currency;
mod speedtest;
pub mod casting;
mod adblock;
mod netscan;
mod exposure;
pub mod remote;
pub mod calc;
pub mod explorer;

pub fn dispatch(app: &AppHandle, method: &str, args: &[Value]) -> Option<Value> {
    let tools: &[fn(&AppHandle, &str, &[Value]) -> Option<Value>] = &[
        // each tool module adds its dispatch function here
        diskmap::dispatch,
        vault::dispatch,
        pdf::dispatch,
        images::dispatch,
        convert::dispatch,
        trimmer::dispatch,
        downloader::dispatch,
        currency::dispatch,
        speedtest::dispatch,
        casting::dispatch,
        adblock::dispatch,
        netscan::dispatch,
        exposure::dispatch,
        remote::dispatch,
        calc::dispatch,
        explorer::dispatch,
    ];
    tools.iter().find_map(|f| f(app, method, args))
}

/// Argument `i` of a page call (Null if missing).
pub fn arg(args: &[Value], i: usize) -> &Value {
    args.get(i).unwrap_or(&Value::Null)
}

/// Windows' "Save as" box, pre-filled. None if cancelled. Windows asks before replacing a file.
pub fn ask_save(app: &AppHandle, directory: &Path, filename: &str, exts: &[&str]) -> Option<PathBuf> {
    let mut d = app.dialog().file().set_directory(directory).set_file_name(filename);
    for e in exts {
        d = d.add_filter(format!("{} file", e.to_uppercase()), &[*e]);
    }
    d.add_filter("All files", &["*"]).blocking_save_file().and_then(|p| p.into_path().ok())
}

/// Windows' folder picker. None if cancelled.
pub fn ask_folder(app: &AppHandle, directory: &Path) -> Option<PathBuf> {
    app.dialog().file().set_directory(directory).blocking_pick_folder().and_then(|p| p.into_path().ok())
}

/// A helper program shipped next to Swiftlane.exe (e.g. "ffmpeg", "ffprobe", "yt-dlp"), or found
/// on PATH. Never runs anything from the current folder.
pub fn program(name: &str) -> Option<PathBuf> {
    let file = if cfg!(windows) { format!("{name}.exe") } else { name.to_string() };
    if let Some(dir) = std::env::current_exe().ok().and_then(|e| e.parent().map(Path::to_path_buf)) {
        for candidate in [dir.join(&file), dir.join("tools").join(&file)] {
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).filter(|d| d.is_absolute()).map(|d| d.join(&file)).find(|p| p.is_file())
}

/// A Command for a helper program that never opens a console window.
pub fn command(program: &Path) -> std::process::Command {
    #[allow(unused_mut)]
    let mut c = std::process::Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    c
}

/// The folder where received/converted files go by default (Settings > Save received files to).
#[allow(dead_code)] // for tools still to be ported
pub fn save_dir(app: &AppHandle) -> PathBuf {
    let st = tauri::Manager::state::<crate::api::AppState>(app);
    let cfg = st.cfg.lock().unwrap();
    PathBuf::from(cfg["save_dir"].as_str().unwrap_or(""))
}
