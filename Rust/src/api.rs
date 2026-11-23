//! The bridge the interface talks to. The page calls `App.api.<name>(...)` exactly as it did with
//! Python; js/bridge.js turns that into one `call` command with the name and arguments, which runs
//! here off the window's thread (so file dialogs and downloads never freeze the window).
//! Tools are ported one by one; until then a call answers with a plain-language note.

use base64::Engine;
use serde_json::{json, Value};
use std::sync::Mutex;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_dialog::DialogExt;

use crate::{config, transfer, updater, winapi};

pub struct AppState {
    pub cfg: Mutex<Value>,
    pub update: Mutex<Option<updater::Update>>,
    pub updating: Mutex<bool>,
    pub transfer: transfer::Transfer,
}

impl AppState {
    pub fn load() -> Self {
        AppState { cfg: Mutex::new(config::load()), update: Mutex::new(None), updating: Mutex::new(false), transfer: transfer::Transfer::new() }
    }
}

const TOOL_LISTS: &str = include_str!("tool_lists.json");
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Sends a live event to the page (window.onCore(event, data)).
pub fn push(app: &AppHandle, event: &str, data: Value) {
    let _ = app.emit("core", json!({ "event": event, "data": data }));
}

#[tauri::command]
pub async fn call(app: AppHandle, method: String, args: Vec<Value>) -> Value {
    tauri::async_runtime::spawn_blocking(move || dispatch(&app, &method, &args))
        .await
        .unwrap_or_else(|_| json!({ "error": "Something went wrong inside Swiftlane." }))
}

fn arg(args: &[Value], i: usize) -> &Value {
    args.get(i).unwrap_or(&Value::Null)
}

fn dispatch(app: &AppHandle, method: &str, args: &[Value]) -> Value {
    let st = app.state::<AppState>();
    match method {
        "get_state" => get_state(&st),
        "get_background" => get_background(&st),
        // ---- settings
        "set_option" => json!(set_option(&st, arg(args, 0).as_str().unwrap_or(""), arg(args, 1))),
        "set_theme" => set_theme(&st, arg(args, 0)),
        "set_feature" => set_feature(app, &st, arg(args, 0).as_str().unwrap_or(""), arg(args, 1).as_bool().unwrap_or(false)),
        "set_notify" => edit(&st, |c| c["notify"] = json!(arg(args, 0).as_bool().unwrap_or(true))),
        "set_autostart" => json!(winapi::set_autostart(arg(args, 0).as_bool().unwrap_or(false))),
        "set_sidebar" => json!(set_sidebar(&st, arg(args, 0))),
        "set_clocks" => json!(set_clocks(&st, arg(args, 0))),
        "set_share_defaults" => set_share_defaults(&st, arg(args, 0)),
        "choose_download_dir" => choose_download_dir(app, &st),
        "reset_download_dir" => edit(&st, |c| c["download_dir"] = json!("")),
        "choose_background" => choose_background(app, &st),
        "clear_background" => clear_background(&st),
        "open_folder" => {
            let dir = st.cfg.lock().unwrap()["save_dir"].as_str().map(std::path::PathBuf::from);
            json!(dir.map(|d| winapi::open_folder(&d)).unwrap_or(false))
        }
        // ---- phone transfer
        "add_outbox" => {
            st.transfer.outbox_add(&paths(arg(args, 0)));
            st.transfer.outbox_list()
        }
        "get_outbox" => st.transfer.outbox_list(),
        "remove_outbox" => {
            st.transfer.outbox_remove(&id_arg(arg(args, 0)));
            st.transfer.outbox_list()
        }
        "clear_outbox" => {
            st.transfer.outbox_clear();
            json!([])
        }
        "open_received" => json!(st.transfer.find_received(&id_arg(arg(args, 0))).is_some_and(|p| winapi::shell_open(std::path::Path::new(&p)))),
        "reveal_received" => json!(st.transfer.find_received(&id_arg(arg(args, 0))).is_some_and(|p| winapi::reveal(std::path::Path::new(&p)))),
        "open_path" => json!(arg(args, 0).as_str().is_some_and(|p| winapi::shell_open(std::path::Path::new(p)))),
        "reveal_path" => json!(arg(args, 0).as_str().is_some_and(|p| winapi::reveal(std::path::Path::new(p)))),
        "pick_files" => pick_files(app),
        "choose_folder" => choose_save_dir(app, &st),
        "new_code" => {
            edit(&st, |c| c["code"] = json!(config::new_code()));
            config::log("Pairing code reset");
            get_state(&st)
        }
        // ---- updates
        "check_update" => check_update(app, &st),
        "install_update" => install_update(app, &st),
        _ => json!({ "error": format!("“{method}” isn't in the Rust version of Swiftlane yet.") }),
    }
}

fn edit(st: &AppState, f: impl FnOnce(&mut Value)) -> Value {
    let mut cfg = st.cfg.lock().unwrap();
    f(&mut cfg);
    config::save(&cfg);
    Value::Null
}

fn is_id(s: &str, min: usize, max: usize) -> bool {
    (min..=max).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_lowercase())
}

fn clean_name(s: &str, max: usize) -> String {
    s.chars().filter(|c| !c.is_control()).take(max).collect::<String>().trim().to_string()
}

pub fn get_state(st: &AppState) -> Value {
    let cfg = st.cfg.lock().unwrap().clone();
    let update = st.update.lock().unwrap().as_ref().filter(|u| u.available).map(|u| u.public());
    let running = st.transfer.running.lock().unwrap().is_some();
    let code = cfg["code"].as_str().unwrap_or("").to_string();
    let port = cfg["port"].as_u64().unwrap_or(8765);
    let addrs = transfer::local_addresses();
    let ca = st.transfer.ca.lock().unwrap().clone();
    let https = cfg["https_only"].as_bool().unwrap_or(false) && ca.is_some();
    let scheme = if https { "https" } else { "http" };
    let addresses: Vec<Value> = addrs.iter().map(|(label, ip)| json!({
        "label": label, "host": format!("{ip}:{port}"), "shortcut": format!("{scheme}://{ip}:{port}/upload?name=")
    })).collect();
    let qr = transfer::qr_svg(&format!("{scheme}://{}:{port}/#{code}", addrs[0].1));
    let tls_info = match &ca {
        Some(ca) => json!({
            "ready": true, "https_only": cfg["https_only"], "fingerprint": ca.fingerprint(),
            "cert_qr": transfer::qr_svg(&format!("http://{}:{port}/cert", addrs[0].1)),
            "secure_qr": transfer::qr_svg(&format!("https://{}:{port}/#{code}", addrs[0].1)),
        }),
        None => json!({ "ready": false, "https_only": false, "fingerprint": "", "cert_qr": null, "secure_qr": null }),
    };
    let mut s = json!({
        "version": VERSION,
        "rust": true,
        "features": cfg["features"],
        "server_running": running,
        "server_error": *st.transfer.error.lock().unwrap(),
        "code_pretty": config::pretty_code(&code),
        "qr": qr,
        "tls": tls_info,
        "addresses": addresses,
        "save_dir": cfg.get("save_dir").cloned().unwrap_or(json!("")),
        "notify": cfg["notify"],
        "autostart": winapi::autostart_enabled(),
        "config_dir": config::config_dir().to_string_lossy(),
        "outbox": st.transfer.outbox_list(),
        "received": st.transfer.received_list(),
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
        "update": update,
        "updated": take_whats_new()
    });
    // Lists the tools show (formats, units, constants...), taken over from the Python version as they are.
    if let (Value::Object(out), Ok(Value::Object(lists))) = (&mut s, serde_json::from_str::<Value>(TOOL_LISTS)) {
        out.extend(lists);
    }
    s
}

// ------------------------------------------------------------------ settings (same rules as Python)

fn set_option(st: &AppState, name: &str, v: &Value) -> bool {
    let mut cfg = st.cfg.lock().unwrap();
    let as_bool = v.as_bool().unwrap_or(false);
    match name {
        "sort_files" | "update_check" | "https_only" | "net_watch" | "remote_power" => cfg[name] = json!(as_bool),
        "calc_angle" if matches!(v.as_str(), Some("deg" | "rad" | "gra")) => cfg[name] = v.clone(),
        "language" if matches!(v.as_str(), Some("en" | "de" | "pl" | "it")) => cfg[name] = v.clone(),
        "vault_lock_minutes" if matches!(v.as_i64(), Some(1 | 5 | 15 | 30 | 60)) => cfg[name] = v.clone(),
        _ => return false,
    }
    config::save(&cfg);
    if name == "remote_power" {
        config::log(&format!("PC remote: sleep and shut down from the phone {}", if as_bool { "allowed" } else { "not allowed" }));
    }
    true
}

fn set_theme(st: &AppState, theme: &Value) -> Value {
    let mut cfg = st.cfg.lock().unwrap();
    if let Value::Object(t) = theme {
        for (k, v) in t {
            let ok = match k.as_str() {
                "accent" => v.as_str().is_some_and(|s| s.len() < 20),
                "custom" => v.as_str().is_some_and(|s| s.len() == 7 && s.starts_with('#') && s[1..].bytes().all(|b| b.is_ascii_hexdigit())),
                "blur_on" => v.is_boolean(),
                "mode" => matches!(v.as_str(), Some("dark" | "light" | "auto")),
                "blur" | "dim" => v.is_number(),
                _ => false,
            };
            if ok {
                cfg["theme"][k] = match k.as_str() {
                    "blur" | "dim" => json!(v.as_f64().unwrap_or(0.0).clamp(0.0, 100.0) as i64),
                    _ => v.clone(),
                };
            }
        }
    }
    config::save(&cfg);
    cfg["theme"].clone()
}

const FEATURES: &[&str] = &[
    "transfer", "share", "remote", "pdf", "trimmer", "vault", "diskmap", "netscan", "exposure", "contacts",
    "worldclock", "converter", "images", "downloader", "currency", "calculator", "speedtest", "adblock",
];

fn set_feature(app: &AppHandle, st: &AppState, name: &str, on: bool) -> Value {
    if FEATURES.contains(&name) {
        edit(st, |c| c["features"][name] = json!(on));
        if name == "transfer" {
            if on {
                let _ = transfer::start(app);
            } else {
                transfer::stop(app);
            }
        }
    }
    get_state(st)
}

fn paths(v: &Value) -> Vec<std::path::PathBuf> {
    v.as_array().map(|a| a.iter().filter_map(|p| p.as_str()).map(std::path::PathBuf::from).collect()).unwrap_or_default()
}

fn id_arg(v: &Value) -> String {
    v.as_str().map(String::from).or_else(|| v.as_u64().map(|n| n.to_string())).unwrap_or_default()
}

/// What the page needs to know about a file: name, size, and what it can be converted to.
pub fn file_info(path: &std::path::Path) -> Value {
    let lists: Value = serde_json::from_str(TOOL_LISTS).unwrap_or(Value::Null);
    let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    let kind = ["image", "video", "audio", "pdf"].into_iter().find(|k| {
        lists["formats"][*k]["in"].as_array().is_some_and(|a| a.iter().any(|x| x.as_str() == Some(ext.as_str())))
    });
    json!({
        "path": path.to_string_lossy(),
        "name": path.file_name().map(|n| n.to_string_lossy()).unwrap_or_default(),
        "size": std::fs::metadata(path).map(|m| if m.is_file() { m.len() } else { 0 }).unwrap_or(0),
        "kind": kind,
        "targets": kind.map(|k| lists["formats"][k]["out"].clone()).unwrap_or(json!([])),
    })
}

fn pick_files(app: &AppHandle) -> Value {
    let picked = app.dialog().file().blocking_pick_files().unwrap_or_default();
    json!(picked.into_iter().filter_map(|p| p.into_path().ok()).map(|p| file_info(&p)).collect::<Vec<_>>())
}

fn choose_save_dir(app: &AppHandle, st: &AppState) -> Value {
    let start = st.cfg.lock().unwrap()["save_dir"].as_str().map(String::from);
    let mut d = app.dialog().file();
    if let Some(s) = start {
        d = d.set_directory(s);
    }
    let Some(path) = d.blocking_pick_folder().and_then(|p| p.into_path().ok()) else { return Value::Null };
    if std::fs::create_dir_all(&path).is_err() {
        return json!({ "error": "Can't use that folder." });
    }
    let s = path.to_string_lossy().to_string();
    edit(st, |c| c["save_dir"] = json!(s.clone()));
    config::log(&format!("Save folder changed to {s}"));
    json!(s)
}

fn set_sidebar(st: &AppState, layout: &Value) -> bool {
    let ids = |v: &Value| -> Vec<Value> {
        v.as_array().map(|a| a.iter().filter_map(|x| x.as_str()).filter(|s| is_id(s, 2, 24)).take(60).map(|s| json!(s)).collect()).unwrap_or_default()
    };
    let folders: Vec<Value> = layout["folders"].as_array().map(|a| {
        a.iter().take(20).map(|f| {
            let name = clean_name(f["name"].as_str().unwrap_or(""), 40);
            json!({ "name": if name.is_empty() { "Folder".into() } else { name }, "items": ids(&f["items"]), "open": f["open"].as_bool().unwrap_or(true) })
        }).collect()
    }).unwrap_or_default();
    let clean = json!({ "pinned": ids(&layout["pinned"]), "folders": folders });
    edit(st, |c| c["sidebar"] = clean);
    true
}

fn set_clocks(st: &AppState, ids: &Value) -> bool {
    let Some(list) = ids.as_array() else { return false };
    let clean: Vec<Value> = list.iter().filter_map(|x| x.as_str()).filter(|s| is_id(s, 2, 20)).take(24).map(|s| json!(s)).collect();
    edit(st, |c| c["clocks"] = json!(clean));
    true
}

fn set_share_defaults(st: &AppState, v: &Value) -> Value {
    let mut cfg = st.cfg.lock().unwrap();
    let d = &mut cfg["share"];
    if let Some(h) = v["hours"].as_f64().filter(|h| *h > 0.0 && *h <= 24.0 * 365.0) {
        d["hours"] = json!(h);
    }
    if matches!(v["mode"].as_str(), Some("open" | "personal")) {
        d["mode"] = v["mode"].clone();
    }
    if v.get("count").is_some() {
        d["count"] = v["count"].as_i64().filter(|n| *n > 0).map(|n| json!(n)).unwrap_or(Value::Null);
    }
    if let Some(p) = v["people"].as_i64().filter(|p| (1..=100).contains(p)) {
        d["people"] = json!(p);
    }
    if let Some(k) = v["keep_awake"].as_bool() {
        d["keep_awake"] = json!(k);
    }
    config::save(&cfg);
    cfg["share"].clone()
}

fn choose_download_dir(app: &AppHandle, st: &AppState) -> Value {
    let start = {
        let c = st.cfg.lock().unwrap();
        c["download_dir"].as_str().filter(|s| !s.is_empty()).or(c["save_dir"].as_str()).map(String::from)
    };
    let mut d = app.dialog().file();
    if let Some(s) = start {
        d = d.set_directory(s);
    }
    match d.blocking_pick_folder().and_then(|p| p.into_path().ok()) {
        Some(path) => {
            let s = path.to_string_lossy().to_string();
            edit(st, |c| c["download_dir"] = json!(s.clone()));
            json!(s)
        }
        None => Value::Null,
    }
}

// ------------------------------------------------------------------ background picture

const BG_MAX: u64 = 60 * 1024 * 1024;

fn get_background(st: &AppState) -> Value {
    let (on, name) = {
        let cfg = st.cfg.lock().unwrap();
        let name = cfg["theme"]["bg_file"].as_str().filter(|n| !n.is_empty()).unwrap_or("background.jpg").to_string();
        (cfg["theme"]["background"].as_bool().unwrap_or(false), name)
    };
    // Only a plain file name inside Swiftlane's own folder, never a path somewhere else.
    if !on || name.contains(['/', '\\', ':']) || name.starts_with('.') {
        return Value::Null;
    }
    let path = config::config_dir().join(&name);
    if std::fs::metadata(&path).map(|m| m.len() > BG_MAX).unwrap_or(true) {
        return Value::Null;
    }
    let Ok(bytes) = std::fs::read(&path) else { return Value::Null };
    let mime = match name.rsplit('.').next().unwrap_or("").to_lowercase().as_str() {
        "gif" => "image/gif",
        "webp" => "image/webp",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        _ => "image/jpeg",
    };
    json!(format!("data:{mime};base64,{}", base64::engine::general_purpose::STANDARD.encode(bytes)))
}

fn remove_old_backgrounds() {
    if let Ok(entries) = std::fs::read_dir(config::config_dir()) {
        for e in entries.flatten() {
            if e.file_name().to_string_lossy().starts_with("background.") {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
}

/// Pictures become a resized JPEG copy; animated GIF/WebP and short videos are copied as they are.
fn choose_background(app: &AppHandle, st: &AppState) -> Value {
    let picked = app.dialog().file()
        .add_filter("Pictures, GIFs and videos", &["jpg", "jpeg", "png", "webp", "bmp", "gif", "mp4", "webm"])
        .blocking_pick_file()
        .and_then(|p| p.into_path().ok());
    let Some(src) = picked else { return Value::Null };
    let ext = src.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    let dir = config::config_dir();
    let _ = std::fs::create_dir_all(&dir);
    let size = std::fs::metadata(&src).map(|m| m.len()).unwrap_or(0);
    let animated = matches!(ext.as_str(), "mp4" | "webm") || (matches!(ext.as_str(), "gif" | "webp") && is_animated(&src, &ext));
    let target = if animated {
        if size > BG_MAX {
            return json!({ "error": "That file is over 60 MB. Pick a shorter or smaller animation." });
        }
        remove_old_backgrounds();
        let t = dir.join(format!("background.{ext}"));
        if std::fs::copy(&src, &t).is_err() {
            return json!({ "error": "Couldn't copy that file." });
        }
        t
    } else {
        let img = match image::open(&src) {
            Ok(i) => i,
            Err(_) => return json!({ "error": "Can't use that picture. Try a JPG or PNG." }),
        };
        let img = img.thumbnail(2560, 2560).to_rgb8();
        remove_old_backgrounds();
        let t = dir.join("background.jpg");
        let Ok(f) = std::fs::File::create(&t) else { return json!({ "error": "Couldn't save the picture." }) };
        let mut w = std::io::BufWriter::new(f);
        if image::codecs::jpeg::JpegEncoder::new_with_quality(&mut w, 88).encode_image(&img).is_err() {
            return json!({ "error": "Couldn't save the picture." });
        }
        t
    };
    let name = target.file_name().unwrap().to_string_lossy().to_string();
    edit(st, |c| {
        c["theme"]["background"] = json!(true);
        c["theme"]["bg_file"] = json!(name);
    });
    get_background(st)
}

fn is_animated(path: &std::path::Path, ext: &str) -> bool {
    use image::AnimationDecoder;
    let Ok(f) = std::fs::File::open(path) else { return false };
    let r = std::io::BufReader::new(f);
    match ext {
        "gif" => image::codecs::gif::GifDecoder::new(r).map(|d| d.into_frames().take(2).count() > 1).unwrap_or(false),
        "webp" => image::codecs::webp::WebPDecoder::new(r).map(|d| d.has_animation()).unwrap_or(false),
        _ => false,
    }
}

fn clear_background(st: &AppState) -> Value {
    edit(st, |c| {
        c["theme"]["background"] = json!(false);
        c["theme"]["bg_file"] = json!("");
    });
    remove_old_backgrounds();
    Value::Null
}

// ------------------------------------------------------------------ updates

fn whats_new_path() -> std::path::PathBuf {
    config::config_dir().join("update-notes.json")
}

/// After an update: what's new in this version, shown once.
fn take_whats_new() -> Value {
    let path = whats_new_path();
    let Ok(text) = std::fs::read_to_string(&path) else { return Value::Null };
    let Ok(data) = serde_json::from_str::<Value>(&text) else { return Value::Null };
    if updater::version_tuple(data["version"].as_str().unwrap_or("")) != updater::version_tuple(VERSION) {
        return Value::Null;
    }
    let _ = std::fs::remove_file(path);
    json!({ "version": VERSION, "notes": data["notes"] })
}

fn check_update(_app: &AppHandle, st: &AppState) -> Value {
    match updater::check(VERSION) {
        Ok(u) => {
            let public = u.available.then(|| u.public());
            let latest = u.version.clone();
            let available = u.available;
            *st.update.lock().unwrap() = Some(u);
            json!({ "available": available, "current": VERSION, "latest": latest, "update": public })
        }
        Err(e) => json!({ "error": e }),
    }
}

fn install_update(app: &AppHandle, st: &AppState) -> Value {
    let Some(u) = st.update.lock().unwrap().clone().filter(|u| u.available) else {
        return json!({ "error": "There's no update to install." });
    };
    {
        let mut busy = st.updating.lock().unwrap();
        if *busy {
            return json!({ "error": "There's no update to install." });
        }
        *busy = true;
    }
    let result = (|| {
        let path = updater::download(&u, |f| push(app, "update-progress", json!({ "fraction": f })))?;
        if !updater::still_matches(&path, &u) {
            return Err("The download changed after it was checked, so it was rejected.".to_string());
        }
        let _ = std::fs::write(whats_new_path(), json!({ "version": u.version, "notes": u.notes }).to_string());
        if !winapi::run_elevated(&path, "--update", path.parent().unwrap()) {
            let _ = std::fs::remove_file(whats_new_path());
            return Err("Windows didn't give permission, so nothing was changed.".to_string());
        }
        Ok(())
    })();
    *st.updating.lock().unwrap() = false;
    match result {
        Ok(()) => {
            config::log(&format!("Updating to {}", u.version));
            let app = app.clone();
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_secs(1));
                app.exit(0); // Setup takes over from here
            });
            json!({ "ok": true })
        }
        Err(e) => json!({ "error": e }),
    }
}

/// Look for updates every time Swiftlane starts, then every 30 minutes (5 after a failed check).
/// Tells the page; never installs anything by itself.
pub fn update_loop(app: AppHandle) {
    std::thread::sleep(std::time::Duration::from_secs(8));
    loop {
        let st = app.state::<AppState>();
        let enabled = st.cfg.lock().unwrap()["update_check"].as_bool().unwrap_or(true);
        let mut wait = 30 * 60;
        if enabled && !*st.updating.lock().unwrap() {
            let r = check_update(&app, &st);
            if r.get("error").is_some() {
                wait = 5 * 60;
            } else if r["update"].is_object() {
                push(&app, "update-available", r["update"].clone());
            }
        }
        std::thread::sleep(std::time::Duration::from_secs(wait));
    }
}
