//! Updates from GitHub Releases, installed only if signed with the owner's key.
//! The same rules as the Python version (updater.py):
//! - the release's manifest.json must verify against PUBLIC_KEY (Ed25519) or it is rejected;
//! - the downloaded setup must match the manifest's size and SHA-256, checked again right before
//!   it starts; nothing installs until the user clicks Update, and Windows asks for admin;
//! - checks are conditional (If-None-Match), so checking often costs almost nothing.

use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

const REPO: &str = "Lewy366/swiftlane";
pub const PUBLIC_KEY: &str = "67ad5096eec22a2fe4fb1290f7f3f199150d319a2b45409ae836bf14276fb0b2";
const MAX_SETUP: u64 = 400 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct Update {
    pub version: String,
    pub notes: String,
    pub size: u64,
    pub published: String,
    pub sha256: String,
    pub url: String,
    pub available: bool,
}

impl Update {
    pub fn public(&self) -> Value {
        json!({ "version": self.version, "notes": self.notes, "size": self.size, "published": self.published })
    }
}

#[derive(Default)]
struct Cache {
    etag: Option<String>,
    result: Option<Update>,
}

static CACHE: Mutex<Cache> = Mutex::new(Cache { etag: None, result: None });

pub fn version_tuple(v: &str) -> (u64, u64, u64) {
    let mut nums = v.split(|c: char| !c.is_ascii_digit()).filter(|s| !s.is_empty()).map(|s| s.parse().unwrap_or(0));
    (nums.next().unwrap_or(0), nums.next().unwrap_or(0), nums.next().unwrap_or(0))
}

fn client() -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        .user_agent("Swiftlane-updater")
        .https_only(true)
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .map_err(|_| "Couldn't start the update check.".to_string())
}

fn get_limited(c: &reqwest::blocking::Client, url: &str, limit: u64) -> Result<Vec<u8>, String> {
    if !url.starts_with("https://") {
        return Err("Unexpected update address.".into());
    }
    let r = c.get(url).send().map_err(|_| offline())?;
    if !r.status().is_success() {
        return Err("GitHub sent something unexpected.".into());
    }
    let mut data = Vec::new();
    r.take(limit + 1).read_to_end(&mut data).map_err(|_| offline())?;
    if data.len() as u64 > limit {
        return Err("The update information is unexpectedly large.".into());
    }
    Ok(data)
}

fn offline() -> String {
    "Couldn't reach GitHub. Check your internet connection.".into()
}

/// Is `manifest` signed by the owner's key? (sig is hex text, like the Python version writes it.)
pub fn verify(manifest: &[u8], sig_hex: &str) -> Result<(), String> {
    let bad = || "The update isn't signed by Swiftlane's owner, so it was rejected.".to_string();
    let key_bytes: [u8; 32] = hex::decode(PUBLIC_KEY).ok().and_then(|b| b.try_into().ok()).ok_or_else(bad)?;
    let key = VerifyingKey::from_bytes(&key_bytes).map_err(|_| bad())?;
    let sig_bytes: [u8; 64] = hex::decode(sig_hex.trim()).ok().and_then(|b| b.try_into().ok()).ok_or_else(bad)?;
    key.verify(manifest, &Signature::from_bytes(&sig_bytes)).map_err(|_| bad())
}

/// The latest release, verified. Errors are plain sentences shown to the user.
pub fn check(current: &str) -> Result<Update, String> {
    let c = client()?;
    let mut cache = CACHE.lock().unwrap();
    let mut req = c.get(format!("https://api.github.com/repos/{REPO}/releases/latest")).header("Accept", "application/vnd.github+json");
    if let (Some(etag), Some(_)) = (&cache.etag, &cache.result) {
        req = req.header("If-None-Match", etag);
    }
    let r = req.send().map_err(|_| offline())?;
    if r.status() == reqwest::StatusCode::NOT_MODIFIED {
        if let Some(u) = &cache.result {
            let mut u = u.clone();
            u.available = version_tuple(&u.version) > version_tuple(current);
            return Ok(u);
        }
    }
    if !r.status().is_success() {
        return Err("GitHub sent something unexpected.".into());
    }
    let etag = r.headers().get("etag").and_then(|v| v.to_str().ok()).map(String::from);
    let mut body = Vec::new();
    r.take(2 * 1024 * 1024 + 1).read_to_end(&mut body).map_err(|_| offline())?;
    let release: Value = serde_json::from_slice(&body).map_err(|_| "GitHub sent something unexpected.".to_string())?;
    let asset = |name: &str| {
        release["assets"].as_array().and_then(|a| {
            a.iter().find(|x| x["name"] == name).and_then(|x| x["browser_download_url"].as_str().map(String::from))
        })
    };
    let (Some(man_url), Some(sig_url)) = (asset("manifest.json"), asset("manifest.sig")) else {
        return Err("The latest release is missing its signature.".into());
    };
    let manifest = get_limited(&c, &man_url, 256 * 1024)?;
    let sig = get_limited(&c, &sig_url, 4096)?;
    verify(&manifest, &String::from_utf8_lossy(&sig))?;
    let m: Value = serde_json::from_slice(&manifest).map_err(|_| "The update information is incomplete.".to_string())?;
    let sha = m["sha256"].as_str().unwrap_or("");
    let file = m["file"].as_str().unwrap_or("");
    let size = m["size"].as_u64().unwrap_or(0);
    let url = asset(file);
    if m["app"] != "Swiftlane" || url.is_none() || sha.len() != 64 || !sha.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()) {
        return Err("The update information is incomplete.".into());
    }
    if size == 0 || size > MAX_SETUP {
        return Err("The update has an unexpected size.".into());
    }
    let version = m["version"].as_str().unwrap_or("0").to_string();
    let u = Update {
        available: version_tuple(&version) > version_tuple(current),
        notes: m["notes"].as_str().unwrap_or("").chars().take(4000).collect(),
        published: m["published"].as_str().unwrap_or("").to_string(),
        sha256: sha.to_string(),
        url: url.unwrap(),
        size,
        version,
    };
    cache.etag = etag;
    cache.result = Some(u.clone());
    Ok(u)
}

/// Downloads the setup into a fresh private temp folder and checks it. Returns its path.
pub fn download(u: &Update, progress: impl Fn(f64)) -> Result<PathBuf, String> {
    let c = client()?;
    let folder = std::env::temp_dir().join(format!("Swiftlane-update-{:016x}", rand_u64()));
    std::fs::create_dir(&folder).map_err(|_| "Couldn't make a folder for the download.".to_string())?;
    let path = folder.join("Swiftlane Setup.exe");
    let result = (|| {
        if !u.url.starts_with("https://") {
            return Err("Unexpected update address.".to_string());
        }
        let mut r = c.get(&u.url).send().map_err(|_| "The download was interrupted. Try again.".to_string())?;
        if !r.status().is_success() {
            return Err("The download was interrupted. Try again.".into());
        }
        let mut f = std::fs::OpenOptions::new().write(true).create_new(true).open(&path)
            .map_err(|_| "Couldn't save the download.".to_string())?;
        let (mut h, mut done, mut buf) = (Sha256::new(), 0u64, vec![0u8; 256 * 1024]);
        loop {
            let n = r.read(&mut buf).map_err(|_| "The download was interrupted. Try again.".to_string())?;
            if n == 0 {
                break;
            }
            done += n as u64;
            if done > u.size {
                return Err("The download is bigger than announced, so it was rejected.".into());
            }
            h.update(&buf[..n]);
            std::io::Write::write_all(&mut f, &buf[..n]).map_err(|_| "Couldn't save the download.".to_string())?;
            progress(done as f64 / u.size as f64);
        }
        if done != u.size || hex::encode(h.finalize()) != u.sha256 {
            return Err("The download doesn't match the signed update, so it was rejected.".into());
        }
        Ok(())
    })();
    match result {
        Ok(()) => Ok(path),
        Err(e) => {
            let _ = std::fs::remove_dir_all(&folder);
            Err(e)
        }
    }
}

/// Re-check right before starting it (nothing may have swapped the file meanwhile).
pub fn still_matches(path: &Path, u: &Update) -> bool {
    let Ok(mut f) = std::fs::File::open(path) else { return false };
    let (mut h, mut buf) = (Sha256::new(), vec![0u8; 256 * 1024]);
    loop {
        match f.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => h.update(&buf[..n]),
            Err(_) => return false,
        }
    }
    hex::encode(h.finalize()) == u.sha256
}

fn rand_u64() -> u64 {
    let mut b = [0u8; 8];
    if getrandom::getrandom(&mut b).is_err() {
        let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        return t as u64 ^ std::process::id() as u64;
    }
    u64::from_le_bytes(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert!(version_tuple("2.0.0") > version_tuple("1.6.1"));
        assert_eq!(version_tuple("v1.4"), (1, 4, 0));
    }

    #[test]
    fn rejects_unsigned() {
        assert!(verify(b"{}", &"00".repeat(64)).is_err());
        assert!(verify(b"{}", "not hex").is_err());
    }
}
