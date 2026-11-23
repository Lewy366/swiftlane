//! File Explorer right-click menu + authenticated single-instance IPC handoff.
//! Port of Source/context_menu.py, Source/ipc.py, and Explorer batching in Source/swiftlane.py.
//!
//! Security rules (matching Python):
//! - Right-click entries live under HKCU\Software\Classes (this Windows account only, no admin).
//! - When Explorer launches a second Swiftlane.exe with --share / --send / --convert, it hands
//!   the paths to the running instance over loopback IPC using a random 32-byte per-account key
//!   (%APPDATA%\Swiftlane\ipc.key) verified via HMAC-SHA256 challenge-response (constant time).
//! - Only ("share", "send", "convert", "shrink") with existing paths (max 500, each < 1024 chars) are accepted.

use hmac::{Hmac, Mac};
use serde_json::{json, Value};
use sha2::Sha256;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use tauri::{AppHandle, Manager};

use super::arg;
use crate::api::{file_info, push};
use crate::config;

type HmacSha256 = Hmac<Sha256>;

const COMMANDS: &[&str] = &["share", "send", "convert", "shrink"];
const MAX_PATHS: usize = 500;
const MAX_BODY: usize = 256 * 1024;
const BATCH_DELAY: Duration = Duration::from_millis(800);

// ------------------------------------------------------------------ CLI flag parsing

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CliArgs {
    pub minimized: bool,
    pub uninstall: bool,
    pub start_page: Option<String>,
    pub explorer: Option<(String, Vec<String>)>,
}

pub fn parse_args<I: IntoIterator<Item = String>>(args: I) -> CliArgs {
    let mut out = CliArgs::default();
    let mut it = args.into_iter();
    let mut active_cmd: Option<String> = None;
    let mut paths: Vec<String> = Vec::new();

    while let Some(a) = it.next() {
        match a.as_str() {
            "--minimized" => {
                out.minimized = true;
                active_cmd = None;
            }
            "--uninstall" => {
                out.uninstall = true;
                active_cmd = None;
            }
            "--page" => {
                if let Some(p) = it.next() {
                    if !p.is_empty() && p.len() <= 32 && p.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
                        out.start_page = Some(p);
                    }
                }
                active_cmd = None;
            }
            "--share" | "--send" | "--convert" | "--shrink" => {
                if active_cmd.is_none() {
                    active_cmd = Some(a.trim_start_matches("--").to_string());
                }
            }
            _ if a.starts_with("--") => {
                active_cmd = None;
            }
            _ => {
                if active_cmd.is_some() {
                    paths.push(a);
                }
            }
        }
    }
    if let Some(cmd) = active_cmd {
        if !paths.is_empty() {
            out.explorer = Some((cmd, paths));
        }
    }
    out
}

// ------------------------------------------------------------------ Windows Explorer context menu

#[cfg(windows)]
mod win_menu {
    use winreg::enums::{HKEY_CURRENT_USER, KEY_READ};
    use winreg::RegKey;

    const ROOT: &str = r"Software\Classes\";
    const ENTRIES: &[(&str, &str, &str)] = &[
        (r"*\shell\SwiftlaneShare", "Share with Swiftlane", "--share"),
        (r"Directory\shell\SwiftlaneShare", "Share with Swiftlane", "--share"),
        (r"*\shell\SwiftlaneSend", "Send to phone with Swiftlane", "--send"),
        (r"*\shell\SwiftlaneConvert", "Convert with Swiftlane", "--convert"),
        // only on videos and pictures (Windows' own "perceived type" groups)
        (r"SystemFileAssociations\video\shell\SwiftlaneShrink", "Shrink to size with Swiftlane", "--shrink"),
        (r"SystemFileAssociations\image\shell\SwiftlaneShrink", "Shrink to size with Swiftlane", "--shrink"),
    ];

    pub fn enable() -> Result<(), String> {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let exe_str = exe.to_string_lossy().to_string();
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        for (sub, label, flag) in ENTRIES {
            let key_path = format!("{ROOT}{sub}");
            let (k, _) = hkcu.create_subkey(&key_path).map_err(|e| e.to_string())?;
            k.set_value("MUIVerb", label).map_err(|e| e.to_string())?;
            k.set_value("Icon", &exe_str).map_err(|e| e.to_string())?;
            k.set_value("MultiSelectModel", &"Player").map_err(|e| e.to_string())?;
            let (cmd_k, _) = hkcu
                .create_subkey(format!(r"{key_path}\command"))
                .map_err(|e| e.to_string())?;
            cmd_k
                .set_value("", &format!("\"{exe_str}\" {flag} \"%1\""))
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    pub fn disable() -> Result<(), String> {
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        for (sub, _, _) in ENTRIES {
            let _ = hkcu.delete_subkey(format!(r"{ROOT}{sub}\command"));
            let _ = hkcu.delete_subkey(format!("{ROOT}{sub}"));
        }
        Ok(())
    }

    pub fn is_enabled() -> bool {
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        hkcu.open_subkey_with_flags(format!(r"{ROOT}{}\command", ENTRIES[0].0), KEY_READ)
            .is_ok()
    }
}

#[cfg(not(windows))]
mod win_menu {
    pub fn enable() -> Result<(), String> {
        Err("Only supported on Windows".into())
    }
    pub fn disable() -> Result<(), String> {
        Ok(())
    }
    pub fn is_enabled() -> bool {
        false
    }
}

pub fn is_enabled() -> bool {
    win_menu::is_enabled()
}

pub fn disable_menu() {
    let _ = win_menu::disable();
}

// ------------------------------------------------------------------ IPC message cleaning & HMAC auth

pub fn clean_message(msg: &Value) -> Option<(String, Vec<String>)> {
    let cmd = msg.get("cmd")?.as_str()?;
    if !COMMANDS.contains(&cmd) {
        return None;
    }
    let arr = msg.get("paths")?.as_array()?;
    let mut out = Vec::new();
    for v in arr.iter().take(MAX_PATHS) {
        let Some(p) = v.as_str() else { continue };
        if p.is_empty() || p.len() >= 1024 {
            continue;
        }
        let path = Path::new(p);
        if !path.exists() {
            continue;
        }
        let abs = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
        out.push(abs.to_string_lossy().to_string());
    }
    if out.is_empty() {
        None
    } else {
        Some((cmd.to_string(), out))
    }
}

fn load_or_create_key(dir: &Path) -> Result<[u8; 32], String> {
    let path = dir.join("ipc.key");
    if let Ok(bytes) = std::fs::read(&path) {
        if bytes.len() == 32 {
            let mut key = [0u8; 32];
            key.copy_from_slice(&bytes);
            return Ok(key);
        }
    }
    let _ = std::fs::create_dir_all(dir);
    let mut key = [0u8; 32];
    getrandom::getrandom(&mut key).map_err(|e| e.to_string())?;
    let tmp = dir.join("ipc.key.tmp");
    std::fs::write(&tmp, key).map_err(|e| e.to_string())?;
    if std::fs::rename(&tmp, &path).is_err() {
        let _ = std::fs::remove_file(&tmp);
        std::fs::write(&path, key).map_err(|e| e.to_string())?;
    }
    Ok(key)
}

fn sign_frame(key: &[u8; 32], nonce: &[u8; 32], payload: &[u8]) -> [u8; 32] {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts any key size");
    mac.update(nonce);
    mac.update(payload);
    let bytes = mac.finalize().into_bytes();
    let mut out = [0u8; 32];
    out.copy_from_slice(&bytes);
    out
}

fn read_exact_or_err(stream: &mut TcpStream, buf: &mut [u8]) -> Result<(), ()> {
    stream.read_exact(buf).map_err(|_| ())
}

fn handle_ipc_conn(mut stream: TcpStream, key: &[u8; 32], on_valid: &(dyn Fn(String, Vec<String>) + Send + Sync)) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));

    let mut nonce = [0u8; 32];
    if getrandom::getrandom(&mut nonce).is_err() {
        return;
    }
    if stream.write_all(&nonce).is_err() {
        return;
    }

    let mut mac_in = [0u8; 32];
    let mut len_buf = [0u8; 4];
    if read_exact_or_err(&mut stream, &mut mac_in).is_err()
        || read_exact_or_err(&mut stream, &mut len_buf).is_err()
    {
        return;
    }
    let len = u32::from_be_bytes(len_buf) as usize;
    if len == 0 || len > MAX_BODY {
        let _ = stream.write_all(b"refused");
        return;
    }
    let mut payload = vec![0u8; len];
    if read_exact_or_err(&mut stream, &mut payload).is_err() {
        return;
    }

    let expected = sign_frame(key, &nonce, &payload);
    if !constant_time_eq::constant_time_eq(&mac_in, &expected) {
        let _ = stream.write_all(b"refused");
        return;
    }

    let parsed: Option<Value> = serde_json::from_slice(&payload).ok();
    match parsed.as_ref().and_then(clean_message) {
        Some((cmd, paths)) => {
            let _ = stream.write_all(b"ok");
            on_valid(cmd, paths);
        }
        None => {
            let _ = stream.write_all(b"refused");
        }
    }
}

/// Starts the loopback IPC server in `dir` and writes `ipc.port`. Returns the bound port.
pub fn serve_in_dir<F>(dir: PathBuf, handler: F) -> Result<u16, String>
where
    F: Fn(String, Vec<String>) + Send + Sync + 'static,
{
    let key = load_or_create_key(&dir)?;
    let listener = TcpListener::bind("127.0.0.1:0").map_err(|e| e.to_string())?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let _ = std::fs::create_dir_all(&dir);
    let port_file = dir.join("ipc.port");
    let tmp = dir.join("ipc.port.tmp");
    if std::fs::write(&tmp, port.to_string()).is_ok() {
        if std::fs::rename(&tmp, &port_file).is_err() {
            let _ = std::fs::remove_file(&tmp);
            let _ = std::fs::write(&port_file, port.to_string());
        }
    }
    let handler = Arc::new(handler);
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(stream) = conn else {
                std::thread::sleep(Duration::from_millis(200));
                continue;
            };
            let h = Arc::clone(&handler);
            std::thread::spawn(move || handle_ipc_conn(stream, &key, h.as_ref()));
        }
    });
    Ok(port)
}

/// Hands off `cmd` and `paths` to an already-running Swiftlane in `dir`. Returns true if accepted.
pub fn send_to_dir(dir: &Path, cmd: &str, paths: &[String]) -> bool {
    let Ok(port_text) = std::fs::read_to_string(dir.join("ipc.port")) else {
        return false;
    };
    let Ok(port) = port_text.trim().parse::<u16>() else {
        return false;
    };
    let Ok(key_bytes) = std::fs::read(dir.join("ipc.key")) else {
        return false;
    };
    if key_bytes.len() != 32 {
        return false;
    }
    let mut key = [0u8; 32];
    key.copy_from_slice(&key_bytes);

    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let Ok(mut stream) = TcpStream::connect_timeout(&addr, Duration::from_secs(2)) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(3)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(3)));

    let mut nonce = [0u8; 32];
    if stream.read_exact(&mut nonce).is_err() {
        return false;
    }
    let payload = json!({ "cmd": cmd, "paths": paths }).to_string();
    let payload_bytes = payload.as_bytes();
    if payload_bytes.len() > MAX_BODY {
        return false;
    }
    let mac = sign_frame(&key, &nonce, payload_bytes);
    let len = (payload_bytes.len() as u32).to_be_bytes();
    if stream.write_all(&mac).is_err()
        || stream.write_all(&len).is_err()
        || stream.write_all(payload_bytes).is_err()
    {
        return false;
    }
    let mut reply = [0u8; 16];
    match stream.read(&mut reply) {
        Ok(n) => &reply[..n] == b"ok",
        Err(_) => false,
    }
}

/// Called early in main() when launched with --share / --send / --convert.
/// If another Swiftlane is already running and accepts the files, returns true so we can exit.
pub fn try_handoff(cmd: &str, paths: &[String]) -> bool {
    send_to_dir(&config::config_dir(), cmd, paths)
}

// ------------------------------------------------------------------ batching & Tauri integration

struct BatchState {
    active: Option<(String, Vec<String>)>,
    pending: Option<Value>,
    ui_ready: bool,
}

static BATCH: Mutex<BatchState> = Mutex::new(BatchState {
    active: None,
    pending: None,
    ui_ready: false,
});

static START_PAGE: OnceLock<Mutex<Option<String>>> = OnceLock::new();

pub fn set_start_page(page: Option<String>) {
    let cell = START_PAGE.get_or_init(|| Mutex::new(None));
    *cell.lock().unwrap() = page;
}

pub fn take_start_page() -> Value {
    START_PAGE
        .get()
        .and_then(|m| m.lock().unwrap().take())
        .map(Value::String)
        .unwrap_or(Value::Null)
}

pub fn take_pending() -> Value {
    let mut b = BATCH.lock().unwrap();
    b.ui_ready = true;
    b.pending.take().unwrap_or(Value::Null)
}

pub fn enqueue(app: &AppHandle, cmd: String, paths: Vec<String>) {
    let spawn_flusher = {
        let mut b = BATCH.lock().unwrap();
        let is_new = b.active.is_none();
        if is_new {
            b.active = Some((cmd.clone(), Vec::new()));
        }
        if let Some((ref active_cmd, ref mut list)) = b.active {
            if active_cmd == &cmd {
                for p in paths {
                    if !list.contains(&p) {
                        list.push(p);
                    }
                }
            }
        }
        is_new
    };

    if spawn_flusher {
        let app = app.clone();
        std::thread::spawn(move || {
            std::thread::sleep(BATCH_DELAY);
            let Some((cmd, paths)) = BATCH.lock().unwrap().active.take() else {
                return;
            };
            let files: Vec<Value> = paths
                .iter()
                .map(Path::new)
                .filter(|p| p.is_file())
                .map(file_info)
                .collect();
            let folders: Vec<String> = paths
                .iter()
                .filter(|p| Path::new(p).is_dir())
                .cloned()
                .collect();
            let req = json!({ "cmd": cmd, "files": files, "folders": folders });

            if let Some(w) = app.get_webview_window("main") {
                let _ = w.unminimize();
                let _ = w.show();
                let _ = w.set_focus();
            }

            let push_now = {
                let mut b = BATCH.lock().unwrap();
                if b.ui_ready {
                    true
                } else {
                    b.pending = Some(req.clone());
                    false
                }
            };
            if push_now {
                push(&app, "explorer", req);
            }
        });
    }
}

pub fn start(app: &AppHandle, initial: Option<(String, Vec<String>)>) {
    let app_ipc = app.clone();
    let _ = serve_in_dir(config::config_dir(), move |cmd, paths| {
        enqueue(&app_ipc, cmd, paths);
    });
    if let Some((cmd, paths)) = initial {
        if let Some((c, cleaned)) = clean_message(&json!({ "cmd": cmd, "paths": paths })) {
            enqueue(app, c, cleaned);
        }
    }
}

pub fn shutdown() {
    let _ = std::fs::remove_file(config::config_dir().join("ipc.port"));
}

pub fn dispatch(_app: &AppHandle, method: &str, args: &[Value]) -> Option<Value> {
    if method != "set_context_menu" {
        return None;
    }
    let enabled = arg(args, 0).as_bool().unwrap_or(false);
    let res = if enabled {
        win_menu::enable()
    } else {
        win_menu::disable()
    };
    Some(match res {
        Ok(()) => {
            config::log(&format!(
                "Explorer right-click menu {}",
                if enabled { "added" } else { "removed" }
            ));
            json!({ "ok": true, "enabled": is_enabled() })
        }
        Err(e) => json!({ "error": format!("Couldn't change the right-click menu ({e})") }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    fn fresh_dir() -> PathBuf {
        let mut b = [0u8; 6];
        getrandom::getrandom(&mut b).unwrap();
        let dir = std::env::temp_dir().join(format!("swl-ipc-test-{}", hex::encode(b)));
        std::fs::create_dir(&dir).unwrap();
        dir
    }

    #[test]
    fn parses_cli_flags() {
        let a = parse_args(["swiftlane.exe", "--minimized", "--page", "calculator"].map(String::from));
        assert!(a.minimized);
        assert_eq!(a.start_page.as_deref(), Some("calculator"));
        assert!(a.explorer.is_none());

        let b = parse_args(["swiftlane.exe", "--share", "C:\\a.txt", "C:\\b.txt"].map(String::from));
        assert_eq!(b.explorer, Some(("share".into(), vec!["C:\\a.txt".into(), "C:\\b.txt".into()])));

        let c = parse_args(["swiftlane.exe", "--uninstall"].map(String::from));
        assert!(c.uninstall);

        let d = parse_args(["swiftlane.exe", "--shrink", "C:\\a.mp4"].map(String::from));
        assert_eq!(d.explorer, Some(("shrink".into(), vec!["C:\\a.mp4".into()])));
    }

    #[test]
    fn cleans_messages_and_authenticates_ipc() {
        let dir = fresh_dir();
        let sample = dir.join("hello.txt");
        std::fs::write(&sample, b"hi").unwrap();
        let sample_str = sample.to_string_lossy().to_string();

        assert!(clean_message(&json!({ "cmd": "evil", "paths": [&sample_str] })).is_none());
        assert!(clean_message(&json!({ "cmd": "share", "paths": [dir.join("missing.txt").to_string_lossy()] })).is_none());
        let cleaned = clean_message(&json!({ "cmd": "convert", "paths": [&sample_str] })).unwrap();
        assert_eq!(cleaned.0, "convert");
        assert_eq!(cleaned.1.len(), 1);

        let (tx, rx) = mpsc::channel();
        let _port = serve_in_dir(dir.clone(), move |cmd, paths| {
            let _ = tx.send((cmd, paths));
        })
        .unwrap();

        assert!(send_to_dir(&dir, "share", std::slice::from_ref(&sample_str)));
        let (cmd, paths) = rx.recv_timeout(Duration::from_secs(3)).unwrap();
        assert_eq!(cmd, "share");
        assert_eq!(paths.len(), 1);

        // Wrong key must be rejected
        let wrong_dir = fresh_dir();
        std::fs::copy(dir.join("ipc.port"), wrong_dir.join("ipc.port")).unwrap();
        std::fs::write(wrong_dir.join("ipc.key"), [9u8; 32]).unwrap();
        assert!(!send_to_dir(&wrong_dir, "share", std::slice::from_ref(&sample_str)));

        std::fs::remove_dir_all(&wrong_dir).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
