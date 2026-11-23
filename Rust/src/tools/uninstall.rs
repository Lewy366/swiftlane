//! Built-in uninstaller (`Swiftlane.exe --uninstall`), showing Source/web/uninstall.html.
//! Port of UninstallApi and run_uninstaller in Source/swiftlane.py.
//!
//! Security & safety rules:
//! - Machine-wide removal (`uninstall.ps1`) asks Windows for admin first; if declined, nothing is changed.
//! - Only files recorded in created-files.txt are ever deleted (and only when the user ticks the box).
//! - Category folders under save_dir are only removed with remove_dir (empty-only), so user files are never touched.
//! - The "uninstall" window may only call `info`, `run`, and `close`.

use serde_json::{json, Value};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Manager};

use super::{arg, command, explorer, program};
use crate::{config, funnel, winapi};

const UNINSTALL_BRIDGE: &str = r#"(() => {
  if (window.pywebview) return;
  const call = (method, args) => window.__TAURI_INTERNALS__.invoke('call', { method, args });
  window.pywebview = { api: new Proxy({}, { get: (_, m) => typeof m === 'string' ? (...a) => call(m, a) : undefined }) };
  window.addEventListener('DOMContentLoaded', () => window.dispatchEvent(new Event('pywebviewready')));
})();"#;

const CATEGORIES: &[&str] = &["Photos", "Videos", "Audio", "Documents", "Archives", "Other"];

fn install_dir() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(Path::to_path_buf))
}

/// Unique existing files recorded by Swiftlane in `created-files.txt`.
pub fn created_files(config_dir: &Path) -> Vec<PathBuf> {
    let Ok(text) = std::fs::read_to_string(config_dir.join("created-files.txt")) else {
        return Vec::new();
    };
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let p = PathBuf::from(trimmed);
        if p.is_file() && seen.insert(p.clone()) {
            out.push(p);
        }
    }
    out
}

/// Deletes only the recorded created files and any now-empty Swiftlane subfolders.
fn remove_created_and_empty_dirs(config_dir: &Path, save_dir: Option<&Path>) -> usize {
    let mut deleted = 0usize;
    for f in created_files(config_dir) {
        if std::fs::remove_file(&f).is_ok() {
            deleted += 1;
        }
    }
    if let Some(save) = save_dir {
        if !save.as_os_str().is_empty() {
            for cat in CATEGORIES {
                let _ = std::fs::remove_dir(save.join(cat)); // empty-only
            }
            let _ = std::fs::remove_dir(save); // empty-only
        }
    }
    deleted
}

#[cfg(windows)]
fn remove_run_entries() {
    use winreg::enums::{HKEY_CURRENT_USER, KEY_SET_VALUE};
    use winreg::RegKey;
    if let Ok(k) = RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey_with_flags(r"Software\Microsoft\Windows\CurrentVersion\Run", KEY_SET_VALUE)
    {
        let _ = k.delete_value("Swiftlane");
        let _ = k.delete_value("PocketDrop");
    }
}

#[cfg(not(windows))]
fn remove_run_entries() {}

fn load_saved_cfg(cfg_dir: &Path) -> Value {
    std::fs::read_to_string(cfg_dir.join("config.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .unwrap_or_else(|| json!({}))
}

fn info() -> Value {
    let cfg_dir = config::config_dir();
    let cfg = load_saved_cfg(&cfg_dir);
    let files = created_files(&cfg_dir);
    let files_size: u64 = files
        .iter()
        .filter_map(|f| std::fs::metadata(f).ok().map(|m| m.len()))
        .sum();
    let dir_str = install_dir()
        .filter(|d| d.join("uninstall.ps1").is_file())
        .map(|d| d.to_string_lossy().to_string())
        .unwrap_or_else(|| "(not installed: running from source)".into());
    json!({
        "install_dir": dir_str,
        "config_dir": cfg_dir.to_string_lossy(),
        "files": files.len(),
        "files_size": files_size,
        "language": cfg.get("language").and_then(Value::as_str).unwrap_or("en"),
    })
}

fn run_uninstall(remove_files: bool) -> Value {
    let cfg_dir = config::config_dir();
    let cfg = load_saved_cfg(&cfg_dir);

    // 1. Machine-wide removal needs admin: ask first, so cancelling changes nothing.
    if let Some(dir) = install_dir() {
        let script = dir.join("uninstall.ps1");
        if script.is_file() {
            let Some(ps) = program("powershell") else {
                return json!({ "error": "Couldn't start PowerShell to remove the program." });
            };
            let params = format!(
                "-NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File \"{}\" -WaitPid {} -InstallDir \"{}\"",
                script.display(),
                std::process::id(),
                dir.display()
            );
            let cwd = std::env::temp_dir();
            if !winapi::run_elevated(&ps, &params, &cwd) {
                return json!({ "error": "Windows didn’t give permission, so nothing was removed." });
            }
        }
    }

    // 2. Stop any other Swiftlane.exe instances.
    if cfg!(windows) {
        if let Some(tk) = program("taskkill") {
            let _ = command(&tk)
                .args(["/F", "/IM", "Swiftlane.exe", "/FI", &format!("PID ne {}", std::process::id())])
                .output();
        }
    }

    // 3. Turn off Tailscale Funnel if active.
    funnel::off();

    // 4. Optional deletion of files Swiftlane created.
    let deleted = if remove_files {
        let save_dir = cfg
            .get("save_dir")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(PathBuf::from);
        remove_created_and_empty_dirs(&cfg_dir, save_dir.as_deref())
    } else {
        0
    };

    // 5. Per-user startup, desktop shortcut, Explorer menu, settings & cache.
    remove_run_entries();
    if let Some(desk) = dirs::desktop_dir() {
        let _ = std::fs::remove_file(desk.join("Swiftlane.lnk"));
    }
    explorer::disable_menu();
    let _ = std::fs::remove_dir_all(&cfg_dir);
    if let Some(appdata) = std::env::var_os("APPDATA") {
        let _ = std::fs::remove_dir_all(PathBuf::from(appdata).join("PocketDrop"));
    }
    if let Some(home) = dirs::home_dir() {
        let _ = std::fs::remove_dir_all(home.join(".cache").join("yt-dlp"));
    }

    json!({ "ok": true, "deleted_files": deleted })
}

/// Opens the dedicated uninstall window and closes the main window.
pub fn open_window(app: &AppHandle) -> Result<(), String> {
    let built = tauri::WebviewWindowBuilder::new(
        app,
        "uninstall",
        tauri::WebviewUrl::App("uninstall.html".into()),
    )
    .title("Uninstall Swiftlane")
    .inner_size(720.0, 640.0)
    .background_color(tauri::window::Color(7, 7, 13, 255))
    .initialization_script(UNINSTALL_BRIDGE)
    .build();
    built.map_err(|e| e.to_string())?;
    if let Some(main) = app.get_webview_window("main") {
        let _ = main.destroy();
    }
    Ok(())
}

/// Calls from the "uninstall" window: only `info`, `run`, and `close`.
pub fn window_call(app: &AppHandle, method: &str, args: &[Value]) -> Option<Value> {
    Some(match method {
        "info" => info(),
        "run" => run_uninstall(arg(args, 0).as_bool().unwrap_or(false)),
        "close" => {
            if let Some(w) = app.get_webview_window("uninstall") {
                let _ = w.destroy();
            }
            Value::Null
        }
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh_dir() -> PathBuf {
        let mut b = [0u8; 6];
        getrandom::getrandom(&mut b).unwrap();
        let dir = std::env::temp_dir().join(format!("swl-uninst-test-{}", hex::encode(b)));
        std::fs::create_dir(&dir).unwrap();
        dir
    }

    #[test]
    fn removes_only_recorded_files_and_empty_category_dirs() {
        let dir = fresh_dir();
        let cfg_dir = dir.join("cfg");
        let save_dir = dir.join("save");
        std::fs::create_dir_all(save_dir.join("Photos")).unwrap();
        std::fs::create_dir_all(save_dir.join("Videos")).unwrap();
        std::fs::create_dir_all(&cfg_dir).unwrap();

        let made_photo = save_dir.join("Photos").join("from-phone.jpg");
        let user_photo = save_dir.join("Photos").join("my-own-file.jpg");
        let made_video = save_dir.join("Videos").join("clip.mp4");
        std::fs::write(&made_photo, b"123").unwrap();
        std::fs::write(&user_photo, b"keep-me").unwrap();
        std::fs::write(&made_video, b"456").unwrap();

        // Duplicate entry + missing entry should be handled cleanly
        let list = format!(
            "{}\n{}\n{}\n{}\n",
            made_photo.display(),
            made_photo.display(),
            made_video.display(),
            save_dir.join("missing.bin").display()
        );
        std::fs::write(cfg_dir.join("created-files.txt"), list).unwrap();

        let found = created_files(&cfg_dir);
        assert_eq!(found.len(), 2);

        let deleted = remove_created_and_empty_dirs(&cfg_dir, Some(&save_dir));
        assert_eq!(deleted, 2);
        assert!(!made_photo.exists());
        assert!(!made_video.exists());
        assert!(user_photo.exists(), "user's own file must never be deleted");
        assert!(save_dir.join("Photos").exists(), "non-empty folder must stay");
        assert!(!save_dir.join("Videos").exists(), "empty category folder should be removed");

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
