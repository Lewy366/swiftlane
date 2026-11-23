//! The bridge the interface talks to. The page calls `App.api.<name>(...)` exactly as it did with
//! Python; js/bridge.js turns that into one `call` command with the name and arguments.
//! Every tool is ported one by one; until then a call answers with a plain-language error.

use base64::Engine;
use serde_json::{json, Value};
use std::sync::Mutex;

use crate::config;

pub struct AppState {
    pub cfg: Mutex<Value>,
}

impl AppState {
    pub fn load() -> Self {
        AppState { cfg: Mutex::new(config::load()) }
    }
}

const TOOL_LISTS: &str = include_str!("tool_lists.json");

#[tauri::command]
pub fn call(state: tauri::State<'_, AppState>, method: String, args: Vec<Value>) -> Value {
    let _ = args;
    match method.as_str() {
        "get_state" => get_state(&state),
        "get_background" => get_background(&state),
        _ => json!({ "error": format!("“{method}” isn't in the Rust version of Swiftlane yet.") }),
    }
}

fn get_state(state: &AppState) -> Value {
    let cfg = state.cfg.lock().unwrap().clone();
    let mut s = json!({
        "version": env!("CARGO_PKG_VERSION"),
        "rust": true,
        "features": cfg["features"],
        "server_running": false,
        "server_error": "Phone transfer isn't in the Rust version yet.",
        "code_pretty": "",
        "qr": null,
        "tls": { "ready": false, "https_only": false, "fingerprint": "", "cert_qr": null, "secure_qr": null },
        "addresses": [],
        "save_dir": cfg.get("save_dir").cloned().unwrap_or(json!("")),
        "notify": cfg["notify"],
        "autostart": false,
        "config_dir": config::config_dir().to_string_lossy(),
        "outbox": [],
        "received": [],
        "clocks": cfg.get("clocks").cloned().unwrap_or(Value::Null),
        "sort_files": cfg["sort_files"],
        "download_dir": cfg["download_dir"],
        "calc_angle": cfg["calc_angle"],
        "language": cfg["language"],
        "theme": cfg["theme"],
        "node": false,
        "start_page": null,
        "update_check": cfg["update_check"],
        "sidebar": cfg.get("sidebar").cloned().unwrap_or(Value::Null),
        "remote_power": cfg["remote_power"],
        "context_menu": false,
        "explorer": null,
        "update": null,
        "updated": null
    });
    // Lists the tools show (formats, units, constants...), taken over from the Python version as they are.
    if let (Value::Object(out), Ok(Value::Object(lists))) = (&mut s, serde_json::from_str::<Value>(TOOL_LISTS)) {
        out.extend(lists);
    }
    s
}

/// Your own background picture as a data: URL, or null for Swiftlane's wallpaper.
fn get_background(state: &AppState) -> Value {
    let cfg = state.cfg.lock().unwrap();
    let name = cfg["theme"]["bg_file"].as_str().filter(|n| !n.is_empty()).unwrap_or("background.jpg").to_string();
    drop(cfg);
    // Only a plain file name inside Swiftlane's own folder, never a path somewhere else.
    if name.contains(['/', '\\', ':']) || name.starts_with('.') {
        return Value::Null;
    }
    let path = config::config_dir().join(&name);
    let Ok(bytes) = std::fs::read(&path) else { return Value::Null };
    if bytes.len() > 60 * 1024 * 1024 {
        return Value::Null;
    }
    let ext = name.rsplit('.').next().unwrap_or("").to_lowercase();
    let mime = match ext.as_str() {
        "png" => "image/png",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        _ => "image/jpeg",
    };
    json!(format!("data:{mime};base64,{}", base64::engine::general_purpose::STANDARD.encode(bytes)))
}
