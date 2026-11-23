//! Swiftlane's settings: %APPDATA%\Swiftlane\config.json, the same file the Python version uses,
//! so the Rust version starts with your theme, language and choices.
//! Step 1 only reads it; nothing is written yet.

use serde_json::{json, Map, Value};
use std::path::PathBuf;

pub fn config_dir() -> PathBuf {
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("Swiftlane")
}

/// The same defaults as the Python version (server.py DEFAULTS).
fn defaults() -> Value {
    json!({
        "port": 8765,
        "notify": true,
        "features": {
            "transfer": true, "converter": true, "images": true, "downloader": true, "currency": true,
            "calculator": true, "speedtest": true, "adblock": true, "share": true, "pdf": true,
            "trimmer": true, "vault": true, "diskmap": true, "netscan": true, "exposure": true,
            "contacts": true, "worldclock": true, "remote": false
        },
        "remote_power": false,
        "https_only": false,
        "sort_files": true,
        "download_dir": "",
        "calc_angle": "deg",
        "language": "en",
        "theme": {
            "accent": "cyan", "custom": "#14b8e0", "background": false, "blur": 14, "dim": 30,
            "blur_on": true, "mode": "dark", "bg_file": ""
        },
        "update_check": true
    })
}

/// Fills in whatever the file doesn't have (one level deep for nested settings like "features").
fn merge(base: &mut Value, saved: &Value) {
    if let (Value::Object(b), Value::Object(s)) = (base, saved) {
        for (k, v) in s {
            match (b.get_mut(k), v) {
                (Some(Value::Object(bo)), Value::Object(so)) => {
                    for (k2, v2) in so {
                        bo.insert(k2.clone(), v2.clone());
                    }
                }
                _ => {
                    b.insert(k.clone(), v.clone());
                }
            }
        }
    }
}

pub fn load() -> Value {
    let mut cfg = defaults();
    if let Ok(text) = std::fs::read_to_string(config_dir().join("config.json")) {
        if let Ok(saved) = serde_json::from_str::<Value>(&text) {
            merge(&mut cfg, &saved);
        }
    }
    if !cfg.is_object() {
        cfg = Value::Object(Map::new());
    }
    cfg
}
