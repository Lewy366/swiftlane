//! Send to contacts: Swiftlane to Swiftlane, straight from one PC to the other over iroh
//! (end-to-end encrypted QUIC, ALPN "swiftlane/send/1").
//! Port of Source/peers.py plus contacts_* / peer_* methods in Source/swiftlane.py.
//!
//! Wire-compatible with Python's iroh 1.2.0 bindings, sharing the same DPAPI-protected
//! 32-byte identity key (%APPDATA%\Swiftlane\contact-key.dpapi) and contacts.json.
//!
//! Security rules:
//! - Only contacts get in: connections from unknown EndpointIds are closed immediately.
//! - The receiver always decides: nothing is written until Accept (120 s timeout).
//! - Every file is verified by exact length + SHA-256; broken/cancelled .part files are deleted.
//! - Received files get Windows' Mark of the Web (:Zone.Identifier ZoneId=3) and never overwrite.

use base64::Engine;
use iroh::endpoint::presets;
use iroh::{Endpoint, EndpointId, SecretKey};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Manager};
use tokio::io::AsyncWriteExt;
use tokio::sync::oneshot;

use super::arg;
use crate::api::{push, AppState};
use crate::config;
use crate::jobs::JobError;
use crate::transfer::{category_for, place_file, record_created, safe_name};

const ALPN: &[u8] = b"swiftlane/send/1";
const CODE_PREFIX: &str = "SL1";
const FRAME_MAX: usize = 64 * 1024;
const MAX_FILES: usize = 1000;
const ASK_SECONDS: u64 = 120;
const CHUNK: usize = 1 << 20;
const HEADROOM: u64 = 512 * 1024 * 1024;
const MAX_LOG: usize = 50;

// ------------------------------------------------------------------ contact codes & safety words

pub fn clean_name(raw: &str) -> String {
    raw.chars()
        .filter(|c| (*c as u32) >= 0x20 && *c != '<' && *c != '>')
        .collect::<String>()
        .trim()
        .chars()
        .take(40)
        .collect()
}

pub fn make_code(endpoint_id: &str, name: &str) -> String {
    let label = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(clean_name(name).as_bytes());
    format!("{CODE_PREFIX}.{endpoint_id}.{label}")
}

pub fn parse_code(code: &str) -> Result<(String, String), String> {
    let compact: String = code.chars().filter(|c| !c.is_whitespace()).collect();
    let parts: Vec<&str> = compact.split('.').collect();
    if parts.len() != 3 || parts[0] != CODE_PREFIX {
        return Err("That isn't a Swiftlane contact code.".into());
    }
    let id = parts[1];
    if !(40..=80).contains(&id.len())
        || !id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
    {
        return Err("That isn't a Swiftlane contact code.".into());
    }
    let decoded_name = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(parts[2])
        .ok()
        .and_then(|b| String::from_utf8(b).ok())
        .map(|s| clean_name(&s))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "Contact".to_string());
    Ok((id.to_string(), decoded_name))
}

pub fn safety_words(endpoint_id: &str) -> String {
    let digest = hex::encode_upper(Sha256::digest(endpoint_id.as_bytes()));
    format!(
        "{}-{}-{}-{}",
        &digest[0..4],
        &digest[4..8],
        &digest[8..12],
        &digest[12..16]
    )
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn random_hex(bytes: usize) -> String {
    let mut b = vec![0u8; bytes];
    let _ = getrandom::getrandom(&mut b);
    hex::encode(b)
}

fn mark_from_internet(path: &Path) {
    if cfg!(windows) {
        let mut ads = path.as_os_str().to_os_string();
        ads.push(":Zone.Identifier");
        let _ = std::fs::write(
            PathBuf::from(ads),
            "[ZoneTransfer]\r\nZoneId=3\r\nReferrerUrl=about:internet\r\n",
        );
    }
}

// ------------------------------------------------------------------ DPAPI identity key (compatible with Python)

#[cfg(windows)]
mod dpapi {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{
        CryptProtectData, CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
    };

    fn run(data: &[u8], protect: bool) -> Result<Vec<u8>, String> {
        let input = CRYPT_INTEGER_BLOB {
            cbData: data.len() as u32,
            pbData: data.as_ptr() as *mut u8,
        };
        let mut out = CRYPT_INTEGER_BLOB {
            cbData: 0,
            pbData: std::ptr::null_mut(),
        };
        let desc: Vec<u16> = "Swiftlane".encode_utf16().chain(std::iter::once(0)).collect();
        let ok = unsafe {
            if protect {
                CryptProtectData(
                    &input,
                    desc.as_ptr(),
                    std::ptr::null(),
                    std::ptr::null(),
                    std::ptr::null(),
                    CRYPTPROTECT_UI_FORBIDDEN,
                    &mut out,
                )
            } else {
                CryptUnprotectData(
                    &input,
                    std::ptr::null_mut(),
                    std::ptr::null(),
                    std::ptr::null(),
                    std::ptr::null(),
                    CRYPTPROTECT_UI_FORBIDDEN,
                    &mut out,
                )
            }
        };
        if ok == 0 || out.pbData.is_null() {
            return Err("Windows couldn't unlock your contact key.".into());
        }
        let result = unsafe { std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec() };
        unsafe { LocalFree(out.pbData as _) };
        Ok(result)
    }

    pub fn protect(data: &[u8]) -> Result<Vec<u8>, String> {
        run(data, true)
    }
    pub fn unprotect(data: &[u8]) -> Result<Vec<u8>, String> {
        run(data, false)
    }
}

#[cfg(not(windows))]
mod dpapi {
    pub fn protect(data: &[u8]) -> Result<Vec<u8>, String> {
        Ok(data.to_vec())
    }
    pub fn unprotect(data: &[u8]) -> Result<Vec<u8>, String> {
        Ok(data.to_vec())
    }
}

fn load_or_create_secret_key(path: &Path) -> Result<[u8; 32], String> {
    if let Ok(raw) = std::fs::read(path) {
        if let Ok(plain) = dpapi::unprotect(&raw) {
            if plain.len() == 32 {
                let mut arr = [0u8; 32];
                arr.copy_from_slice(&plain);
                return Ok(arr);
            }
        }
    }
    let mut key = [0u8; 32];
    getrandom::getrandom(&mut key).map_err(|e| e.to_string())?;
    let protected = dpapi::protect(&key)?;
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let mut tmp = path.as_os_str().to_os_string();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, &protected).map_err(|e| e.to_string())?;
    if std::fs::rename(&tmp, path).is_err() {
        let _ = std::fs::remove_file(&tmp);
        std::fs::write(path, &protected).map_err(|e| e.to_string())?;
    }
    Ok(key)
}

// ------------------------------------------------------------------ contacts store

pub struct Contacts {
    path: PathBuf,
    items: Mutex<Vec<Value>>,
}

impl Contacts {
    pub fn load(path: PathBuf) -> Self {
        let items = std::fs::read_to_string(&path)
            .ok()
            .and_then(|t| serde_json::from_str::<Vec<Value>>(&t).ok())
            .unwrap_or_default();
        Self {
            path,
            items: Mutex::new(items),
        }
    }

    fn save_locked(&self, items: &[Value]) {
        if let Some(parent) = self.path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let Ok(text) = serde_json::to_string_pretty(items) else {
            return;
        };
        let mut tmp = self.path.as_os_str().to_os_string();
        tmp.push(".tmp");
        let tmp = PathBuf::from(tmp);
        if std::fs::write(&tmp, &text).is_ok() {
            if std::fs::rename(&tmp, &self.path).is_err() {
                let _ = std::fs::remove_file(&tmp);
                let _ = std::fs::write(&self.path, &text);
            }
        }
    }

    pub fn list(&self) -> Vec<Value> {
        let guard = self.items.lock().unwrap();
        guard
            .iter()
            .map(|c| {
                let mut v = c.clone();
                if let Some(id) = c["id"].as_str() {
                    v["safety"] = json!(safety_words(id));
                }
                v
            })
            .collect()
    }

    pub fn is_empty(&self) -> bool {
        self.items.lock().unwrap().is_empty()
    }

    pub fn get(&self, cid: &str) -> Option<Value> {
        self.items
            .lock()
            .unwrap()
            .iter()
            .find(|c| c["id"].as_str() == Some(cid))
            .cloned()
    }

    pub fn add(&self, code: &str, name: Option<&str>, own_id: Option<&str>) -> Result<Value, String> {
        let (cid, code_name) = parse_code(code)?;
        if own_id == Some(cid.as_str()) {
            return Err("That's your own contact code.".into());
        }
        let mut guard = self.items.lock().unwrap();
        if guard.iter().any(|c| c["id"].as_str() == Some(cid.as_str())) {
            return Err("That person is already in your contacts.".into());
        }
        let chosen = name
            .map(clean_name)
            .filter(|s| !s.is_empty())
            .unwrap_or(code_name);
        let entry = json!({
            "id": cid,
            "name": chosen,
            "added": now_unix(),
            "last": Value::Null,
        });
        guard.push(entry.clone());
        self.save_locked(&guard);
        Ok(entry)
    }

    pub fn remove(&self, cid: &str) {
        let mut guard = self.items.lock().unwrap();
        guard.retain(|c| c["id"].as_str() != Some(cid));
        self.save_locked(&guard);
    }

    pub fn rename(&self, cid: &str, name: &str) {
        let cleaned = clean_name(name);
        if cleaned.is_empty() {
            return;
        }
        let mut guard = self.items.lock().unwrap();
        for c in guard.iter_mut() {
            if c["id"].as_str() == Some(cid) {
                c["name"] = json!(cleaned);
            }
        }
        self.save_locked(&guard);
    }

    pub fn seen(&self, cid: &str) {
        let mut guard = self.items.lock().unwrap();
        let now = now_unix();
        for c in guard.iter_mut() {
            if c["id"].as_str() == Some(cid) {
                c["last"] = json!(now);
            }
        }
        self.save_locked(&guard);
    }
}

// ------------------------------------------------------------------ length-prefixed JSON framing

async fn send_frame(w: &mut iroh::endpoint::SendStream, obj: &Value) -> Result<(), String> {
    let data = serde_json::to_vec(obj).map_err(|e| e.to_string())?;
    let len = u32::try_from(data.len()).map_err(|_| "Frame too large".to_string())?;
    w.write_all(&len.to_be_bytes())
        .await
        .map_err(|_| "The connection was lost.".to_string())?;
    w.write_all(&data)
        .await
        .map_err(|_| "The connection was lost.".to_string())?;
    Ok(())
}

async fn read_frame(r: &mut iroh::endpoint::RecvStream) -> Result<Value, String> {
    let mut len_buf = [0u8; 4];
    r.read_exact(&mut len_buf)
        .await
        .map_err(|_| "The connection was lost.".to_string())?;
    let size = u32::from_be_bytes(len_buf) as usize;
    if size > FRAME_MAX {
        return Err("The other side sent something too big.".into());
    }
    let mut buf = vec![0u8; size];
    r.read_exact(&mut buf)
        .await
        .map_err(|_| "The connection was lost.".to_string())?;
    serde_json::from_slice(&buf).map_err(|_| "The other side sent invalid data.".into())
}

// ------------------------------------------------------------------ runtime & global peer manager

struct PeerManager {
    contacts: Arc<Contacts>,
    endpoint: Mutex<Option<Endpoint>>,
    error: Mutex<Option<String>>,
    transfers: Mutex<VecDeque<Value>>,
    offers: Mutex<HashMap<String, oneshot::Sender<bool>>>,
    sync_lock: Mutex<()>,
    place_lock: Mutex<()>,
    rt: tokio::runtime::Runtime,
}

static PEER: OnceLock<PeerManager> = OnceLock::new();

fn manager() -> &'static PeerManager {
    PEER.get_or_init(|| {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .thread_name("swiftlane-peer")
            .build()
            .expect("peer tokio runtime");
        PeerManager {
            contacts: Arc::new(Contacts::load(config::config_dir().join("contacts.json"))),
            endpoint: Mutex::new(None),
            error: Mutex::new(None),
            transfers: Mutex::new(VecDeque::with_capacity(MAX_LOG)),
            offers: Mutex::new(HashMap::new()),
            sync_lock: Mutex::new(()),
            place_lock: Mutex::new(()),
            rt,
        }
    })
}

fn peer_key_path() -> PathBuf {
    config::config_dir().join("contact-key.dpapi")
}

fn my_contact_name(app: &AppHandle) -> String {
    let configured = app
        .state::<AppState>()
        .cfg
        .lock()
        .unwrap()
        .get("contact_name")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let fallback = std::env::var("USERNAME")
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_default();
    let picked = if !configured.is_empty() {
        clean_name(&configured)
    } else {
        clean_name(&fallback)
    };
    if picked.is_empty() {
        "Swiftlane".into()
    } else {
        picked
    }
}

fn record_progress(app: &AppHandle, info: &Value) {
    let m = manager();
    {
        let mut log = m.transfers.lock().unwrap();
        let id = info["id"].as_str().unwrap_or("");
        if let Some(slot) = log.iter_mut().find(|t| t["id"].as_str() == Some(id)) {
            *slot = info.clone();
        } else {
            if log.len() >= MAX_LOG {
                log.pop_back();
            }
            log.push_front(info.clone());
        }
    }
    if matches!(
        info["status"].as_str(),
        Some("done" | "declined" | "failed")
    ) {
        if let Some(id) = info["id"].as_str() {
            m.offers.lock().unwrap().remove(id);
        }
    }
    push(app, "peer-transfer", info.clone());
}

fn place_received(app: &AppHandle, tmp: &Path, name: &str) -> Result<PathBuf, String> {
    let (save_dir, sort_files) = {
        let st = app.state::<AppState>();
        let cfg = st.cfg.lock().unwrap();
        (
            PathBuf::from(cfg["save_dir"].as_str().unwrap_or("")),
            cfg["sort_files"].as_bool().unwrap_or(true),
        )
    };
    let folder = if sort_files {
        save_dir.join(category_for(name))
    } else {
        save_dir
    };
    let _guard = manager().place_lock.lock().unwrap();
    place_file(tmp, &folder, name).map_err(|e| format!("Couldn't save {name}: {e}"))
}

async fn handle_incoming(app: AppHandle, incoming: iroh::endpoint::Incoming) {
    let Ok(accepting) = incoming.accept() else {
        return;
    };
    let Ok(conn) = accepting.await else {
        return;
    };
    let sender = conn.remote_id().to_string();
    let m = manager();
    let Some(contact) = m.contacts.get(&sender) else {
        // Not one of your contacts: say nothing, take nothing.
        conn.close(1u32.into(), b"");
        return;
    };
    let contact_name = contact["name"].as_str().unwrap_or("Contact").to_string();
    let tid = random_hex(6);
    let mut info = json!({
        "id": tid,
        "dir": "in",
        "contact": contact_name,
        "contact_id": sender,
        "status": "asking",
        "files": [],
        "total": 0,
        "done": 0,
        "error": Value::Null,
    });
    let mut tmp_paths: Vec<PathBuf> = Vec::new();
    let mut registered = false;

    let res: Result<(), String> = async {
        let (mut w, mut r) = tokio::time::timeout(Duration::from_secs(30), conn.accept_bi())
            .await
            .map_err(|_| "Timed out waiting for offer.".to_string())?
            .map_err(|_| "The connection was lost.".to_string())?;

        let offer = tokio::time::timeout(Duration::from_secs(30), read_frame(&mut r))
            .await
            .map_err(|_| "Timed out reading offer.".to_string())??;

        let raw_files = offer
            .get("files")
            .and_then(Value::as_array)
            .ok_or_else(|| "bad offer".to_string())?;
        if offer.get("t").and_then(Value::as_str) != Some("offer")
            || raw_files.is_empty()
            || raw_files.len() > MAX_FILES
        {
            return Err("bad offer".into());
        }

        let mut files: Vec<(String, u64)> = Vec::with_capacity(raw_files.len());
        let mut total: u64 = 0;
        for f in raw_files {
            let name = safe_name(f.get("name").and_then(Value::as_str).unwrap_or(""));
            let size = f
                .get("size")
                .and_then(Value::as_u64)
                .ok_or_else(|| "bad offer".to_string())?;
            total = total.checked_add(size).ok_or_else(|| "bad offer".to_string())?;
            files.push((name, size));
        }

        let json_files: Vec<Value> = files
            .iter()
            .map(|(n, s)| json!({ "name": n, "size": s }))
            .collect();
        info["files"] = json!(json_files);
        info["total"] = json!(total);
        info["note"] = json!(clean_name(
            offer.get("from_name").and_then(Value::as_str).unwrap_or("")
        ));
        registered = true;
        m.contacts.seen(&sender);

        let folder = super::save_dir(&app);
        let _ = std::fs::create_dir_all(&folder);
        let free = fs4::available_space(&folder).unwrap_or(0);
        if free < total.saturating_add(HEADROOM) {
            let _ = send_frame(
                &mut w,
                &json!({ "t": "answer", "accept": false, "reason": "Not enough free space on their PC." }),
            )
            .await;
            info["status"] = json!("declined");
            info["error"] = json!("Not enough free space on this PC.");
            return Ok(());
        }

        let (tx, rx) = oneshot::channel::<bool>();
        m.offers.lock().unwrap().insert(tid.clone(), tx);
        record_progress(&app, &info);

        let n = files.len();
        config::log(&format!(
            "Send to contacts: {contact_name} offers {n} file(s), {total} bytes"
        ));
        crate::notify::show(
            &app,
            &format!(
                "{contact_name} wants to send you {n} file{}",
                if n == 1 { "" } else { "s" }
            ),
            "Open Swiftlane to accept or decline.",
        );
        push(&app, "peer-offer", info.clone());

        let decision = tokio::time::timeout(Duration::from_secs(ASK_SECONDS), rx).await;
        m.offers.lock().unwrap().remove(&tid);

        match decision {
            Ok(Ok(true)) => {}
            Ok(Ok(false)) => {
                let _ = send_frame(
                    &mut w,
                    &json!({ "t": "answer", "accept": false, "reason": "They declined." }),
                )
                .await;
                info["status"] = json!("declined");
                return Ok(());
            }
            _ => {
                let _ = send_frame(
                    &mut w,
                    &json!({ "t": "answer", "accept": false, "reason": "No answer in time." }),
                )
                .await;
                info["status"] = json!("declined");
                return Ok(());
            }
        }

        send_frame(&mut w, &json!({ "t": "answer", "accept": true })).await?;
        info["status"] = json!("receiving");
        record_progress(&app, &info);

        let mut saved: Vec<String> = Vec::with_capacity(files.len());
        let mut done_total: u64 = 0;
        let mut buf = vec![0u8; CHUNK];

        for (idx, (name, size)) in files.iter().enumerate() {
            let tmp = folder.join(format!(".peer-{tid}-{idx}.part"));
            tmp_paths.push(tmp.clone());
            let mut out = tokio::fs::File::create(&tmp)
                .await
                .map_err(|e| format!("Couldn't create temp file: {e}"))?;
            let mut hasher = Sha256::new();
            let mut got: u64 = 0;
            let mut last_push = Instant::now();

            while got < *size {
                let want = (*size - got).min(buf.len() as u64) as usize;
                let n = r
                    .read(&mut buf[..want])
                    .await
                    .map_err(|_| "The connection was lost.".to_string())?
                    .ok_or_else(|| "The sender stopped.".to_string())?;
                if n == 0 {
                    return Err("The sender stopped.".into());
                }
                out.write_all(&buf[..n])
                    .await
                    .map_err(|e| format!("Couldn't write file: {e}"))?;
                hasher.update(&buf[..n]);
                got += n as u64;
                done_total += n as u64;
                info["done"] = json!(done_total);
                if last_push.elapsed() > Duration::from_millis(250) {
                    last_push = Instant::now();
                    record_progress(&app, &info);
                }
            }
            out.flush()
                .await
                .map_err(|e| format!("Couldn't flush file: {e}"))?;
            drop(out);

            let end = read_frame(&mut r).await?;
            let expected_hash = hex::encode(hasher.finalize());
            if end.get("t").and_then(Value::as_str) != Some("file")
                || end.get("sha256").and_then(Value::as_str) != Some(expected_hash.as_str())
            {
                return Err(format!("{name} arrived damaged, so it was thrown away."));
            }

            let final_path = place_received(&app, &tmp, name)?;
            tmp_paths.retain(|p| p != &tmp);
            mark_from_internet(&final_path);
            record_created(&final_path);
            let final_name = final_path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| name.clone());
            saved.push(final_name.clone());
            push(
                &app,
                "received",
                json!({
                    "transfer": Value::Null,
                    "entry": {
                        "id": random_hex(4),
                        "name": final_name,
                        "path": final_path.to_string_lossy(),
                        "size": size,
                        "from": contact_name,
                        "time": now_unix(),
                    }
                }),
            );
        }

        send_frame(&mut w, &json!({ "t": "result", "saved": saved })).await?;
        let _ = w.finish();
        info["status"] = json!("done");
        tokio::time::sleep(Duration::from_millis(300)).await;
        Ok(())
    }
    .await;

    if let Err(err) = res {
        info["status"] = json!("failed");
        info["error"] = json!(err);
    }

    for p in tmp_paths {
        let _ = std::fs::remove_file(p);
    }
    if registered {
        record_progress(&app, &info);
    }
    conn.close(0u32.into(), b"");
}

fn is_running() -> bool {
    manager()
        .endpoint
        .lock()
        .unwrap()
        .as_ref()
        .is_some_and(|ep| !ep.is_closed())
}

fn peer_wanted(app: &AppHandle) -> bool {
    let on = app
        .state::<AppState>()
        .cfg
        .lock()
        .unwrap()["features"]["contacts"]
        .as_bool()
        .unwrap_or(true);
    on && !manager().contacts.is_empty()
}

/// Starts the iroh endpoint when contacts feature is on + at least one contact exists; stops it otherwise.
pub fn sync(app: &AppHandle) {
    let m = manager();
    let _guard = m.sync_lock.lock().unwrap();
    let wanted = peer_wanted(app);
    let running = is_running();

    if wanted && !running {
        match load_or_create_secret_key(&peer_key_path()) {
            Ok(key_bytes) => {
                let sk = SecretKey::from_bytes(&key_bytes);
                let bound = m.rt.block_on(async {
                    let ep = Endpoint::builder(presets::N0)
                        .secret_key(sk)
                        .alpns(vec![ALPN.to_vec()])
                        .bind()
                        .await
                        .map_err(|e| format!("Couldn't start sending to contacts ({e})."))?;
                    let _ = tokio::time::timeout(Duration::from_secs(20), ep.online()).await;
                    Ok::<Endpoint, String>(ep)
                });
                match bound {
                    Ok(ep) => {
                        *m.endpoint.lock().unwrap() = Some(ep.clone());
                        *m.error.lock().unwrap() = None;
                        config::log("Send to contacts: ready");
                        let app_accept = app.clone();
                        m.rt.spawn(async move {
                            while let Some(incoming) = ep.accept().await {
                                let app_c = app_accept.clone();
                                tokio::spawn(handle_incoming(app_c, incoming));
                            }
                        });
                    }
                    Err(e) => {
                        *m.error.lock().unwrap() = Some(e.clone());
                        config::log(&format!("Send to contacts: {e}"));
                    }
                }
            }
            Err(e) => {
                *m.error.lock().unwrap() = Some(e.clone());
                config::log(&format!("Send to contacts: {e}"));
            }
        }
    } else if !wanted && running {
        if let Some(ep) = m.endpoint.lock().unwrap().take() {
            let _ = m.rt.block_on(tokio::time::timeout(Duration::from_secs(3), ep.close()));
            config::log("Send to contacts: stopped (no contacts, or switched off)");
        }
    }

    push(app, "contacts", contacts_state(app));
}

pub fn shutdown() {
    let Some(m) = PEER.get() else { return };
    if let Some(ep) = m.endpoint.lock().unwrap().take() {
        let _ = m.rt.block_on(tokio::time::timeout(Duration::from_secs(3), ep.close()));
    }
}

fn my_endpoint_id() -> Result<String, String> {
    if let Some(ep) = manager().endpoint.lock().unwrap().as_ref() {
        return Ok(ep.id().to_string());
    }
    let key = load_or_create_secret_key(&peer_key_path())?;
    Ok(SecretKey::from_bytes(&key).public().to_string())
}

fn contacts_state(app: &AppHandle) -> Value {
    let m = manager();
    let name = my_contact_name(app);
    let (code, safety) = match my_endpoint_id() {
        Ok(id) => (Some(make_code(&id, &name)), Some(safety_words(&id))),
        Err(e) => {
            let mut err = m.error.lock().unwrap();
            if err.is_none() {
                *err = Some(e);
            }
            (None, None)
        }
    };
    let on = app
        .state::<AppState>()
        .cfg
        .lock()
        .unwrap()["features"]["contacts"]
        .as_bool()
        .unwrap_or(true);
    let transfers: Vec<Value> = m.transfers.lock().unwrap().iter().cloned().collect();
    let offer_ids: Vec<String> = m.offers.lock().unwrap().keys().cloned().collect();
    let offers: Vec<Value> = transfers
        .iter()
        .filter(|t| {
            t["status"].as_str() == Some("asking")
                && t["id"]
                    .as_str()
                    .is_some_and(|id| offer_ids.iter().any(|k| k == id))
        })
        .cloned()
        .collect();

    json!({
        "code": code,
        "safety": safety,
        "name": name,
        "contacts": m.contacts.list(),
        "running": is_running(),
        "error": *m.error.lock().unwrap(),
        "on": on,
        "transfers": transfers,
        "offers": offers,
    })
}

fn send_to_contact(
    app: &AppHandle,
    contact_id: &str,
    paths: &[PathBuf],
    cancelled: &(dyn Fn() -> bool + Send + Sync),
    report_job: &(dyn Fn(Option<f64>, Option<&str>) + Send + Sync),
) -> Result<Vec<String>, JobError> {
    let m = manager();
    let ep = m
        .endpoint
        .lock()
        .unwrap()
        .clone()
        .filter(|e| !e.is_closed())
        .ok_or_else(|| JobError::Failed("Sending to contacts isn't running.".into()))?;
    let contact = m
        .contacts
        .get(contact_id)
        .ok_or_else(|| JobError::Failed("That person isn't in your contacts.".into()))?;
    let contact_name = contact["name"].as_str().unwrap_or("Contact").to_string();

    let files: Vec<(PathBuf, String, u64)> = paths
        .iter()
        .filter(|p| p.is_file())
        .filter_map(|p| {
            let meta = std::fs::metadata(p).ok()?;
            let name = p.file_name()?.to_string_lossy().to_string();
            Some((p.clone(), name, meta.len()))
        })
        .collect();
    if files.is_empty() || files.len() > MAX_FILES {
        return Err(JobError::Failed("Pick between 1 and 1000 files.".into()));
    }

    let total: u64 = files.iter().map(|(_, _, s)| *s).sum();
    let tid = random_hex(6);
    let mut info = json!({
        "id": tid,
        "dir": "out",
        "contact": contact_name,
        "contact_id": contact_id,
        "status": "connecting",
        "files": files.iter().map(|(_, n, s)| json!({ "name": n, "size": s })).collect::<Vec<_>>(),
        "total": total,
        "done": 0,
        "error": Value::Null,
    });
    record_progress(app, &info);
    report_job(None, Some("Connecting…"));

    let my_name = my_contact_name(app);
    let peer_id = EndpointId::from_str(contact_id)
        .map_err(|_| JobError::Failed("Invalid contact code.".into()))?;

    let result = m.rt.block_on(async {
        let conn = tokio::time::timeout(Duration::from_secs(25), ep.connect(peer_id, ALPN))
            .await
            .map_err(|_| {
                format!("Couldn't reach {contact_name}. Their PC needs to be on with Swiftlane open.")
            })?
            .map_err(|_| {
                format!("Couldn't reach {contact_name}. Their PC needs to be on with Swiftlane open.")
            })?;

        if conn.remote_id().to_string() != contact_id {
            conn.close(1u32.into(), b"");
            return Err("Connected to the wrong PC.".into());
        }
        m.contacts.seen(contact_id);

        let (mut w, mut r) = conn
            .open_bi()
            .await
            .map_err(|_| format!("The connection to {contact_name} was lost."))?;

        send_frame(
            &mut w,
            &json!({
                "t": "offer",
                "v": 1,
                "from_name": my_name,
                "files": info["files"],
            }),
        )
        .await?;

        info["status"] = json!("waiting");
        record_progress(app, &info);
        report_job(None, Some("Waiting for them to accept…"));

        let answer =
            tokio::time::timeout(Duration::from_secs(ASK_SECONDS + 15), read_frame(&mut r))
                .await
                .map_err(|_| "No answer in time.".to_string())??;
        if !answer.get("accept").and_then(Value::as_bool).unwrap_or(false) {
            info["status"] = json!("declined");
            let reason = answer
                .get("reason")
                .and_then(Value::as_str)
                .map(String::from)
                .unwrap_or_else(|| format!("{contact_name} declined."));
            conn.close(0u32.into(), b"");
            return Err(reason);
        }

        info["status"] = json!("sending");
        record_progress(app, &info);

        let mut done_total: u64 = 0;
        let mut last_push = Instant::now();
        let mut buf = vec![0u8; CHUNK];

        for (path, _, expected_size) in &files {
            let mut f = tokio::fs::File::open(path)
                .await
                .map_err(|e| format!("Couldn't read {}: {e}", path.display()))?;
            let mut hasher = Sha256::new();
            let mut sent: u64 = 0;
            while sent < *expected_size {
                if cancelled() {
                    conn.close(0u32.into(), b"");
                    return Err("Cancelled.".into());
                }
                use tokio::io::AsyncReadExt;
                let want = (*expected_size - sent).min(buf.len() as u64) as usize;
                let n = f
                    .read(&mut buf[..want])
                    .await
                    .map_err(|e| format!("Couldn't read file: {e}"))?;
                if n == 0 {
                    return Err("A file changed while sending.".into());
                }
                w.write_all(&buf[..n])
                    .await
                    .map_err(|_| format!("The connection to {contact_name} was lost."))?;
                hasher.update(&buf[..n]);
                sent += n as u64;
                done_total += n as u64;
                info["done"] = json!(done_total);
                if last_push.elapsed() > Duration::from_millis(250) {
                    last_push = Instant::now();
                    record_progress(app, &info);
                    if total > 0 {
                        report_job(Some((done_total as f64 / total as f64).min(1.0)), None);
                    }
                }
            }
            send_frame(
                &mut w,
                &json!({ "t": "file", "sha256": hex::encode(hasher.finalize()) }),
            )
            .await?;
        }

        let _ = w.finish();
        let reply = tokio::time::timeout(Duration::from_secs(60), read_frame(&mut r))
            .await
            .map_err(|_| format!("The connection to {contact_name} was lost."))??;
        let saved: Vec<String> = reply
            .get("saved")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        info["status"] = json!("done");
        conn.close(0u32.into(), b"");
        Ok::<Vec<String>, String>(saved)
    });

    match result {
        Ok(saved) => {
            record_progress(app, &info);
            Ok(saved)
        }
        Err(err) => {
            if info["status"].as_str() != Some("declined") {
                info["status"] = json!("failed");
            }
            info["error"] = json!(err);
            record_progress(app, &info);
            if err == "Cancelled." {
                Err(JobError::Cancelled)
            } else {
                Err(JobError::Failed(err))
            }
        }
    }
}

pub fn dispatch(app: &AppHandle, method: &str, args: &[Value]) -> Option<Value> {
    Some(match method {
        "contacts_state" => contacts_state(app),
        "contact_add" => {
            let code = arg(args, 0).as_str().unwrap_or("");
            let name = arg(args, 1).as_str().filter(|s| !s.is_empty());
            let own = my_endpoint_id().ok();
            match manager().contacts.add(code, name, own.as_deref()) {
                Ok(c) => {
                    config::log(&format!(
                        "Send to contacts: added {} ({})",
                        c["name"].as_str().unwrap_or(""),
                        safety_words(c["id"].as_str().unwrap_or(""))
                    ));
                    let app_sync = app.clone();
                    std::thread::spawn(move || sync(&app_sync));
                    contacts_state(app)
                }
                Err(e) => json!({ "error": e }),
            }
        }
        "contact_remove" => {
            let cid = arg(args, 0).as_str().unwrap_or("");
            manager().contacts.remove(cid);
            let app_sync = app.clone();
            std::thread::spawn(move || sync(&app_sync));
            contacts_state(app)
        }
        "contact_rename" => {
            let cid = arg(args, 0).as_str().unwrap_or("");
            let name = arg(args, 1).as_str().unwrap_or("");
            manager().contacts.rename(cid, name);
            contacts_state(app)
        }
        "set_contact_name" => {
            let name = clean_name(arg(args, 0).as_str().unwrap_or(""));
            {
                let st = app.state::<AppState>();
                let mut cfg = st.cfg.lock().unwrap();
                cfg["contact_name"] = json!(name);
                config::save(&cfg);
            }
            contacts_state(app)
        }
        "peer_answer" => {
            let offer_id = arg(args, 0).as_str().unwrap_or("");
            let accept = arg(args, 1).as_bool().unwrap_or(false);
            let tx = manager().offers.lock().unwrap().remove(offer_id);
            match tx {
                Some(sender) => {
                    let _ = sender.send(accept);
                    json!(true)
                }
                None => json!({ "error": "That request has already ended." }),
            }
        }
        "peer_send" => {
            let contact_ids: Vec<String> = arg(args, 0)
                .as_array()
                .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                .unwrap_or_default();
            let paths: Vec<PathBuf> = arg(args, 1)
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(PathBuf::from))
                        .filter(|p| p.is_file())
                        .collect()
                })
                .unwrap_or_default();
            if paths.is_empty() {
                return Some(json!({
                    "error": "Add some files first (folders can't be sent to contacts yet)."
                }));
            }
            if !is_running() {
                sync(app);
                if !is_running() {
                    let err = manager()
                        .error
                        .lock()
                        .unwrap()
                        .clone()
                        .unwrap_or_else(|| "Sending to contacts isn't running.".into());
                    return Some(json!({ "error": err }));
                }
            }
            let mut job_ids = Vec::new();
            for cid in contact_ids {
                let Some(c) = manager().contacts.get(&cid) else {
                    continue;
                };
                let cname = c["name"].as_str().unwrap_or("Contact").to_string();
                let n = paths.len();
                let title = format!("{n} file{} to {cname}", if n == 1 { "" } else { "s" });
                let paths_clone = paths.clone();
                let id = app
                    .state::<AppState>()
                    .jobs
                    .start(app, "peer", &title, false, move |ctx| {
                        send_to_contact(
                            ctx.app(),
                            &cid,
                            &paths_clone,
                            &|| ctx.cancelled(),
                            &|f, t| ctx.report(f, t),
                        )?;
                        Ok(Vec::new())
                    });
                job_ids.push(id);
            }
            if job_ids.is_empty() {
                json!({ "error": "Pick who to send to." })
            } else {
                json!({ "jobs": job_ids })
            }
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
        let dir = std::env::temp_dir().join(format!("swl-peers-test-{}", hex::encode(b)));
        std::fs::create_dir(&dir).unwrap();
        dir
    }

    #[test]
    fn contact_codes_and_safety_words_match_python() {
        let key = [42u8; 32];
        let id = SecretKey::from_bytes(&key).public().to_string();
        assert_eq!(id.len(), 64);
        assert!(id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit()));

        let code = make_code(&id, "Alice <PC>\n");
        let (parsed_id, parsed_name) = parse_code(&format!("  {code}  ")).unwrap();
        assert_eq!(parsed_id, id);
        assert_eq!(parsed_name, "Alice PC");

        let words = safety_words(&id);
        assert_eq!(words.len(), 19); // XXXX-XXXX-XXXX-XXXX
        assert_eq!(words.chars().filter(|&c| c == '-').count(), 3);

        assert!(parse_code("not-a-code").is_err());
        assert!(parse_code("SL2.abc.def").is_err());
        assert!(parse_code("SL1.short.abc").is_err());
    }

    #[test]
    fn contacts_crud_and_self_rejection() {
        let dir = fresh_dir();
        let store = Contacts::load(dir.join("contacts.json"));
        let own_id = SecretKey::from_bytes(&[1u8; 32]).public().to_string();
        let peer_id = SecretKey::from_bytes(&[2u8; 32]).public().to_string();

        assert!(store.add(&make_code(&own_id, "Me"), None, Some(&own_id)).is_err());

        let added = store.add(&make_code(&peer_id, "Bob"), None, Some(&own_id)).unwrap();
        assert_eq!(added["name"], "Bob");
        assert!(store.add(&make_code(&peer_id, "Bob"), None, Some(&own_id)).is_err());

        store.rename(&peer_id, "Bobby");
        store.seen(&peer_id);
        let list = store.list();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0]["name"], "Bobby");
        assert!(list[0]["last"].as_i64().is_some());
        assert_eq!(list[0]["safety"], safety_words(&peer_id));

        store.remove(&peer_id);
        assert!(store.is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
