//! Swiftlane Setup: native Rust/Tauri 2 install wizard window.
//!
//! Displays `Source/web/setup.html` + `Source/web/setup/setup.js` and drives `installer.rs`.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod installer;

use installer::{InstallResult, PayloadSource, RegHive, SystemOps, WindowsSystem, APP, RUN_KEY};
use serde_json::{json, Map, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use tauri::{AppHandle, Manager, WebviewWindow};
use tauri_plugin_dialog::DialogExt;

const RESTARTED_ENV: &str = "SWIFTLANE_SETUP_RESTARTED";

const FEATURES: &[&str] = &[
    "transfer",
    "share",
    "contacts",
    "findfiles",
    "duplicates",
    "diskmap",
    "converter",
    "images",
    "pdf",
    "trimmer",
    "downloader",
    "vault",
    "netscan",
    "exposure",
    "startup",
    "biosusb",
    "qrstudio",
    "worldclock",
    "airplay",
    "remote",
    "currency",
    "calculator",
    "speedtest",
    "adblock",
];

struct SetupState {
    update_flag: bool,
    payload: Option<PayloadSource>,
    version: String,
    result: Mutex<Option<InstallResult>>,
    installing: AtomicBool,
}

impl SetupState {
    fn new(update_flag: bool) -> Self {
        let payload = PayloadSource::resolve().ok();
        let version = payload
            .as_ref()
            .map(|p| p.version().to_string())
            .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string());
        Self {
            update_flag,
            payload,
            version,
            result: Mutex::new(None),
            installing: AtomicBool::new(false),
        }
    }
}

/// Returns en/de/pl/it from the Windows display language.
#[cfg(windows)]
fn windows_language() -> &'static str {
    let primary = unsafe { windows_sys::Win32::Globalization::GetUserDefaultUILanguage() } & 0x3FF;
    match primary {
        0x07 => "de",
        0x15 => "pl",
        0x10 => "it",
        _ => "en",
    }
}

#[cfg(not(windows))]
fn windows_language() -> &'static str {
    "en"
}

/// Free disk bytes available on the volume containing `path`.
#[cfg(windows)]
fn free_disk_space(path: &Path) -> u64 {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;

    let root = path
        .ancestors()
        .last()
        .filter(|p| !p.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from(r"C:\"));
    let wide: Vec<u16> = root
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let mut avail: u64 = 0;
    let ok = unsafe {
        GetDiskFreeSpaceExW(
            wide.as_ptr(),
            &mut avail,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if ok != 0 {
        avail
    } else {
        0
    }
}

#[cfg(not(windows))]
fn free_disk_space(_path: &Path) -> u64 {
    0
}

/// Checks if `node` (`node.exe` on Windows) is available on `PATH`.
fn has_node_on_path() -> bool {
    let Some(path_var) = std::env::var_os("PATH") else {
        return false;
    };
    for dir in std::env::split_paths(&path_var) {
        if dir.join("node.exe").is_file() || dir.join("node").is_file() {
            return true;
        }
    }
    false
}

/// Opens a file, program, or URL through `explorer.exe` so it runs as the normal user, not elevated.
fn shell_open_via_explorer(target: &str) {
    let root = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
    let explorer = PathBuf::from(root).join("explorer.exe");
    let _ = Command::new(explorer).arg(target).spawn();
}

fn push_setup(window: &WebviewWindow, event: &str, data: Value) {
    let ev = serde_json::to_string(event).unwrap_or_else(|_| "\"\"".into());
    let mut payload = serde_json::to_string(&data).unwrap_or_else(|_| "null".into());
    if payload.contains('\u{2028}') || payload.contains('\u{2029}') {
        payload = payload
            .replace('\u{2028}', "\\u2028")
            .replace('\u{2029}', "\\u2029");
    }
    let _ = window.eval(&format!("window.onSetup && window.onSetup({ev}, {payload})"));
}

fn build_info(state: &SetupState) -> Value {
    let sys = WindowsSystem;
    let existing = installer::installed_version(&sys);
    let program_files = installer::program_files_dir();
    let install_dir = existing
        .as_ref()
        .and_then(|(_, d)| if d.is_empty() { None } else { Some(PathBuf::from(d)) })
        .unwrap_or_else(|| program_files.join(APP));

    let cfg: Value = std::env::var_os("APPDATA")
        .map(|a| PathBuf::from(a).join(APP).join("config.json"))
        .and_then(|p| fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| json!({}));

    let documents = installer::documents_dir();
    let needed = state
        .payload
        .as_ref()
        .map(PayloadSource::uncompressed_size)
        .unwrap_or(0);
    let free = free_disk_space(&install_dir);
    let networks = sys.networks().unwrap_or_default();

    let language = cfg
        .get("language")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .unwrap_or_else(|| windows_language().to_string());

    let save_dir = cfg
        .get("save_dir")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .unwrap_or_else(|| documents.join(APP).to_string_lossy().into_owned());

    let mut features = Map::new();
    for &f in FEATURES {
        let default_on = f != "remote";
        let on = cfg
            .get("features")
            .and_then(|m| m.get(f))
            .and_then(Value::as_bool)
            .unwrap_or(default_on);
        features.insert(f.to_string(), Value::Bool(on));
    }

    let mut theme = Map::new();
    theme.insert("accent".into(), json!("cyan"));
    theme.insert("custom".into(), json!("#14b8e0"));
    theme.insert("mode".into(), json!("dark"));
    if let Some(t) = cfg.get("theme").and_then(Value::as_object) {
        for (k, v) in t {
            theme.insert(k.clone(), v.clone());
        }
    }

    let existing_json = existing
        .as_ref()
        .map(|(v, d)| json!({ "version": v, "dir": d }))
        .unwrap_or(Value::Null);

    let autostart = sys.read_value(RegHive::Hkcu, RUN_KEY, APP).is_some();
    let desktop = installer::desktop_dir().join(format!("{APP}.lnk")).exists() || existing.is_none();
    let start_menu =
        installer::start_menu_dir().join(format!("{APP}.lnk")).exists() || existing.is_none();
    let update = state.update_flag && existing.is_some();

    json!({
        "version": state.version,
        "existing": existing_json,
        "install_dir": install_dir.to_string_lossy(),
        "needed": needed,
        "free": free,
        "language": language,
        "save_dir": save_dir,
        "sort_files": cfg.get("sort_files").and_then(Value::as_bool).unwrap_or(true),
        "download_dir": cfg.get("download_dir").and_then(Value::as_str).unwrap_or(""),
        "calc_angle": cfg.get("calc_angle").and_then(Value::as_str).unwrap_or("deg"),
        "notify": cfg.get("notify").and_then(Value::as_bool).unwrap_or(true),
        "features": Value::Object(features),
        "theme": Value::Object(theme),
        "networks": networks,
        "node": has_node_on_path(),
        "autostart": autostart,
        "desktop": desktop,
        "start_menu": start_menu,
        "update": update,
    })
}

fn arg(args: &[Value], i: usize) -> &Value {
    args.get(i).unwrap_or(&Value::Null)
}

fn dispatch(app: &AppHandle, window: &WebviewWindow, method: &str, args: &[Value]) -> Value {
    let state = app.state::<SetupState>();
    match method {
        "info" => build_info(&state),

        "pick_folder" => {
            let start = arg(args, 0).as_str().unwrap_or("");
            let mut dialog = app.dialog().file();
            if !start.is_empty() && Path::new(start).exists() {
                dialog = dialog.set_directory(start);
            }
            match dialog.blocking_pick_folder().and_then(|p| p.into_path().ok()) {
                Some(p) => Value::String(p.to_string_lossy().into_owned()),
                None => Value::Null,
            }
        }

        "open_url" => {
            if let Some(url) = arg(args, 0).as_str() {
                if url == "https://nodejs.org/en/download" {
                    shell_open_via_explorer(url);
                }
            }
            Value::Null
        }

        "open_log" => {
            let log = installer::log_file();
            if log.exists() {
                shell_open_via_explorer(&log.to_string_lossy());
            }
            Value::Null
        }

        "install" => {
            if state.installing.swap(true, Ordering::SeqCst) {
                return Value::Null;
            }
            let mut choices = arg(args, 0).clone();
            if !choices.is_object() {
                choices = json!({});
            }
            if let Some(obj) = choices.as_object_mut() {
                obj.insert("version".into(), json!(state.version));
                let mut logged = obj.clone();
                logged.remove("theme");
                let summary = serde_json::to_string(&Value::Object(logged)).unwrap_or_default();
                installer::write_log(&format!(
                    "Swiftlane Setup {} starting. Choices: {summary}",
                    state.version
                ));
            }

            let payload = state
                .payload
                .clone()
                .or_else(|| PayloadSource::resolve().ok());
            let app_bg = app.clone();
            let win_bg = window.clone();

            std::thread::spawn(move || {
                let st = app_bg.state::<SetupState>();
                let Some(payload) = payload else {
                    let msg = "The installer package is missing its payload.";
                    installer::write_log(&format!("Error: {msg}"));
                    st.installing.store(false, Ordering::SeqCst);
                    push_setup(&win_bg, "failed", json!({ "error": msg }));
                    return;
                };

                let fail_at = std::env::var("SWIFTLANE_SETUP_FAIL_AT")
                    .ok()
                    .filter(|s| !s.is_empty());

                let win_prog = win_bg.clone();
                let res = installer::install(
                    &payload,
                    &choices,
                    &WindowsSystem,
                    |fraction, text| {
                        push_setup(
                            &win_prog,
                            "progress",
                            json!({ "fraction": fraction, "text": text }),
                        );
                    },
                    fail_at.as_deref(),
                    RegHive::Hklm,
                );

                match res {
                    Ok(installed) => {
                        let out = json!({ "exe": installed.exe, "stamp": installed.stamp });
                        *st.result.lock().unwrap() = Some(installed);
                        push_setup(&win_bg, "installed", out);
                    }
                    Err(e) => {
                        installer::write_log(&format!("Error: {e}"));
                        st.installing.store(false, Ordering::SeqCst);
                        push_setup(&win_bg, "failed", json!({ "error": e }));
                    }
                }
            });
            Value::Null
        }

        "finish" => {
            let launch = arg(args, 0).as_bool().unwrap_or(false);
            let pair = arg(args, 1).as_bool().unwrap_or(false);
            if launch {
                let res = state.result.lock().unwrap().clone();
                if let Some(installed) = res {
                    let exe = PathBuf::from(&installed.exe);
                    if pair {
                        if let Some(dir) = exe.parent() {
                            let choices_file = dir.join("setup-choices.json");
                            if let Ok(text) = fs::read_to_string(&choices_file) {
                                if let Ok(mut data) = serde_json::from_str::<Value>(&text) {
                                    if let Some(obj) = data.as_object_mut() {
                                        obj.insert("open_page".into(), json!("connect"));
                                        if let Ok(pretty) = serde_json::to_string_pretty(&data) {
                                            let _ = fs::write(&choices_file, pretty);
                                        }
                                    }
                                }
                            }
                        }
                    }
                    shell_open_via_explorer(&exe.to_string_lossy());
                }
            }
            let _ = window.destroy();
            Value::Null
        }

        "uninstall_existing" => {
            if let Some((_, dir)) = installer::installed_version(&WindowsSystem) {
                if !dir.is_empty() {
                    let exe = PathBuf::from(dir).join(format!("{APP}.exe"));
                    if exe.is_file() && Command::new(&exe).arg("--uninstall").spawn().is_ok() {
                        let _ = window.destroy();
                        return json!(true);
                    }
                }
            }
            json!(false)
        }

        "close" => {
            let _ = window.destroy();
            Value::Null
        }

        _ => Value::Null,
    }
}

#[tauri::command]
async fn setup_call(
    app: AppHandle,
    window: WebviewWindow,
    method: String,
    args: Vec<Value>,
) -> Value {
    tauri::async_runtime::spawn_blocking(move || dispatch(&app, &window, &method, &args))
        .await
        .unwrap_or(Value::Null)
}

/// Started by Swiftlane's Update button, Setup can inherit Swiftlane's install folder as its CWD
/// (causing Windows to refuse moving that folder as "in use") or a hidden startup flag. Spawning a
/// fresh copy with `%TEMP%` as its CWD drops both. Returns `true` if the fresh copy is running and
/// this process should exit cleanly.
fn restart_for_update() -> bool {
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    let is_update = args.iter().any(|a| a == "--update");
    if !is_update || std::env::var_os(RESTARTED_ENV).is_some() {
        return false;
    }
    let Ok(exe) = std::env::current_exe() else {
        return false;
    };
    let cwd = std::env::temp_dir();
    match Command::new(&exe)
        .args(&args)
        .current_dir(&cwd)
        .env(RESTARTED_ENV, "1")
        .spawn()
    {
        Ok(_) => true,
        Err(e) => {
            installer::write_log(&format!("Couldn't restart Setup ({e}); carrying on"));
            false
        }
    }
}

fn main() {
    if restart_for_update() {
        return;
    }
    let _ = std::env::set_current_dir(std::env::temp_dir());
    let _ = fs::remove_file(installer::log_file());

    let update_flag = std::env::args().any(|a| a == "--update");
    let state = SetupState::new(update_flag);

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(state)
        .invoke_handler(tauri::generate_handler![setup_call])
        .setup(move |app| {
            if update_flag {
                if let Some(w) = app.get_webview_window("main") {
                    let _ = w.set_title("Updating Swiftlane");
                }
            }
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("Swiftlane Setup couldn't start");
}
