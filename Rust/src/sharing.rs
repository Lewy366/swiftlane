//! Share links in the app: the Share links page's calls, and keeping the share server and Tailscale
//! Funnel on exactly while a link can be used (same as the Python version's share_* methods).

use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::{AppHandle, Manager};
use tauri_plugin_dialog::DialogExt;

use crate::api::{push, AppState};
use crate::share_server::{self, ServerState};
use crate::{config, funnel, notify, transfer};

const READY_WAIT: Duration = Duration::from_secs(180); // the first time, Tailscale needs a minute or two

#[derive(Default)]
pub struct Sharing {
    pub server: Mutex<Option<(Arc<ServerState>, tokio::sync::oneshot::Sender<()>)>>,
    funnel_state: Mutex<Option<Value>>,
    note: Mutex<Option<Value>>,
    host: Mutex<Option<String>>,
    link_ready: Mutex<Option<bool>>, // None: not checked yet / checking
    checking: Mutex<bool>,
    sync_lock: Mutex<()>,
}

fn st(app: &AppHandle) -> tauri::State<'_, AppState> {
    app.state::<AppState>()
}

fn base_url(app: &AppHandle) -> Option<String> {
    st(app).sharing.host.lock().unwrap().as_ref().map(|h| format!("https://{h}"))
}

fn refresh_funnel(app: &AppHandle) {
    let s = funnel::status();
    let sh = &st(app).sharing;
    if let Some(host) = s["host"].as_str().filter(|h| !h.is_empty()) {
        *sh.host.lock().unwrap() = Some(host.to_string());
        if let Some(srv) = share_server::server(app) {
            *srv.host_name.lock().unwrap() = Some(host.to_string());
        }
    }
    *sh.funnel_state.lock().unwrap() = Some(s);
}

pub fn info(app: &AppHandle, check: bool) -> Value {
    if check || st(app).sharing.funnel_state.lock().unwrap().is_none() {
        refresh_funnel(app);
    }
    let s = st(app);
    json!({
        "shares": s.shares.summary(base_url(app).as_deref()),
        "funnel": *s.sharing.funnel_state.lock().unwrap(),
        "ready": *s.sharing.link_ready.lock().unwrap(),
        "note": *s.sharing.note.lock().unwrap(),
        "defaults": s.cfg.lock().unwrap()["share"].clone(),
        "busy": share_server::server(app).map(|x| x.busy.load(Ordering::Relaxed)).unwrap_or(0),
    })
}

fn push_shares(app: &AppHandle) {
    push(app, "shares", info(app, false));
}

fn feature_on(app: &AppHandle) -> bool {
    st(app).cfg.lock().unwrap()["features"]["share"].as_bool().unwrap_or(true)
}

fn needed(app: &AppHandle) -> bool {
    let busy = share_server::server(app).map(|x| x.busy.load(Ordering::Relaxed)).unwrap_or(0);
    feature_on(app) && (st(app).shares.any_active() || busy > 0)
}

fn ensure_server(app: &AppHandle) {
    if share_server::server(app).is_some() {
        return;
    }
    if st(app).sharing.host.lock().unwrap().is_none() {
        refresh_funnel(app);
    }
    let host = st(app).sharing.host.lock().unwrap().clone();
    if let Err(e) = share_server::start(app, host) {
        *st(app).sharing.note.lock().unwrap() = Some(json!({ "error": e }));
    }
}

/// Server and Funnel on while a link can be used (or a download is running), off otherwise.
pub fn sync(app: &AppHandle) {
    {
        let s = st(app);
        let _one_at_a_time = s.sharing.sync_lock.lock().unwrap();
        if needed(app) {
            ensure_server(app);
            match funnel::on() {
                Ok(r) => {
                    *s.sharing.note.lock().unwrap() = r.get("approve").map(|a| json!({ "approve": a }));
                    if r["ok"] == true && *s.sharing.link_ready.lock().unwrap() != Some(true) {
                        *s.sharing.link_ready.lock().unwrap() = None;
                        let app2 = app.clone();
                        std::thread::spawn(move || check_link_ready(&app2));
                    }
                }
                Err(e) => *s.sharing.note.lock().unwrap() = Some(json!({ "error": e })),
            }
        } else {
            funnel::off();
            share_server::stop(app);
            *s.sharing.note.lock().unwrap() = None;
            *s.sharing.link_ready.lock().unwrap() = None; // check again next time it's switched on
        }
        refresh_funnel(app);
    }
    push_shares(app);
}

fn sync_later(app: &AppHandle) {
    let app = app.clone();
    std::thread::spawn(move || sync(&app));
}

/// Funnel is on: try the link from the internet side until it works, then say so.
fn check_link_ready(app: &AppHandle) {
    let s = st(app);
    {
        let mut c = s.sharing.checking.lock().unwrap();
        if *c {
            return;
        }
        *c = true;
    }
    let host = s.sharing.funnel_state.lock().unwrap().as_ref().and_then(|f| f["host"].as_str().map(String::from))
        .or_else(|| s.sharing.host.lock().unwrap().clone());
    if let (Some(host), Some(srv)) = (host, share_server::server(app)) {
        let mut nonce = [0u8; 18];
        let _ = getrandom::getrandom(&mut nonce);
        let nonce = hex::encode(nonce);
        *srv.ready_nonce.lock().unwrap() = Some(nonce.clone());
        let deadline = std::time::Instant::now() + READY_WAIT;
        let mut why = String::new();
        let result = loop {
            if std::time::Instant::now() > deadline || share_server::server(app).map_or(true, |x| !Arc::ptr_eq(&x, &srv)) {
                break Some(false);
            }
            let (ok, w) = funnel::reachable(&host, &nonce);
            why = w;
            match ok {
                None => {
                    config::log(&format!("Share links: not checked from the internet ({why})"));
                    break Some(true); // can't check here: don't hold the link back
                }
                Some(true) => {
                    config::log("Share links checked: reachable from the internet");
                    break Some(true);
                }
                Some(false) => std::thread::sleep(Duration::from_secs(4)),
            }
        };
        if result == Some(false) {
            config::log(&format!("Share links not reachable from the internet after {} s ({why})", READY_WAIT.as_secs()));
        }
        *s.sharing.link_ready.lock().unwrap() = result;
        *srv.ready_nonce.lock().unwrap() = None;
    }
    *s.sharing.checking.lock().unwrap() = false;
    push_shares(app);
}

/// Every 30 s: forget old shares, close everything once nothing is shared, keep the PC awake.
pub fn share_loop(app: AppHandle) {
    sync(&app); // also closes a Funnel left open by a crash
    loop {
        std::thread::sleep(Duration::from_secs(30));
        st(&app).shares.prune();
        let need = needed(&app);
        if need != share_server::server(&app).is_some() {
            sync(&app);
        } else if st(&app).shares.has_any() {
            push_shares(&app);
        }
        if need && st(&app).cfg.lock().unwrap()["share"]["keep_awake"].as_bool().unwrap_or(true) {
            funnel::keep_awake();
        }
    }
}

/// Swiftlane is closing: links stop working until it runs again, so close Funnel too.
pub fn shutdown(app: &AppHandle) {
    funnel::off();
    share_server::stop(app);
}

pub fn downloaded(app: &AppHandle, s: &Value, link: &Value) {
    notify::show(app, "Download started", &format!("{} · {}", s["title"].as_str().unwrap_or(""), link["label"].as_str().unwrap_or("")));
    push_shares(app);
}

pub fn uploaded(app: &AppHandle, s: &Value, name: &str) {
    notify::show(app, "File received through a link", &format!("{name} · {}", s["title"].as_str().unwrap_or("")));
    push_shares(app);
}

// ------------------------------------------------------------------ the Share links page's calls

pub fn copy_text(text: &str) -> bool {
    arboard::Clipboard::new().and_then(|mut c| c.set_text(text.to_string())).is_ok()
}

pub fn paste_text() -> Value {
    arboard::Clipboard::new().and_then(|mut c| c.get_text()).map(Value::String).unwrap_or(Value::Null)
}

fn link_url(app: &AppHandle, share_id: &str, first_only: bool) -> Option<String> {
    let list = st(app).shares.summary(base_url(app).as_deref());
    let s = list.as_array()?.iter().find(|s| s["id"] == share_id)?;
    let links = s["links"].as_array()?;
    if first_only && links.len() != 1 {
        return None;
    }
    links.first()?["url"].as_str().map(String::from)
}

fn after_change(app: &AppHandle, r: Result<(), String>, resync: bool) -> Value {
    if let Err(e) = r {
        return json!({ "error": e });
    }
    if resync {
        sync_later(app);
    }
    info(app, false)
}

pub fn dispatch(app: &AppHandle, method: &str, args: &[Value]) -> Option<Value> {
    let a = |i: usize| args.get(i).cloned().unwrap_or(Value::Null);
    let s = st(app);
    let id = a(0).as_str().unwrap_or("").to_string();
    Some(match method {
        "share_info" => info(app, a(0).as_bool().unwrap_or(false)),
        "share_create" => {
            if !feature_on(app) {
                return Some(json!({ "error": "Sharing is switched off in Settings." }));
            }
            let paths: Vec<PathBuf> = a(0).as_array().map(|v| v.iter().filter_map(|p| p.as_str()).map(PathBuf::from).collect()).unwrap_or_default();
            let r = match s.shares.create(&paths, &a(1), a(2).as_bool().unwrap_or(false)) {
                Ok(r) => r,
                Err(e) => return Some(json!({ "error": e })),
            };
            if r.get("confirm").is_some() {
                return Some(r);
            }
            ensure_server(app);
            let new_id = r["share"].as_str().unwrap_or("").to_string();
            let copied = link_url(app, &new_id, true).filter(|u| copy_text(u));
            sync_later(app);
            let mut out = info(app, false);
            out["share"] = json!(new_id);
            out["copied"] = json!(copied);
            out
        }
        "share_request" => {
            if !feature_on(app) {
                return Some(json!({ "error": "Sharing is switched off in Settings." }));
            }
            let o = a(0);
            let title = o["title"].as_str().unwrap_or("").trim().chars().take(80).collect::<String>();
            let title = if title.is_empty() { "Files for me".to_string() } else { title };
            let save = PathBuf::from(s.cfg.lock().unwrap()["save_dir"].as_str().unwrap_or(""));
            let folder = save.join("From links").join(transfer::safe_name(&format!("{title} {}", chrono::Local::now().format("%Y-%m-%d"))));
            let r = match s.shares.create_request(&folder, &title, &o) {
                Ok(r) => r,
                Err(e) => return Some(json!({ "error": e })),
            };
            ensure_server(app);
            let new_id = r["share"].as_str().unwrap_or("").to_string();
            let copied = link_url(app, &new_id, false).filter(|u| copy_text(u));
            sync_later(app);
            let mut out = info(app, false);
            out["share"] = json!(new_id);
            out["copied"] = json!(copied);
            out
        }
        "share_open_folder" => {
            if let Some(folder) = s.shares.get(&id).and_then(|x| x["folder"].as_str().map(PathBuf::from)) {
                let _ = std::fs::create_dir_all(&folder);
                crate::winapi::open_folder(&folder);
            }
            Value::Null
        }
        "share_extend" => after_change(app, s.shares.extend(&id, a(1).as_f64().unwrap_or(0.0)), true),
        "share_end" => after_change(app, s.shares.end(&id), true),
        "share_end_all" => {
            s.shares.end_all();
            after_change(app, Ok(()), true)
        }
        "share_remove" => {
            s.shares.remove(&id);
            after_change(app, Ok(()), true)
        }
        "share_password" => after_change(app, s.shares.set_password(&id, a(1).as_str().unwrap_or("")), false),
        "share_add_links" => {
            let labels: Vec<String> = a(2).as_array().map(|v| v.iter().filter_map(|x| x.as_str()).map(|x| x.trim().chars().take(60).collect::<String>()).filter(|x| !x.is_empty()).collect()).unwrap_or_default();
            after_change(app, s.shares.add_links(&id, a(1).as_u64().unwrap_or(1) as usize, &labels, 1), true)
        }
        "share_link" => after_change(app, s.shares.set_link(&id, a(1).as_str().unwrap_or(""), &a(2)), true),
        "share_copy" => {
            let list = s.shares.summary(base_url(app).as_deref());
            let url = list.as_array().into_iter().flatten().filter(|x| x["id"] == id.as_str())
                .flat_map(|x| x["links"].as_array().cloned().unwrap_or_default())
                .find(|l| l["id"] == a(1)).and_then(|l| l["url"].as_str().map(String::from));
            json!(url.filter(|u| copy_text(u)))
        }
        "share_retry" => {
            {
                let mut r = s.sharing.link_ready.lock().unwrap();
                if *r == Some(false) {
                    *r = None; // try the internet-side check again
                }
            }
            sync(app);
            info(app, false)
        }
        "share_open_page" => {
            // only Tailscale's own pages
            if let Some(url) = a(0).as_str().filter(|u| u.starts_with("https://login.tailscale.com/") && u.len() < 500) {
                let _ = std::process::Command::new("explorer").arg(url).spawn();
            }
            Value::Null
        }
        "share_pick" => {
            let d = app.dialog().file();
            let picked: Vec<String> = if a(0).as_bool().unwrap_or(false) {
                d.blocking_pick_folder().and_then(|p| p.into_path().ok()).into_iter().map(|p| p.to_string_lossy().to_string()).collect()
            } else {
                d.blocking_pick_files().unwrap_or_default().into_iter().filter_map(|p| p.into_path().ok()).map(|p| p.to_string_lossy().to_string()).collect()
            };
            json!(picked)
        }
        "copy_text" => json!(copy_text(a(0).as_str().unwrap_or(""))),
        "paste_text" => paste_text(),
        _ => return None,
    })
}

/// Sharing switched off in Settings: every link ends and Funnel closes.
pub fn feature_switched(app: &AppHandle, on: bool) {
    if !on {
        st(app).shares.end_all();
    }
    sync_later(app);
}
