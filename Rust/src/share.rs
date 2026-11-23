//! Share links: the list of shares, their links and who has downloaded. Same rules and the same
//! shares.json file as the Python version (share.py), so existing links keep working.
//! - A link is a random 128-bit token. Files are picked by their number in a list fixed when the
//!   share was made; URLs never contain file paths. Folders are listed once, without following
//!   shortcuts/links/junctions and without hidden or system files.
//! - Every share has an end time; links can have a download limit, be personal (one person each),
//!   be blocked, and the share can have a password (scrypt, with lockouts).

use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::config;
use crate::transfer::human;

pub const SHARE_PORT: u16 = 8766;
pub const MAX_HOURS: f64 = 24.0 * 365.0;
pub const MAX_LINKS: usize = 100;
const OPEN_SESSIONS_PER_LINK: usize = 300; // uncounted page visits remembered per link (link previews, bots)
pub const RESUME_SECONDS: f64 = 15.0 * 60.0; // a used-up link stays reachable this long after its last download

pub fn now() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

fn token_hex(n: usize) -> String {
    let mut b = vec![0u8; n];
    getrandom::getrandom(&mut b).expect("Windows' random number source failed");
    hex::encode(b)
}

fn token_urlsafe(n: usize) -> String {
    use base64::Engine;
    let mut b = vec![0u8; n];
    getrandom::getrandom(&mut b).expect("Windows' random number source failed");
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b)
}

// ------------------------------------------------------------------ what may be shared

const HIDDEN: u32 = 0x2;
const SYSTEM: u32 = 0x4;
const REPARSE: u32 = 0x400;

fn attrs(meta: &std::fs::Metadata) -> u32 {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        meta.file_attributes()
    }
    #[cfg(not(windows))]
    {
        let _ = meta;
        0
    }
}

/// Symlinks, junctions and other reparse points (also cloud placeholders): never followed.
pub fn is_link(meta: &std::fs::Metadata) -> bool {
    meta.file_type().is_symlink() || attrs(meta) & REPARSE != 0
}

fn norm(p: &Path) -> String {
    let s = p.to_string_lossy().to_string();
    if cfg!(windows) { s.to_lowercase().replace('/', "\\") } else { s }
}

fn inside(path: &Path, folder: &Path) -> bool {
    let (p, f) = (norm(path), norm(folder));
    let sep = std::path::MAIN_SEPARATOR;
    p == f || p.starts_with(&format!("{}{sep}", f.trim_end_matches(['\\', '/'])))
}

fn known_folders() -> Vec<String> {
    let mut out = Vec::new();
    if let Some(home) = dirs::home_dir() {
        out.push(norm(&home));
        for n in ["Desktop", "Documents", "Downloads", "Pictures", "Videos", "Music", "OneDrive"] {
            out.push(norm(&home.join(n)));
        }
    }
    for d in [dirs::document_dir(), dirs::desktop_dir(), dirs::download_dir(), dirs::picture_dir(), dirs::video_dir(), dirs::audio_dir()].into_iter().flatten() {
        out.push(norm(&d));
    }
    out
}

/// Places that are never shared: Windows, programs, app data (incl. Swiftlane's own settings).
fn forbidden_folders() -> Vec<PathBuf> {
    let env = |k: &str, d: &str| std::env::var_os(k).map(PathBuf::from).unwrap_or_else(|| PathBuf::from(d));
    let mut v = vec![
        env("SystemRoot", r"C:\Windows"),
        env("ProgramFiles", r"C:\Program Files"),
        env("ProgramFiles(x86)", r"C:\Program Files (x86)"),
        env("ProgramData", r"C:\ProgramData"),
        config::config_dir(),
    ];
    if let Some(home) = dirs::home_dir() {
        v.push(home.join("AppData"));
    }
    v
}

/// File names that look like keys, passwords or wallets (asks before sharing them).
pub fn looks_private(name: &str) -> bool {
    let n = name.to_lowercase();
    let id_key = n.strip_prefix("id_").is_some_and(|r| matches!(r, "rsa" | "ed25519" | "ecdsa" | "dsa"));
    let ext = [".pem", ".key", ".pfx", ".p12", ".kdbx", ".ovpn"].iter().any(|e| n.ends_with(e));
    id_key || ext || n == ".env" || n == "wallet.dat" || matches!(n.as_str(), "login data" | "cookies" | "web data") || n.contains("password")
}

pub struct Collected {
    pub items: Vec<Value>,
    pub total: u64,
    pub skipped: Value,
    pub warnings: Vec<String>,
}

/// The fixed list of files a share will offer, plus what was left out and why.
pub fn collect(paths: &[PathBuf]) -> Result<Collected, String> {
    let (mut items, mut warnings, mut sensitive) = (Vec::new(), Vec::new(), Vec::new());
    let (mut hidden, mut links, mut unreadable) = (0u64, 0u64, 0u64);
    let (known, forbidden) = (known_folders(), forbidden_folders());
    let mut used = std::collections::HashSet::new();
    let mut unique = |name: &str| {
        let (stem, ext) = match name.rfind('.') {
            Some(i) if i > 0 => (&name[..i], &name[i..]),
            _ => (name, ""),
        };
        let (mut n, mut candidate) = (1, name.to_string());
        while used.contains(&candidate.to_lowercase()) {
            n += 1;
            candidate = format!("{stem} ({n}){ext}");
        }
        used.insert(candidate.to_lowercase());
        candidate
    };
    for raw in paths {
        let path = std::path::absolute(raw).unwrap_or_else(|_| raw.clone());
        let base = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let meta = std::fs::symlink_metadata(&path).map_err(|_| format!("Can't find {}", path.display()))?;
        if is_link(&meta) {
            return Err(format!("“{base}” is a shortcut or link to somewhere else. Share the real file or folder instead."));
        }
        if forbidden.iter().any(|f| inside(&path, f)) {
            return Err(format!("Swiftlane never shares system or program folders ({}).", path.display()));
        }
        if meta.is_file() {
            let name = unique(&base);
            if looks_private(&base) {
                sensitive.push(name.clone());
            }
            items.push(json!({ "name": name, "path": path.to_string_lossy(), "size": meta.len() }));
            continue;
        }
        if !meta.is_dir() {
            return Err(format!("Can't share {}", path.display()));
        }
        if known.contains(&norm(&path)) || path.parent().is_none() || path.components().count() <= 1 {
            warnings.push(format!("“{}” is a whole personal folder or drive. Everything in it will be downloadable.", path.display()));
        }
        let drive: String = path.to_string_lossy().chars().take(1).collect();
        let root = unique(if base.is_empty() { &drive } else { &base });
        let mut stack = vec![(path.clone(), root)];
        while let Some((folder, rel)) = stack.pop() {
            let Ok(rd) = std::fs::read_dir(&folder) else {
                unreadable += 1;
                continue;
            };
            let mut entries: Vec<_> = rd.flatten().collect();
            entries.sort_by_key(|e| e.file_name().to_string_lossy().to_lowercase());
            for e in entries {
                let Ok(m) = std::fs::symlink_metadata(e.path()) else {
                    unreadable += 1;
                    continue;
                };
                let name = e.file_name().to_string_lossy().to_string();
                if is_link(&m) {
                    links += 1;
                } else if name.starts_with('.') || attrs(&m) & (HIDDEN | SYSTEM) != 0 {
                    hidden += 1;
                } else if m.is_dir() {
                    stack.push((e.path(), format!("{rel}/{name}")));
                } else if m.is_file() {
                    if looks_private(&name) {
                        sensitive.push(format!("{rel}/{name}"));
                    }
                    items.push(json!({ "name": format!("{rel}/{name}"), "path": e.path().to_string_lossy(), "size": m.len() }));
                }
            }
        }
    }
    if items.is_empty() {
        return Err("There's nothing to share: no files found (hidden files and links are left out).".into());
    }
    if !sensitive.is_empty() {
        let mut shown = sensitive.iter().take(4).cloned().collect::<Vec<_>>().join(", ");
        if sensitive.len() > 4 {
            shown.push_str(&format!(" and {} more", sensitive.len() - 4));
        }
        warnings.push(format!("Some files look private (keys, passwords or wallets): {shown}."));
    }
    let total = items.iter().map(|i| i["size"].as_u64().unwrap_or(0)).sum();
    Ok(Collected { items, total, skipped: json!({ "hidden": hidden, "links": links, "unreadable": unreadable }), warnings })
}

// ------------------------------------------------------------------ passwords (scrypt, same as Python)

pub fn hash_password(password: &str) -> Value {
    let mut salt = [0u8; 16];
    getrandom::getrandom(&mut salt).expect("Windows' random number source failed");
    json!({ "salt": hex::encode(salt), "hash": hex::encode(scrypt_hash(password, &salt)) })
}

fn scrypt_hash(password: &str, salt: &[u8]) -> [u8; 32] {
    let params = scrypt::Params::new(14, 8, 1, 32).expect("valid scrypt settings");
    let mut out = [0u8; 32];
    scrypt::scrypt(password.as_bytes(), salt, &params, &mut out).expect("scrypt output length");
    out
}

pub fn check_password(password: &str, stored: &Value) -> bool {
    let (Some(salt), Some(hash)) = (stored["salt"].as_str().and_then(|s| hex::decode(s).ok()), stored["hash"].as_str()) else {
        return false;
    };
    let candidate = hex::encode(scrypt_hash(password, &salt));
    constant_time_eq::constant_time_eq(candidate.as_bytes(), hash.as_bytes())
}

// ------------------------------------------------------------------ the list of shares

fn new_link(label: &str, limit: Option<u64>, counts: &str) -> Value {
    json!({ "id": token_hex(4), "token": token_urlsafe(16), "label": label, "max": limit, "uses": 0,
            "blocked": false, "last": null, "counts": counts })
}

#[derive(Default)]
pub struct Inner {
    pub shares: Map<String, Value>,
    pub open_sessions: HashMap<String, Value>, // visits that haven't downloaded yet
    pub pending: HashMap<String, (u64, u64)>,  // request share id -> (files, bytes) being uploaded now
}

pub struct Shares {
    pub inner: Mutex<Inner>,
    path: PathBuf,
}

pub fn is_active(s: &Value) -> bool {
    s["ended"].is_null() && s["expires"].as_f64().unwrap_or(0.0) > now()
}

/// None if one more file of `size` bytes fits a request's limits, else why not.
fn room(inner: &Inner, s: &Value, size: u64) -> Option<String> {
    let (files, bytes) = inner.pending.get(s["id"].as_str().unwrap_or("")).copied().unwrap_or((0, 0));
    let have = s["items"].as_array().map(|a| a.len() as u64).unwrap_or(0);
    if have + files >= s["max_files"].as_u64().unwrap_or(0) {
        return Some("This request already has all the files it accepts.".into());
    }
    if s["total"].as_u64().unwrap_or(0) + bytes + size > s["max_bytes"].as_u64().unwrap_or(0) {
        return Some("That's more than this request accepts.".into());
    }
    None
}

pub fn link_open(inner: &Inner, s: &Value, l: &Value) -> bool {
    let base = is_active(s) && !l["blocked"].as_bool().unwrap_or(false);
    if s["kind"] == "request" {
        return base && room(inner, s, 0).is_none();
    }
    base && (l["max"].is_null() || l["uses"].as_u64().unwrap_or(0) < l["max"].as_u64().unwrap_or(0))
}

impl Shares {
    pub fn load() -> Self {
        let path = config::config_dir().join("shares.json");
        let mut inner = Inner::default();
        if let Ok(text) = std::fs::read_to_string(&path) {
            if let Ok(data) = serde_json::from_str::<Value>(&text) {
                for s in data["shares"].as_array().cloned().unwrap_or_default() {
                    if let Some(id) = s["id"].as_str() {
                        inner.shares.insert(id.to_string(), s);
                    }
                }
            }
        }
        let me = Shares { inner: Mutex::new(inner), path };
        me.prune();
        me
    }

    /// Saved through a temporary file, so a crash never leaves half a file.
    pub fn save(&self, inner: &Inner) {
        let data = json!({ "shares": inner.shares.values().cloned().collect::<Vec<_>>() });
        let tmp = self.path.with_extension("tmp");
        if let Some(parent) = self.path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if std::fs::write(&tmp, serde_json::to_string_pretty(&data).unwrap_or_default()).is_ok() {
            let _ = std::fs::rename(&tmp, &self.path);
        }
    }

    /// True while a link could still be used, or a used-up link was downloaded in the last 15
    /// minutes (so an interrupted download can resume). Funnel is on only then.
    pub fn any_active(&self) -> bool {
        let inner = self.inner.lock().unwrap();
        inner.shares.values().any(|s| {
            s["links"].as_array().into_iter().flatten().any(|l| {
                link_open(&inner, s, l)
                    || (is_active(s) && !l["blocked"].as_bool().unwrap_or(false)
                        && l["last"].as_f64().is_some_and(|t| now() - t < RESUME_SECONDS))
            })
        })
    }

    /// Forget shares that ended or expired more than a day ago.
    pub fn prune(&self) -> bool {
        let mut inner = self.inner.lock().unwrap();
        let cutoff = now() - 86400.0;
        let before = inner.shares.len();
        inner.shares.retain(|_, s| s["ended"].as_f64().or(s["expires"].as_f64()).unwrap_or(0.0) >= cutoff);
        let gone = inner.shares.len() != before;
        if gone {
            self.save(&inner);
        }
        gone
    }

    pub fn has_any(&self) -> bool {
        !self.inner.lock().unwrap().shares.is_empty()
    }

    pub fn get(&self, id: &str) -> Option<Value> {
        self.inner.lock().unwrap().shares.get(id).cloned()
    }

    fn edit<T>(&self, id: &str, f: impl FnOnce(&mut Inner, &str) -> Result<T, String>) -> Result<T, String> {
        let mut inner = self.inner.lock().unwrap();
        if !inner.shares.contains_key(id) {
            return Err("That share doesn't exist any more.".into());
        }
        let r = f(&mut inner, id)?;
        self.save(&inner);
        Ok(r)
    }

    // ---- changes from the app

    /// mode "open": one link for anyone, optionally limited to `count` downloads.
    /// mode "personal": one single-use link per person (`count` people, or one per label).
    pub fn create(&self, paths: &[PathBuf], o: &Value, confirmed: bool) -> Result<Value, String> {
        let hours = o["hours"].as_f64().unwrap_or(24.0);
        if !(hours > 0.0 && hours <= MAX_HOURS) {
            return Err("Choose a time between a few minutes and one year.".into());
        }
        let found = collect(paths)?;
        if !found.warnings.is_empty() && !confirmed {
            return Ok(json!({ "confirm": found.warnings, "files": found.items.len(), "total": found.total }));
        }
        let labels: Vec<String> = o["labels"].as_array().map(|a| a.iter().filter_map(|x| x.as_str())
            .map(|x| x.trim().chars().take(60).collect::<String>()).filter(|x| !x.is_empty()).collect()).unwrap_or_default();
        let count = o["count"].as_u64().or_else(|| o["count"].as_str().and_then(|s| s.parse().ok())).filter(|c| *c > 0);
        let links: Vec<Value> = if o["mode"] == "personal" {
            let n = if labels.is_empty() { count.unwrap_or(1) as usize } else { labels.len() };
            if !(1..=MAX_LINKS).contains(&n) {
                return Err(format!("Personal links: between 1 and {MAX_LINKS} people."));
            }
            (0..n).map(|i| new_link(labels.get(i).map(String::as_str).unwrap_or(&format!("Person {}", i + 1)), Some(1), "visits")).collect()
        } else {
            vec![new_link("Anyone with the link", count, "files")]
        };
        let mut roots: Vec<String> = Vec::new();
        for p in paths {
            let name = p.to_string_lossy().trim_end_matches(['\\', '/']).rsplit(['\\', '/']).next().unwrap_or("").to_string();
            if !roots.contains(&name) {
                roots.push(name);
            }
        }
        let title = if roots.len() == 1 { roots[0].clone() } else { format!("{} + {} more", roots[0], roots.len() - 1) };
        let password = o["password"].as_str().filter(|p| !p.is_empty()).map(hash_password);
        let id = token_hex(6);
        let share = json!({
            "id": id, "title": title, "created": now(), "expires": now() + hours * 3600.0, "ended": null,
            "items": found.items, "total": found.total, "skipped": found.skipped, "password": password,
            "card_picture": o["card_picture"].as_bool().unwrap_or(true), "links": links, "sessions": {},
        });
        let n_links = share["links"].as_array().map(Vec::len).unwrap_or(0);
        let mut inner = self.inner.lock().unwrap();
        inner.shares.insert(id.clone(), share);
        self.save(&inner);
        config::log(&format!("Share made: {title} ({} file(s), {}), {n_links} link(s)", found.items.len(), human(found.total as f64)));
        Ok(json!({ "share": id }))
    }

    pub fn create_request(&self, folder: &Path, title: &str, o: &Value) -> Result<Value, String> {
        let hours = o["hours"].as_f64().unwrap_or(24.0);
        let max_mb = o["max_mb"].as_u64().unwrap_or(2048);
        let max_files = o["max_files"].as_u64().unwrap_or(50);
        if !(hours > 0.0 && hours <= MAX_HOURS) {
            return Err("Choose a time between a few minutes and one year.".into());
        }
        if !(1..=1024 * 1024).contains(&max_mb) || !(1..=10000).contains(&max_files) {
            return Err("Check the size and file limits.".into());
        }
        let title: String = title.chars().filter(|c| !c.is_control()).collect::<String>().trim().chars().take(80).collect();
        let title = if title.is_empty() { "Files for me".to_string() } else { title };
        let id = token_hex(6);
        let share = json!({
            "id": id, "kind": "request", "title": title, "created": now(), "expires": now() + hours * 3600.0,
            "ended": null, "items": [], "total": 0, "skipped": {}, "folder": folder.to_string_lossy(),
            "max_bytes": max_mb * 1024 * 1024, "max_files": max_files,
            "password": o["password"].as_str().filter(|p| !p.is_empty()).map(hash_password),
            "links": [new_link("Anyone with the link", None, "uploads")], "sessions": {},
        });
        let mut inner = self.inner.lock().unwrap();
        inner.shares.insert(id.clone(), share);
        self.save(&inner);
        config::log(&format!("Request made: {title} (up to {max_files} files, {max_mb} MB) into {}", folder.display()));
        Ok(json!({ "share": id }))
    }

    pub fn extend(&self, id: &str, hours: f64) -> Result<(), String> {
        if !(hours > 0.0 && hours <= MAX_HOURS) {
            return Err("Choose a time between a few minutes and one year.".into());
        }
        self.edit(id, |inner, id| {
            let s = inner.shares.get_mut(id).unwrap();
            s["expires"] = json!(now() + hours * 3600.0);
            s["ended"] = Value::Null;
            Ok(())
        })
    }

    pub fn end(&self, id: &str) -> Result<(), String> {
        self.edit(id, |inner, id| {
            inner.shares.get_mut(id).unwrap()["ended"] = json!(now());
            inner.open_sessions.retain(|_, v| v["share"] != id);
            config::log(&format!("Share ended: {}", inner.shares[id]["title"].as_str().unwrap_or("")));
            Ok(())
        })
    }

    pub fn end_all(&self) {
        let mut inner = self.inner.lock().unwrap();
        for s in inner.shares.values_mut() {
            if s["ended"].is_null() {
                s["ended"] = json!(now());
            }
        }
        inner.open_sessions.clear();
        self.save(&inner);
        config::log("All shares ended");
    }

    pub fn remove(&self, id: &str) {
        let mut inner = self.inner.lock().unwrap();
        inner.shares.remove(id);
        inner.open_sessions.retain(|_, v| v["share"] != id);
        self.save(&inner);
    }

    pub fn set_password(&self, id: &str, password: &str) -> Result<(), String> {
        self.edit(id, |inner, id| {
            let s = inner.shares.get_mut(id).unwrap();
            s["password"] = if password.is_empty() { Value::Null } else { hash_password(password) };
            if let Some(sessions) = s["sessions"].as_object_mut() {
                for sess in sessions.values_mut() {
                    sess["unlocked"] = json!(false); // everyone has to enter the new one
                }
            }
            inner.open_sessions.retain(|_, v| v["share"] != id);
            Ok(())
        })
    }

    pub fn add_links(&self, id: &str, count: usize, labels: &[String], limit: u64) -> Result<(), String> {
        self.edit(id, |inner, id| {
            let s = inner.shares.get_mut(id).unwrap();
            let links = s["links"].as_array_mut().ok_or("That share is damaged.")?;
            let n = if labels.is_empty() { count } else { labels.len() };
            if n < 1 || links.len() + n > MAX_LINKS {
                return Err(format!("A share can have up to {MAX_LINKS} links."));
            }
            let people = links.iter().filter(|l| l["label"].as_str().is_some_and(|x| x.starts_with("Person "))).count();
            for i in 0..n {
                let label = labels.get(i).cloned().unwrap_or_else(|| format!("Person {}", people + i + 1));
                links.push(new_link(&label, Some(limit), "visits"));
            }
            Ok(())
        })
    }

    pub fn set_link(&self, id: &str, link_id: &str, changes: &Value) -> Result<(), String> {
        self.edit(id, |inner, id| {
            let s = inner.shares.get_mut(id).unwrap();
            let link = s["links"].as_array_mut().and_then(|a| a.iter_mut().find(|l| l["id"] == link_id))
                .ok_or("That link doesn't exist any more.")?;
            if let Some(b) = changes["blocked"].as_bool() {
                link["blocked"] = json!(b);
            }
            if let Some(limit) = changes.get("limit") {
                link["max"] = match limit.as_u64().or_else(|| limit.as_str().and_then(|x| x.parse().ok())) {
                    Some(n) if n > 0 => json!(n),
                    _ => Value::Null,
                };
            }
            if let Some(label) = changes["label"].as_str().map(str::trim).filter(|l| !l.is_empty()) {
                link["label"] = json!(label.chars().take(60).collect::<String>());
            }
            Ok(())
        })
    }

    // ---- for the app's list
    pub fn summary(&self, base_url: Option<&str>) -> Value {
        let inner = self.inner.lock().unwrap();
        let mut list: Vec<&Value> = inner.shares.values().collect();
        list.sort_by(|a, b| b["created"].as_f64().partial_cmp(&a["created"].as_f64()).unwrap_or(std::cmp::Ordering::Equal));
        json!(list.iter().map(|s| {
            let items = s["items"].as_array().cloned().unwrap_or_default();
            json!({
                "id": s["id"], "title": s["title"], "created": s["created"], "expires": s["expires"], "ended": s["ended"],
                "active": is_active(s), "files": items.len(), "total": s["total"], "skipped": s["skipped"],
                "password": !s["password"].is_null(),
                "preview": items.iter().take(200).map(|i| json!({ "name": i["name"], "size": i["size"], "downloads": i["downloads"].as_u64().unwrap_or(0) })).collect::<Vec<_>>(),
                "downloads": items.iter().map(|i| i["downloads"].as_u64().unwrap_or(0)).sum::<u64>(),
                "kind": s["kind"].as_str().unwrap_or("send"), "max_bytes": s["max_bytes"], "max_files": s["max_files"], "folder": s["folder"],
                "links": s["links"].as_array().into_iter().flatten().map(|l| json!({
                    "id": l["id"], "label": l["label"], "max": l["max"], "uses": l["uses"], "blocked": l["blocked"], "last": l["last"],
                    "open": link_open(&inner, s, l), "counts": l["counts"].as_str().unwrap_or("visits"),
                    "url": base_url.map(|b| format!("{b}/s/{}", l["token"].as_str().unwrap_or(""))),
                })).collect::<Vec<_>>(),
            })
        }).collect::<Vec<_>>())
    }

    // ---- for the share server

    /// (share id, link id) for a link token, whatever its state.
    pub fn find(&self, token: &str) -> Option<(String, String)> {
        if !(20..=40).contains(&token.len()) || !token.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') {
            return None;
        }
        let inner = self.inner.lock().unwrap();
        for s in inner.shares.values() {
            for l in s["links"].as_array().into_iter().flatten() {
                if constant_time_eq::constant_time_eq(l["token"].as_str().unwrap_or("").as_bytes(), token.as_bytes()) {
                    return Some((s["id"].as_str()?.to_string(), l["id"].as_str()?.to_string()));
                }
            }
        }
        None
    }

    pub fn new_session(&self, share_id: &str, link_id: &str) -> String {
        let sid = token_urlsafe(18);
        let mut inner = self.inner.lock().unwrap();
        let mut mine: Vec<(String, f64)> = inner.open_sessions.iter()
            .filter(|(_, v)| v["link"] == link_id).map(|(k, v)| (k.clone(), v["created"].as_f64().unwrap_or(0.0))).collect();
        if mine.len() + 1 > OPEN_SESSIONS_PER_LINK {
            mine.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
            for (k, _) in mine.iter().take(mine.len() + 1 - OPEN_SESSIONS_PER_LINK) {
                inner.open_sessions.remove(k); // forget the oldest previews/bots
            }
        }
        inner.open_sessions.insert(sid.clone(), json!({ "share": share_id, "link": link_id, "created": now(), "counted": false, "unlocked": false }));
        sid
    }

    pub fn unlock(&self, share_id: &str, sid: &str) {
        let mut inner = self.inner.lock().unwrap();
        let mut save = false;
        if let Some(sess) = inner.shares.get_mut(share_id).and_then(|s| s["sessions"].get_mut(sid)) {
            sess["unlocked"] = json!(true);
            save = true;
        } else if let Some(sess) = inner.open_sessions.get_mut(sid) {
            sess["unlocked"] = json!(true);
        }
        if save {
            self.save(&inner);
        }
    }

    /// Called before sending file data. `item` is the file's number or None for "Download all";
    /// `resume` is true for a request that continues a download (a range not starting at 0).
    /// Every download counts once; continuing one this visit already started is free. On a
    /// personal link only the visit's first download uses it up. Ok(fresh) if allowed, else why not.
    pub fn start_download(&self, share_id: &str, link_id: &str, sid: Option<&str>, item: Option<usize>, resume: bool) -> Result<bool, &'static str> {
        let mut inner = self.inner.lock().unwrap();
        let inner = &mut *inner;
        let Some(s) = inner.shares.get(share_id).cloned() else { return Err("ended") };
        if !is_active(&s) {
            return Err("ended");
        }
        let link = s["links"].as_array().and_then(|a| a.iter().find(|l| l["id"] == link_id)).cloned().ok_or("ended")?;
        if link["blocked"].as_bool().unwrap_or(false) {
            return Err("blocked");
        }
        let sess = session_in(inner, &s, link_id, sid).ok_or("no-session")?;
        let sid = sid.unwrap();
        if !s["password"].is_null() && !sess["unlocked"].as_bool().unwrap_or(false) {
            return Err("password");
        }
        let key = item.map(|i| json!(i)).unwrap_or(json!("zip"));
        let started: Vec<Value> = sess["started"].as_array().cloned().unwrap_or_default();
        let fresh = !(resume && started.contains(&key));
        let per_file = link["counts"] == "files";
        let uses_one = if per_file { fresh } else { !sess["counted"].as_bool().unwrap_or(false) };
        let share = inner.shares.get_mut(share_id).unwrap();
        if uses_one {
            let l = share["links"].as_array_mut().unwrap().iter_mut().find(|l| l["id"] == link_id).unwrap();
            if let Some(max) = l["max"].as_u64() {
                if l["uses"].as_u64().unwrap_or(0) >= max {
                    return Err("used-up");
                }
            }
            l["uses"] = json!(l["uses"].as_u64().unwrap_or(0) + 1);
        }
        if fresh {
            if let Some(items) = share["items"].as_array_mut() {
                for (n, it) in items.iter_mut().enumerate() {
                    if item.is_none() || item == Some(n) {
                        it["downloads"] = json!(it["downloads"].as_u64().unwrap_or(0) + 1);
                    }
                }
            }
        }
        let mut started = started;
        if !started.contains(&key) {
            started.push(key);
        }
        if let Some(existing) = share["sessions"].get_mut(sid) {
            existing["started"] = json!(started);
        } else {
            // the visit's first download: remember it across restarts so it can resume
            inner.open_sessions.remove(sid);
            share["sessions"][sid] = json!({ "link": link_id, "counted": true, "started": started,
                "unlocked": sess["unlocked"].as_bool().unwrap_or(false), "created": sess["created"].as_f64().unwrap_or(now()) });
        }
        let l = share["links"].as_array_mut().unwrap().iter_mut().find(|l| l["id"] == link_id).unwrap();
        l["last"] = json!(now());
        self.save(inner);
        Ok(fresh)
    }

    pub fn session(&self, share_id: &str, link_id: &str, sid: Option<&str>) -> Option<Value> {
        let inner = self.inner.lock().unwrap();
        let s = inner.shares.get(share_id)?;
        session_in(&inner, s, link_id, sid)
    }

    // ---- request links
    pub fn reserve(&self, share_id: &str, size: u64) -> Result<(), String> {
        let mut inner = self.inner.lock().unwrap();
        let s = inner.shares.get(share_id).cloned().ok_or("This link isn't available any more.")?;
        if !is_active(&s) {
            return Err("This link isn't available any more.".into());
        }
        if let Some(problem) = room(&inner, &s, size) {
            return Err(problem);
        }
        let p = inner.pending.entry(share_id.to_string()).or_insert((0, 0));
        p.0 += 1;
        p.1 += size;
        Ok(())
    }

    pub fn release(&self, share_id: &str, size: u64) {
        if let Some(p) = self.inner.lock().unwrap().pending.get_mut(share_id) {
            p.0 = p.0.saturating_sub(1);
            p.1 = p.1.saturating_sub(size);
        }
    }

    pub fn received(&self, share_id: &str, link_id: &str, name: &str, size: u64, ip: &str) {
        self.release(share_id, size);
        let mut inner = self.inner.lock().unwrap();
        let Some(s) = inner.shares.get_mut(share_id) else { return };
        if let Some(items) = s["items"].as_array_mut() {
            items.push(json!({ "name": name, "size": size, "time": now(), "from": ip }));
        }
        s["total"] = json!(s["total"].as_u64().unwrap_or(0) + size);
        if let Some(l) = s["links"].as_array_mut().and_then(|a| a.iter_mut().find(|l| l["id"] == link_id)) {
            l["uses"] = json!(l["uses"].as_u64().unwrap_or(0) + 1);
            l["last"] = json!(now());
        }
        let title = s["title"].as_str().unwrap_or("").to_string();
        self.save(&inner);
        config::log(&format!("Request {title:?}: received {name} ({}) from {ip}", human(size as f64)));
    }
}

fn session_in(inner: &Inner, s: &Value, link_id: &str, sid: Option<&str>) -> Option<Value> {
    let sid = sid.filter(|x| !x.is_empty() && x.len() <= 64)?;
    if let Some(sess) = s["sessions"].get(sid) {
        if sess["link"] == link_id {
            return Some(sess.clone());
        }
    }
    let sess = inner.open_sessions.get(sid)?;
    (sess["share"] == s["id"] && sess["link"] == link_id).then(|| sess.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_shares() -> Shares {
        Shares { inner: Mutex::new(Inner::default()), path: std::env::temp_dir().join(format!("swl-test-{}.json", token_hex(4))) }
    }

    #[test]
    fn passwords_match_python() {
        // made with Python: share.hash_password("hunter2", bytes.fromhex("00112233445566778899aabbccddeeff"))
        let stored = json!({ "salt": "00112233445566778899aabbccddeeff", "hash": PY_HASH });
        assert!(check_password("hunter2", &stored));
        assert!(!check_password("hunter3", &stored));
        assert!(check_password("x", &hash_password("x")));
    }
    const PY_HASH: &str = include_str!("../tests/fixtures/scrypt-hunter2.txt");

    #[test]
    fn private_names() {
        assert!(looks_private("id_rsa"));
        assert!(looks_private("Bank PASSWORDS.xlsx"));
        assert!(looks_private("server.pem"));
        assert!(!looks_private("holiday.jpg"));
    }

    #[test]
    fn download_counting() {
        let dir = std::env::temp_dir().join(format!("swl-share-{}", token_hex(4)));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), "hello").unwrap();
        let sh = test_shares();
        let id = sh.create(&[dir.join("a.txt")], &json!({ "hours": 1, "count": 2 }), false).unwrap()["share"].as_str().unwrap().to_string();
        let link = sh.get(&id).unwrap()["links"][0]["id"].as_str().unwrap().to_string();
        let sid = sh.new_session(&id, &link);
        assert_eq!(sh.start_download(&id, &link, Some(&sid), Some(0), false), Ok(true));
        assert_eq!(sh.start_download(&id, &link, Some(&sid), Some(0), true), Ok(false), "resuming is free");
        assert_eq!(sh.start_download(&id, &link, Some(&sid), Some(0), false), Ok(true));
        assert_eq!(sh.start_download(&id, &link, Some(&sid), Some(0), false), Err("used-up"));
        assert_eq!(sh.start_download(&id, &link, None, Some(0), false), Err("no-session"));
        sh.set_password(&id, "pw").unwrap();
        let sid2 = sh.new_session(&id, &link);
        assert_eq!(sh.start_download(&id, &link, Some(&sid2), Some(0), false), Err("password"));
        sh.end(&id).unwrap();
        assert_eq!(sh.start_download(&id, &link, Some(&sid), Some(0), true), Err("ended"));
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Checks the rules only; nothing on disk is created or removed.
    #[test]
    fn never_shares_system_folders() {
        let forbidden = forbidden_folders();
        if let Some(home) = dirs::home_dir() {
            assert!(forbidden.iter().any(|f| inside(&home.join("AppData").join("Roaming").join("x"), f)));
            assert!(!forbidden.iter().any(|f| inside(&home.join("Documents"), f)));
        }
        assert!(forbidden.iter().any(|f| inside(&crate::config::config_dir().join("shares.json"), f)));
        assert!(!inside(Path::new("C:\\Windows2"), Path::new("C:\\Windows")));
    }
}
