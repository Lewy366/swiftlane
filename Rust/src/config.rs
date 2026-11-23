//! Swiftlane's settings: %APPDATA%\Swiftlane\config.json, the same file the Python version uses,
//! so the Rust version keeps your theme, language and choices. Plus the log file next to it.

use serde_json::{json, Value};
use std::io::Write;
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
            "trimmer": true, "vault": true, "diskmap": true, "duplicates": true, "findfiles": true,
            "startup": true, "qrstudio": true, "netscan": true, "exposure": true,
            "contacts": true, "worldclock": true, "remote": false, "airplay": true
        },
        "airplay_on": false,
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
        "max_file_gb": 20,
        "share": { "hours": 24, "mode": "open", "count": null, "people": 1, "keep_awake": true },
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
    let mut changed = false;
    if let Some(exe_dir) = std::env::current_exe().ok().and_then(|e| e.parent().map(PathBuf::from)) {
        if apply_installer_choices(&mut cfg, &exe_dir.join("setup-choices.json")) {
            changed = true;
        }
    }
    if cfg["save_dir"].as_str().map_or(true, str::is_empty) {
        let docs = dirs::document_dir().unwrap_or_else(|| config_dir().join(".."));
        cfg["save_dir"] = json!(docs.join("Swiftlane").to_string_lossy());
        changed = true;
    }
    if cfg["code"].as_str().map_or(true, str::is_empty) {
        cfg["code"] = json!(new_code());
        changed = true;
    }
    if changed {
        save(&cfg);
    }
    cfg
}

/// A new pairing code: 16 characters without look-alikes (0/O, 1/I/L), about 79 bits.
pub fn new_code() -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHJKMNPQRSTUVWXYZ23456789";
    let mut out = String::new();
    while out.len() < 16 {
        let mut b = [0u8; 32];
        getrandom::getrandom(&mut b).expect("Windows' random number source failed");
        for x in b {
            // 31 symbols; reject the top of the byte range so every symbol is equally likely
            if (x as usize) < 248 && out.len() < 16 {
                out.push(ALPHABET[x as usize % 31] as char);
            }
        }
    }
    out
}

/// "ABCD-EFGH-JKMN-PQRS"
pub fn pretty_code(code: &str) -> String {
    code.as_bytes().chunks(4).map(|c| String::from_utf8_lossy(c).into_owned()).collect::<Vec<_>>().join("-")
}

/// Codes typed or pasted with spaces/dashes or in lower case still match.
pub fn normalize_code(code: &str) -> String {
    code.chars().filter(|c| !c.is_whitespace() && *c != '-').collect::<String>().to_uppercase()
}

/// Saves the settings: written to a temporary file first and then swapped in, so a crash
/// or power cut can never leave a half-written settings file behind.
pub fn save(cfg: &Value) {
    let dir = config_dir();
    let _ = std::fs::create_dir_all(&dir);
    let mut out = cfg.clone();
    if let Value::Object(m) = &mut out {
        m.retain(|k, _| !k.starts_with('_'));
    }
    let Ok(text) = serde_json::to_string_pretty(&out) else { return };
    let tmp = dir.join("config.json.tmp");
    if std::fs::write(&tmp, text).is_ok() {
        if std::fs::rename(&tmp, dir.join("config.json")).is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
    }
}

/// One line in swiftlane.log (kept under about 1 MB; the older half moves to swiftlane.old.log).
pub fn log(msg: &str) {
    let dir = config_dir();
    let path = dir.join("swiftlane.log");
    if std::fs::metadata(&path).map(|m| m.len() > 1024 * 1024).unwrap_or(false) {
        let _ = std::fs::rename(&path, dir.join("swiftlane.old.log"));
    }
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(f, "[{}] {}", format_utc(secs), msg);
    }
}

/// "2026-11-23 14.05.09" in local time, for names of files that arrive without one.
pub fn stamp_now() -> String {
    chrono::Local::now().format("%Y-%m-%d %H.%M.%S").to_string()
}

/// "2026-11-23 14:05:09 UTC" without pulling in a date library.
fn format_utc(secs: u64) -> String {
    let days = (secs / 86400) as i64;
    let (h, m, s) = ((secs % 86400) / 3600, (secs % 3600) / 60, secs % 60);
    // civil-from-days (Howard Hinnant's algorithm)
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if mo <= 2 { 1 } else { 0 };
    format!("{y:04}-{mo:02}-{d:02} {h:02}:{m:02}:{s:02} UTC")
}

/// Applies choices saved by Swiftlane Setup (`setup-choices.json` next to `Swiftlane.exe`) once
/// per install stamp. Never touches the pairing code.
pub fn apply_installer_choices(cfg: &mut Value, path: &std::path::Path) -> bool {
    let Ok(text) = std::fs::read_to_string(path) else {
        return false;
    };
    let Ok(choices) = serde_json::from_str::<Value>(&text) else {
        return false;
    };
    let Some(stamp) = choices.get("stamp").filter(|v| !v.is_null()) else {
        return false;
    };
    if cfg.get("setup_applied") == Some(stamp) {
        return false;
    }
    for key in ["save_dir", "sort_files", "download_dir", "calc_angle", "language", "notify"] {
        if let Some(v) = choices.get(key) {
            cfg[key] = v.clone();
        }
    }
    if let (Some(dst), Some(src)) = (
        cfg.get_mut("features").and_then(Value::as_object_mut),
        choices.get("features").and_then(Value::as_object),
    ) {
        for (k, v) in src {
            if dst.contains_key(k) {
                dst.insert(k.clone(), json!(v.as_bool().unwrap_or(false)));
            }
        }
    }
    if let (Some(dst), Some(src)) = (
        cfg.get_mut("theme").and_then(Value::as_object_mut),
        choices.get("theme").and_then(Value::as_object),
    ) {
        for (k, v) in src {
            if k != "background" && dst.contains_key(k) {
                dst.insert(k.clone(), v.clone());
            }
        }
    }
    if let Some(page) = choices.get("open_page").and_then(Value::as_str).filter(|s| !s.is_empty()) {
        cfg["_open_page"] = json!(page);
    }
    cfg["setup_applied"] = stamp.clone();
    log(&format!("Applied choices from Swiftlane Setup ({stamp})"));
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utc_dates() {
        assert_eq!(super::format_utc(0), "1970-01-01 00:00:00 UTC");
        assert_eq!(super::format_utc(1_795_000_000), "2026-11-18 11:06:40 UTC");
    }

    #[test]
    fn applies_setup_choices_once_without_touching_code() {
        let mut b = [0u8; 6];
        getrandom::getrandom(&mut b).unwrap();
        let dir = std::env::temp_dir().join(format!("swl-cfg-test-{}", hex::encode(b)));
        std::fs::create_dir(&dir).unwrap();
        let choices_path = dir.join("setup-choices.json");
        std::fs::write(
            &choices_path,
            json!({
                "stamp": "2026-11-23T19:00:00Z",
                "language": "de",
                "code": "SHOULD-NOT-OVERWRITE",
                "features": { "remote": true, "bogus": true },
                "theme": { "accent": "violet", "background": true },
                "open_page": "connect"
            })
            .to_string(),
        )
        .unwrap();

        let mut cfg = defaults();
        cfg["code"] = json!("KEEPME");
        assert!(apply_installer_choices(&mut cfg, &choices_path));
        assert_eq!(cfg["language"], "de");
        assert_eq!(cfg["code"], "KEEPME");
        assert_eq!(cfg["features"]["remote"], true);
        assert!(cfg["features"].get("bogus").is_none());
        assert_eq!(cfg["theme"]["accent"], "violet");
        assert_eq!(cfg["theme"]["background"], false);
        assert_eq!(cfg["_open_page"], "connect");

        // Second call with the same stamp is a no-op
        assert!(!apply_installer_choices(&mut cfg, &choices_path));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
