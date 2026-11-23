//! Encrypted vault: a folder of files only your password opens. A port of Source/toolkit/vault.py
//! plus the vault_* methods of Source/swiftlane.py, and FILE-COMPATIBLE with it: a vault made by
//! one version opens in the other (see the tests).
//!
//! Layout of a vault folder:
//!     vault.json      header: scrypt settings + salt, and the master key wrapped with the password key
//!     index.bin       encrypted list of files (names, sizes, dates)
//!     files/<id>.bin  each file, encrypted in 1 MiB chunks
//!
//! A random 256-bit master key encrypts everything; the password only unwraps it, so changing the
//! password never re-encrypts files. Every chunk is AES-256-GCM with the file id, chunk number and a
//! "last chunk" flag bound in, so a changed, swapped, reordered or cut-off file fails to open instead
//! of giving back damaged data. There is no recovery: without the password the files stay locked.

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use serde_json::{json, Map, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager};
use zeroize::Zeroizing;

use super::arg;
use crate::api::{push, AppState};
use crate::config;
use crate::jobs::JobError;

const FORMAT: &str = "swiftlane-vault";
const VERSION: u64 = 1;
const CHUNK: usize = 1 << 20;
const TAG: usize = 16;
const MAGIC: &[u8; 4] = b"SLV1";
const SCRYPT_N: u64 = 1 << 17;
const SCRYPT_R: u64 = 8;
const SCRYPT_P: u64 = 1;
const MAX_MEM: u64 = 256 * 1024 * 1024; // the same limit Python's hashlib.scrypt gets
const KEY_AAD: &[u8] = b"swiftlane-vault/key/v1";
const INDEX_AAD: &[u8] = b"swiftlane-vault/index/v1";
const MIN_PASSWORD: usize = 8;

type Key = Zeroizing<[u8; 32]>;

#[derive(Debug)]
pub enum Error {
    WrongPassword(String),
    Msg(String),
    Cancelled,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::WrongPassword(m) | Error::Msg(m) => f.write_str(m),
            Error::Cancelled => f.write_str("Cancelled."),
        }
    }
}

fn msg(s: &str) -> Error {
    Error::Msg(s.to_string())
}

/// File errors in plain words.
impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        use std::io::ErrorKind::*;
        Error::Msg(match e.kind() {
            NotFound => "A file is missing (it may have been moved or deleted).".into(),
            PermissionDenied => "Windows didn't allow access to a file (it may be open in another program).".into(),
            StorageFull => "There's no room left on the drive.".into(),
            _ => format!("A file couldn't be read or written ({e})."),
        })
    }
}

type Res<T> = Result<T, Error>;

fn random<const N: usize>() -> [u8; N] {
    let mut b = [0u8; N];
    getrandom::getrandom(&mut b).expect("Windows' random number source failed");
    b
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn derive(password: &str, kdf: &Value) -> Res<Key> {
    let damaged = || msg("The vault's settings look damaged.");
    let (n, r, p) = (kdf["n"].as_u64().ok_or_else(damaged)?, kdf["r"].as_u64().ok_or_else(damaged)?, kdf["p"].as_u64().ok_or_else(damaged)?);
    let salt = hex::decode(kdf["salt"].as_str().ok_or_else(damaged)?).map_err(|_| damaged())?;
    // Python's hashlib.scrypt(maxmem=256 MiB) refuses anything bigger, so a vault can't need more.
    if !n.is_power_of_two() || n < 2 || 128 * r * (n + 2) + 128 * r * p > MAX_MEM {
        return Err(damaged());
    }
    let params = scrypt::Params::new(n.trailing_zeros() as u8, r as u32, p as u32, 32).map_err(|_| damaged())?;
    let mut out = Zeroizing::new([0u8; 32]);
    scrypt::scrypt(password.as_bytes(), &salt, &params, &mut out[..]).map_err(|_| damaged())?;
    Ok(out)
}

fn cipher(key: &[u8; 32]) -> Aes256Gcm {
    Aes256Gcm::new(key.into())
}

/// nonce (12) + ciphertext + tag, like Python's nonce + AESGCM.encrypt(nonce, data, aad).
fn seal(key: &[u8; 32], data: &[u8], aad: &[u8]) -> Vec<u8> {
    let nonce = random::<12>();
    let mut out = nonce.to_vec();
    out.extend(cipher(key).encrypt(Nonce::from_slice(&nonce), Payload { msg: data, aad }).expect("AES-GCM encryption"));
    out
}

fn open_sealed(key: &[u8; 32], blob: &[u8], aad: &[u8]) -> Option<Vec<u8>> {
    if blob.len() < 12 + TAG {
        return None;
    }
    cipher(key).decrypt(Nonce::from_slice(&blob[..12]), Payload { msg: &blob[12..], aad }).ok()
}

fn wrap(password: &str, master: &[u8; 32]) -> Res<(Value, String)> {
    let kdf = json!({ "name": "scrypt", "n": SCRYPT_N, "r": SCRYPT_R, "p": SCRYPT_P, "salt": hex::encode(random::<16>()) });
    let pkey = derive(password, &kdf)?;
    Ok((kdf, hex::encode(seal(&pkey, master, KEY_AAD))))
}

fn unwrap_key(password: &str, h: &Value) -> Res<Option<Key>> {
    let blob = hex::decode(h["key"].as_str().unwrap_or("")).map_err(|_| msg("The vault's settings look damaged."))?;
    let pkey = derive(password, &h["kdf"])?;
    let Some(plain) = open_sealed(&pkey, &blob, KEY_AAD).map(Zeroizing::new) else { return Ok(None) };
    let key: [u8; 32] = plain.as_slice().try_into().map_err(|_| msg("The vault's settings look damaged."))?;
    Ok(Some(Zeroizing::new(key)))
}

fn write_atomic(path: &Path, data: &[u8]) -> Res<()> {
    let tmp = with_suffix(path, ".tmp");
    let mut f = std::fs::File::create(&tmp)?;
    f.write_all(data)?;
    f.flush()?;
    f.sync_all()?;
    drop(f);
    std::fs::rename(&tmp, path)?;
    Ok(())
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(suffix);
    path.with_file_name(name)
}

/// The same JSON layout Python writes (json.dumps(..., indent=1)).
fn pretty(v: &Value) -> Vec<u8> {
    let mut out = Vec::new();
    let mut ser = serde_json::Serializer::with_formatter(&mut out, serde_json::ser::PrettyFormatter::with_indent(b" "));
    serde::Serialize::serialize(v, &mut ser).expect("JSON");
    out
}

/// None if the password is acceptable, otherwise why not.
pub fn check_password(password: &str) -> Option<String> {
    if password.chars().count() < MIN_PASSWORD {
        return Some(format!("Use at least {MIN_PASSWORD} characters."));
    }
    if password.chars().collect::<std::collections::HashSet<_>>().len() < 4 {
        return Some("That password is too easy to guess.".into());
    }
    None
}

/// A file name that is safe on Windows (same rules as Python's safe_name).
pub fn safe_name(name: &str) -> String {
    let b = name.as_bytes();
    let name = if b.len() >= 2 && b[1] == b':' && b[0].is_ascii_alphabetic() { &name[2..] } else { name }; // "C:" drive
    let last = name.split(['/', '\\']).filter(|s| !s.is_empty()).last().unwrap_or("");
    let cleaned: String = last.chars().map(|c| if "<>:\"/\\|?*".contains(c) || (c as u32) < 0x20 { '_' } else { c }).collect();
    let cleaned: String = cleaned.trim_matches([' ', '.']).chars().take(200).collect();
    if cleaned.is_empty() { "file".into() } else { cleaned }
}

/// folder/name, or "name (2).ext" etc. if taken.
pub fn unique_path(folder: &Path, name: &str) -> PathBuf {
    let p = folder.join(name);
    let stem = p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    let suffix = p.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    let mut p = p;
    let mut n = 2;
    while p.symlink_metadata().is_ok() {
        p = folder.join(format!("{stem} ({n}){suffix}"));
        n += 1;
    }
    p
}

/// Overwrite a file once with random bytes, then delete it. On SSDs and synced folders (OneDrive)
/// old copies may survive elsewhere, so this lowers the risk rather than guaranteeing it.
pub fn shred(path: &Path) -> Res<()> {
    let meta = path.symlink_metadata()?;
    if meta.is_file() {
        let mut f = std::fs::OpenOptions::new().write(true).open(path)?;
        let mut left = meta.len();
        let mut buf = vec![0u8; CHUNK];
        while left > 0 {
            let n = left.min(CHUNK as u64) as usize;
            getrandom::getrandom(&mut buf[..n]).expect("Windows' random number source failed");
            f.write_all(&buf[..n])?;
            left -= n as u64;
        }
        f.flush()?;
        f.sync_all()?;
    }
    std::fs::remove_file(path)?; // a link is only removed, never followed
    Ok(())
}

/// Reads until `buf` is full or the file ends (like Python's f.read(n) on a file).
fn read_full(r: &mut impl Read, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut got = 0;
    while got < buf.len() {
        match r.read(&mut buf[got..]) {
            Ok(0) => break,
            Ok(n) => got += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(got)
}

fn chunk_nonce(prefix: &[u8; 8], n: u32) -> [u8; 12] {
    let mut nonce = [0u8; 12];
    nonce[..8].copy_from_slice(prefix);
    nonce[8..].copy_from_slice(&n.to_be_bytes());
    nonce
}

/// file id (ASCII) + chunk number (big-endian u32) + last-chunk flag (one byte).
fn chunk_aad(file_id: &str, n: u32, last: bool) -> Vec<u8> {
    let mut aad = file_id.as_bytes().to_vec();
    aad.extend(n.to_be_bytes());
    aad.push(last as u8);
    aad
}

fn is_file_id(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

#[derive(Default)]
struct Inner {
    key: Option<Key>,
    index: Map<String, Value>,
    temp: Option<PathBuf>,
    generation: u64, // goes up at every lock, so a running job notices the vault was locked
}

pub struct Vault {
    pub folder: PathBuf,
    temp_root: PathBuf,
    inner: Mutex<Inner>,
}

impl Vault {
    /// `temp_root`: where opened files are decrypted to (wiped on lock).
    pub fn new(folder: PathBuf, temp_root: PathBuf) -> Self {
        Vault { folder, temp_root, inner: Mutex::new(Inner::default()) }
    }

    fn header_path(&self) -> PathBuf {
        self.folder.join("vault.json")
    }

    pub fn exists(&self) -> bool {
        self.header_path().is_file()
    }

    pub fn unlocked(&self) -> bool {
        self.inner.lock().unwrap().key.is_some()
    }

    fn header(&self) -> Res<Value> {
        let h: Value = std::fs::read(self.header_path())
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .ok_or_else(|| msg("This folder doesn't hold a Swiftlane vault."))?;
        if h["format"].as_str() != Some(FORMAT) || h["version"].as_u64() != Some(VERSION) {
            return Err(msg("This vault was made by a different version of Swiftlane."));
        }
        let kdf = &h["kdf"];
        let n = kdf["n"].as_u64().unwrap_or(0);
        // Refuse settings that are weaker than ours (someone editing the header to speed up guessing) or absurdly big.
        if kdf["name"].as_str() != Some("scrypt")
            || !(SCRYPT_N..=1 << 20).contains(&n)
            || kdf["r"].as_u64() != Some(SCRYPT_R)
            || kdf["p"].as_u64() != Some(SCRYPT_P)
            || kdf["salt"].as_str().map_or(0, |s| s.chars().count()) != 32
        {
            return Err(msg("The vault's settings look damaged."));
        }
        Ok(h)
    }

    // ------------------------------------------------------------ open / close
    pub fn create(&self, password: &str) -> Res<()> {
        if self.exists() {
            return Err(msg("There is already a vault in this folder."));
        }
        if let Some(why) = check_password(password) {
            return Err(Error::Msg(why));
        }
        std::fs::create_dir_all(self.folder.join("files"))?;
        let master: Key = Zeroizing::new(random::<32>());
        let (kdf, wrapped) = wrap(password, &master)?;
        let header = json!({ "format": FORMAT, "version": VERSION, "kdf": kdf, "key": wrapped, "created": now() });
        write_atomic(&self.header_path(), &pretty(&header))?;
        let mut inner = self.inner.lock().unwrap();
        inner.key = Some(master);
        inner.index = Map::new();
        self.save_index(&inner)
    }

    pub fn unlock(&self, password: &str) -> Res<()> {
        let h = self.header()?;
        let master = unwrap_key(password, &h)?.ok_or_else(|| Error::WrongPassword("Wrong password.".into()))?;
        let index = self.load_index(&master)?;
        let mut inner = self.inner.lock().unwrap();
        inner.key = Some(master);
        inner.index = index;
        Ok(())
    }

    pub fn lock(&self) {
        {
            let mut inner = self.inner.lock().unwrap();
            inner.key = None;
            inner.index = Map::new();
            inner.temp = None;
            inner.generation += 1;
        }
        wipe_leftovers(&self.temp_root);
    }

    pub fn change_password(&self, old: &str, new: &str) -> Res<()> {
        if let Some(why) = check_password(new) {
            return Err(Error::Msg(why));
        }
        let mut h = self.header()?;
        let master = unwrap_key(old, &h)?.ok_or_else(|| Error::WrongPassword("The current password is wrong.".into()))?;
        let (kdf, wrapped) = wrap(new, &master)?;
        h["kdf"] = kdf;
        h["key"] = json!(wrapped);
        write_atomic(&self.header_path(), &pretty(&h))
    }

    /// The key and the lock generation, for work done outside the lock.
    fn session(&self) -> Res<(Key, u64)> {
        let inner = self.inner.lock().unwrap();
        let key = inner.key.clone().ok_or_else(|| msg("The vault is locked."))?;
        Ok((key, inner.generation))
    }

    fn still_open(&self, generation: u64) -> Res<()> {
        let inner = self.inner.lock().unwrap();
        if inner.key.is_none() || inner.generation != generation {
            return Err(msg("The vault was locked, so Swiftlane stopped."));
        }
        Ok(())
    }

    // ------------------------------------------------------------ index
    fn load_index(&self, key: &[u8; 32]) -> Res<Map<String, Value>> {
        let path = self.folder.join("index.bin");
        if !path.exists() {
            return Ok(Map::new());
        }
        let blob = std::fs::read(path)?;
        open_sealed(key, &blob, INDEX_AAD)
            .and_then(|plain| serde_json::from_slice::<Map<String, Value>>(&plain).ok())
            .ok_or_else(|| msg("The vault's file list is damaged."))
    }

    fn save_index(&self, inner: &Inner) -> Res<()> {
        let key = inner.key.as_ref().ok_or_else(|| msg("The vault is locked."))?;
        let data = serde_json::to_vec(&inner.index).expect("JSON");
        write_atomic(&self.folder.join("index.bin"), &seal(key, &data, INDEX_AAD))
    }

    /// The files in the vault, newest first: {id, name, size, added}.
    pub fn items(&self) -> Res<Vec<Value>> {
        let inner = self.inner.lock().unwrap();
        if inner.key.is_none() {
            return Err(msg("The vault is locked."));
        }
        let mut out: Vec<Value> = inner
            .index
            .iter()
            .map(|(id, v)| {
                let mut item = Map::new();
                item.insert("id".into(), json!(id));
                if let Value::Object(m) = v {
                    item.extend(m.clone());
                }
                Value::Object(item)
            })
            .collect();
        out.sort_by(|a, b| b["added"].as_f64().unwrap_or(0.0).total_cmp(&a["added"].as_f64().unwrap_or(0.0)));
        Ok(out)
    }

    // ------------------------------------------------------------ files
    fn blob(&self, file_id: &str) -> Res<PathBuf> {
        if !is_file_id(file_id) {
            return Err(msg("Unknown file."));
        }
        Ok(self.folder.join("files").join(format!("{file_id}.bin")))
    }

    /// Encrypts files into the vault. `progress` gets 0..1; `cancelled` is asked between chunks.
    pub fn add(&self, paths: &[PathBuf], mut progress: impl FnMut(f64), cancelled: impl Fn() -> bool, shred_originals: bool) -> Res<Vec<String>> {
        let (key, generation) = self.session()?;
        let aes = cipher(&key);
        let sources: Vec<&PathBuf> = paths.iter().filter(|p| p.is_file()).collect();
        let total = sources.iter().map(|p| p.metadata().map(|m| m.len()).unwrap_or(0)).sum::<u64>().max(1);
        let mut done = 0u64;
        let mut added = Vec::new();
        std::fs::create_dir_all(self.folder.join("files"))?;
        for src in sources {
            let file_id = hex::encode(random::<16>());
            let prefix = random::<8>();
            let out = self.blob(&file_id)?;
            let tmp = with_suffix(&out, ".tmp");
            let result = (|| -> Res<()> {
                let mut f = std::fs::File::open(src)?;
                let mut o = std::fs::File::create(&tmp)?;
                o.write_all(MAGIC)?;
                o.write_all(&prefix)?;
                let (mut cur, mut next) = (vec![0u8; CHUNK], vec![0u8; CHUNK]);
                let mut len = read_full(&mut f, &mut cur)?;
                let mut n: u32 = 0;
                loop {
                    if cancelled() {
                        return Err(Error::Cancelled);
                    }
                    self.still_open(generation)?;
                    let next_len = read_full(&mut f, &mut next)?;
                    let last = next_len == 0;
                    let nonce = chunk_nonce(&prefix, n);
                    let sealed = aes
                        .encrypt(Nonce::from_slice(&nonce), Payload { msg: &cur[..len], aad: &chunk_aad(&file_id, n, last) })
                        .expect("AES-GCM encryption");
                    o.write_all(&sealed)?;
                    done += len as u64;
                    progress(done as f64 / total as f64);
                    if last {
                        break;
                    }
                    std::mem::swap(&mut cur, &mut next);
                    len = next_len;
                    n = n.checked_add(1).ok_or_else(|| msg("This file is too big for the vault."))?;
                }
                o.flush()?;
                o.sync_all()?;
                drop(o);
                std::fs::rename(&tmp, &out)?;
                Ok(())
            })();
            if let Err(e) = result {
                let _ = std::fs::remove_file(&tmp);
                return Err(e);
            }
            let size = src.metadata().map(|m| m.len()).unwrap_or(0);
            let name = safe_name(&src.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default());
            {
                let mut inner = self.inner.lock().unwrap();
                if inner.key.is_none() || inner.generation != generation {
                    drop(inner);
                    let _ = std::fs::remove_file(&out);
                    return Err(msg("The vault was locked, so Swiftlane stopped."));
                }
                inner.index.insert(file_id.clone(), json!({ "name": name, "size": size, "added": now() }));
                self.save_index(&inner)?;
            }
            added.push(file_id);
            if shred_originals {
                shred(src)?;
            }
        }
        Ok(added)
    }

    fn decrypt_to(&self, key: &[u8; 32], generation: u64, file_id: &str, dest: &Path, cancelled: &dyn Fn() -> bool) -> Res<PathBuf> {
        let aes = cipher(key);
        let tmp = with_suffix(dest, ".part");
        let result = (|| -> Res<()> {
            let mut f = BufReader::with_capacity(CHUNK + TAG, std::fs::File::open(self.blob(file_id)?)?);
            let mut head = [0u8; 12];
            if read_full(&mut f, &mut head)? != 12 || &head[..4] != MAGIC {
                return Err(msg("This file in the vault is damaged."));
            }
            let prefix: [u8; 8] = head[4..].try_into().expect("8 bytes");
            let mut o = std::fs::File::create(&tmp)?;
            let mut block = vec![0u8; CHUNK + TAG];
            let mut n: u32 = 0;
            loop {
                if cancelled() {
                    return Err(Error::Cancelled);
                }
                self.still_open(generation)?;
                let len = read_full(&mut f, &mut block)?;
                if len == 0 {
                    return Err(msg("This file in the vault is cut off."));
                }
                let last = f.fill_buf()?.is_empty();
                let nonce = chunk_nonce(&prefix, n);
                let plain = aes
                    .decrypt(Nonce::from_slice(&nonce), Payload { msg: &block[..len], aad: &chunk_aad(file_id, n, last) })
                    .map_err(|_| msg("This file in the vault was changed or damaged, so it won't be opened."))?;
                o.write_all(&plain)?;
                if last {
                    break;
                }
                n = n.checked_add(1).ok_or_else(|| msg("This file in the vault is damaged."))?;
            }
            o.flush()?;
            drop(o);
            std::fs::rename(&tmp, dest)?;
            Ok(())
        })();
        if let Err(e) = result {
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }
        Ok(dest.to_path_buf())
    }

    fn name_of(&self, file_id: &str) -> Res<String> {
        let inner = self.inner.lock().unwrap();
        if inner.key.is_none() {
            return Err(msg("The vault is locked."));
        }
        inner.index.get(file_id).map(|v| safe_name(v["name"].as_str().unwrap_or("file"))).ok_or_else(|| msg("Unknown file."))
    }

    /// Decrypts copies of the files into `folder` (never replacing anything there).
    pub fn export(&self, ids: &[String], folder: &Path, cancelled: &dyn Fn() -> bool) -> Res<Vec<PathBuf>> {
        let (key, generation) = self.session()?;
        let mut out = Vec::new();
        for id in ids {
            let Ok(name) = self.name_of(id) else { continue };
            out.push(self.decrypt_to(&key, generation, id, &unique_path(folder, &name), cancelled)?);
        }
        Ok(out)
    }

    /// Decrypts one file into a private temp folder (wiped when the vault locks) and returns its path.
    pub fn open_copy(&self, file_id: &str) -> Res<PathBuf> {
        let name = self.name_of(file_id)?;
        let (key, generation) = self.session()?;
        let temp = {
            let mut inner = self.inner.lock().unwrap();
            match &inner.temp {
                Some(t) if t.is_dir() => t.clone(),
                _ => {
                    let t = private_dir(&self.temp_root)?;
                    inner.temp = Some(t.clone());
                    t
                }
            }
        };
        self.decrypt_to(&key, generation, file_id, &unique_path(&temp, &name), &|| false)
    }

    pub fn remove(&self, ids: &[String]) -> Res<()> {
        let mut inner = self.inner.lock().unwrap();
        if inner.key.is_none() {
            return Err(msg("The vault is locked."));
        }
        for id in ids {
            if inner.index.remove(id).is_some() {
                self.save_index(&inner)?;
                let _ = std::fs::remove_file(self.blob(id)?);
            }
        }
        Ok(())
    }

    pub fn rename(&self, file_id: &str, name: &str) -> Res<()> {
        let mut inner = self.inner.lock().unwrap();
        if inner.key.is_none() {
            return Err(msg("The vault is locked."));
        }
        let entry = inner.index.get_mut(file_id).ok_or_else(|| msg("Unknown file."))?;
        entry["name"] = json!(safe_name(name));
        self.save_index(&inner)
    }
}

/// A new folder only this user can read, inside `root`.
fn private_dir(root: &Path) -> Res<PathBuf> {
    std::fs::create_dir_all(root)?;
    loop {
        let dir = root.join(format!("tmp{}", hex::encode(random::<6>())));
        #[allow(unused_mut)] // only Unix needs to set the folder's permissions; %TEMP% is private on Windows
        let mut b = std::fs::DirBuilder::new();
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut b, 0o700);
        match b.create(&dir) {
            Ok(()) => return Ok(dir),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.into()),
        }
    }
}

/// Deletes the decrypted copies (overwriting them first). Files still open in another app can't be
/// deleted yet; they go next time. True if nothing is left.
pub fn wipe_leftovers(root: &Path) -> bool {
    if root.symlink_metadata().is_err() {
        return true;
    }
    let mut all = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for e in entries.flatten() {
            let p = e.path();
            if e.file_type().is_ok_and(|t| t.is_dir()) {
                stack.push(p.clone());
            }
            all.push(p);
        }
    }
    all.sort_by_key(|p| std::cmp::Reverse(p.components().count()));
    for p in all {
        match p.symlink_metadata() {
            Ok(m) if m.is_dir() => {
                let _ = std::fs::remove_dir(&p);
            }
            Ok(_) => {
                let _ = shred(&p);
            }
            Err(_) => {}
        }
    }
    std::fs::remove_dir(root).is_ok()
}

// ================================================================ the page's calls

/// Where opened files are decrypted to: the same folder the Python version uses.
fn temp_root() -> PathBuf {
    std::env::temp_dir().join("Swiftlane vault")
}

static VAULT: Mutex<Option<Arc<Vault>>> = Mutex::new(None);
static USED: Mutex<Option<Instant>> = Mutex::new(None); // last vault activity, for locking it again on its own

fn touch() {
    *USED.lock().unwrap() = Some(Instant::now());
}

fn folder(app: &AppHandle) -> PathBuf {
    let st = app.state::<AppState>();
    let cfg = st.cfg.lock().unwrap();
    match cfg["vault_dir"].as_str().filter(|s| !s.is_empty()) {
        Some(d) => PathBuf::from(d),
        None => dirs::document_dir().or_else(|| dirs::home_dir().map(|h| h.join("Documents"))).unwrap_or_default().join("Swiftlane Vault"),
    }
}

fn vault_box(app: &AppHandle, touched: bool) -> Arc<Vault> {
    let folder = folder(app);
    let v = {
        let mut slot = VAULT.lock().unwrap();
        match slot.as_ref() {
            Some(v) if v.folder == folder => v.clone(),
            _ => {
                if let Some(old) = slot.take() {
                    old.lock();
                }
                let v = Arc::new(Vault::new(folder, temp_root()));
                *slot = Some(v.clone());
                v
            }
        }
    };
    if touched {
        touch();
    }
    v
}

fn state(app: &AppHandle) -> Value {
    let v = vault_box(app, false);
    let minutes = app.state::<AppState>().cfg.lock().unwrap()["vault_lock_minutes"].as_u64().unwrap_or(5);
    json!({
        "folder": v.folder.to_string_lossy(),
        "exists": v.exists(),
        "unlocked": v.unlocked(),
        "items": v.items().unwrap_or_default(),
        "lock_minutes": minutes,
    })
}

fn error(e: Error) -> Value {
    json!({ "error": e.to_string() })
}

fn open_box(app: &AppHandle) -> Res<Arc<Vault>> {
    let v = vault_box(app, true);
    if !v.unlocked() {
        return Err(msg("The vault is locked."));
    }
    Ok(v)
}

fn strings(v: &Value) -> Vec<String> {
    v.as_array()
        .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from).or_else(|| x.as_u64().map(|n| n.to_string()))).collect())
        .unwrap_or_default()
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

fn job_error(e: Error) -> JobError {
    match e {
        Error::Cancelled => JobError::Cancelled,
        other => JobError::Failed(other.to_string()),
    }
}

pub fn dispatch(app: &AppHandle, method: &str, args: &[Value]) -> Option<Value> {
    let s = |i: usize| arg(args, i).as_str().unwrap_or("").to_string();
    Some(match method {
        "vault_state" => state(app),
        "vault_create" => {
            if s(0) != s(1) {
                return Some(json!({ "error": "The two passwords don't match." }));
            }
            match vault_box(app, true).create(&s(0)) {
                Ok(()) => {
                    config::log("Vault created");
                    state(app)
                }
                Err(e) => error(e),
            }
        }
        "vault_unlock" => match vault_box(app, true).unlock(&s(0)) {
            Ok(()) => state(app),
            Err(Error::WrongPassword(m)) => {
                config::log("Vault: wrong password");
                std::thread::sleep(Duration::from_secs(1)); // slows down guessing through the window
                json!({ "error": m })
            }
            Err(e) => error(e),
        },
        "vault_lock" => {
            if let Some(v) = VAULT.lock().unwrap().clone() {
                v.lock();
            }
            state(app)
        }
        "vault_choose_folder" => choose_folder(app),
        "vault_add" => add(app, arg(args, 0), arg(args, 1).as_bool().unwrap_or(false)),
        "vault_export" => export(app, strings(arg(args, 0))),
        "vault_open" => match open_box(app).and_then(|v| v.open_copy(&s(0))) {
            Ok(path) => json!(crate::winapi::shell_open(&path)),
            Err(e) => error(e),
        },
        "vault_remove" => match open_box(app).and_then(|v| v.remove(&strings(arg(args, 0)))) {
            Ok(()) => state(app),
            Err(e) => error(e),
        },
        "vault_rename" => match open_box(app).and_then(|v| v.rename(&s(0), &s(1))) {
            Ok(()) => state(app),
            Err(e) => error(e),
        },
        "vault_password" => {
            if s(1) != s(2) {
                return Some(json!({ "error": "The two new passwords don't match." }));
            }
            match vault_box(app, true).change_password(&s(0), &s(1)) {
                Ok(()) => {
                    config::log("Vault password changed");
                    json!(true)
                }
                Err(Error::WrongPassword(m)) => {
                    std::thread::sleep(Duration::from_secs(1));
                    json!({ "error": m })
                }
                Err(e) => error(e),
            }
        }
        _ => return None,
    })
}

/// Open a vault somewhere else (e.g. on a USB stick), or pick where a new one goes.
fn choose_folder(app: &AppHandle) -> Value {
    let current = vault_box(app, false).folder.clone();
    let start = current.parent().filter(|p| p.exists()).map(Path::to_path_buf).or_else(dirs::home_dir).unwrap_or_default();
    let Some(mut chosen) = super::ask_folder(app, &start) else { return Value::Null };
    if !chosen.join("vault.json").exists() {
        chosen = chosen.join("Swiftlane Vault"); // never mix vault files into one of your own folders
    }
    if let Some(v) = VAULT.lock().unwrap().clone() {
        v.lock();
    }
    {
        let st = app.state::<AppState>();
        let mut cfg = st.cfg.lock().unwrap();
        cfg["vault_dir"] = json!(chosen.to_string_lossy());
        config::save(&cfg);
    }
    state(app)
}

fn add(app: &AppHandle, paths: &Value, shred: bool) -> Value {
    let v = match open_box(app) {
        Ok(v) => v,
        Err(e) => return error(e),
    };
    let mut paths: Vec<PathBuf> = paths.as_array().map(|a| a.iter().filter_map(|p| p.as_str()).map(PathBuf::from).collect()).unwrap_or_default();
    if paths.is_empty() {
        use tauri_plugin_dialog::DialogExt;
        let Some(picked) = app.dialog().file().blocking_pick_files() else { return Value::Null };
        paths = picked.into_iter().filter_map(|p| p.into_path().ok()).collect();
    }
    paths.retain(|p| p.is_file());
    if paths.is_empty() {
        return json!({ "error": "Only files can go into the vault (not folders)." });
    }
    let n = paths.len();
    let title = format!("Lock {n} file{} in the vault", plural(n));
    let id = app.state::<AppState>().jobs.start(app, "vault", &title, false, move |ctx| {
        let r = v.add(
            &paths,
            |f| {
                touch(); // don't lock in the middle of a big file
                ctx.report(Some(f), None);
            },
            || ctx.cancelled(),
            shred,
        );
        push(ctx.app(), "vault", state(ctx.app()));
        r.map(|_| Vec::new()).map_err(job_error)
    });
    json!(id)
}

fn export(app: &AppHandle, ids: Vec<String>) -> Value {
    let v = match open_box(app) {
        Ok(v) => v,
        Err(e) => return error(e),
    };
    let Some(folder) = super::ask_folder(app, &super::save_dir(app)) else { return Value::Null };
    let n = ids.len();
    let title = format!("Take {n} file{} out of the vault", plural(n));
    let id = app.state::<AppState>().jobs.start(app, "vault", &title, true, move |ctx| {
        v.export(&ids, &folder, &|| {
            touch();
            ctx.cancelled()
        })
        .map_err(job_error)
    });
    json!(id)
}

/// Wipes copies left by a crash, then locks the vault after a few quiet minutes.
pub fn start(app: &AppHandle) {
    let app = app.clone();
    std::thread::spawn(move || {
        wipe_leftovers(&temp_root());
        loop {
            std::thread::sleep(Duration::from_secs(15));
            let Some(v) = VAULT.lock().unwrap().clone() else { continue };
            let minutes = app.state::<AppState>().cfg.lock().unwrap()["vault_lock_minutes"].as_u64().unwrap_or(5);
            let idle = USED.lock().unwrap().map_or(Duration::MAX, |t| t.elapsed());
            if v.unlocked() && idle > Duration::from_secs(60 * minutes) {
                v.lock();
                config::log("Vault locked (not used for a while)");
                push(&app, "vault", state(&app));
            }
        }
    });
}

/// Swiftlane is closing: lock the vault (this also wipes files opened from it).
pub fn shutdown() {
    if let Some(v) = VAULT.lock().unwrap().clone() {
        v.lock();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    /// A fresh, empty folder for one test; removed again by `done`.
    fn fresh() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("swl-vault-test-{}", hex::encode(random::<6>())));
        std::fs::create_dir(&dir).unwrap();
        dir
    }

    fn done(dir: &Path) {
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Test files: empty, small, exactly one chunk, and several chunks with a short last one.
    fn samples(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
        std::fs::create_dir(dir).unwrap();
        let pattern = |n: usize| (0..n).map(|i| (i * 7 + i / 1000) as u8).collect::<Vec<u8>>();
        let files = [("empty.txt", Vec::new()), ("hello é.txt", "hello vault".as_bytes().to_vec()), ("one chunk.bin", pattern(CHUNK)), ("big.bin", pattern(2 * CHUNK + 12345))];
        files
            .into_iter()
            .map(|(name, data)| {
                let p = dir.join(name);
                std::fs::write(&p, &data).unwrap();
                (p, data)
            })
            .collect()
    }

    #[test]
    fn round_trip_and_protections() {
        let dir = fresh();
        let v = Vault::new(dir.join("vault"), dir.join("temp"));
        assert!(matches!(v.create("short"), Err(Error::Msg(_))));
        assert!(matches!(v.create("aaaaaaaaaa"), Err(Error::Msg(_))), "too few different characters");
        v.create("correct horse").unwrap();
        let files = samples(&dir.join("in"));
        let paths: Vec<PathBuf> = files.iter().map(|f| f.0.clone()).collect();
        let ids = v.add(&paths, |_| {}, || false, false).unwrap();
        assert_eq!(v.items().unwrap().len(), 4);

        // Opening: wrong password, then right one; contents come back exactly.
        let v2 = Vault::new(dir.join("vault"), dir.join("temp"));
        assert!(matches!(v2.unlock("wrong password"), Err(Error::WrongPassword(_))));
        v2.unlock("correct horse").unwrap();
        let out = dir.join("out");
        std::fs::create_dir(&out).unwrap();
        let exported = v2.export(&ids, &out, &|| false).unwrap();
        for ((_, data), path) in files.iter().zip(&exported) {
            assert_eq!(&std::fs::read(path).unwrap(), data);
        }
        assert_eq!(exported[1].file_name().unwrap(), "hello é.txt");
        // Exporting again never replaces: "name (2).ext".
        let again = v2.export(&ids[1..2], &out, &|| false).unwrap();
        assert_eq!(again[0].file_name().unwrap(), "hello é (2).txt");

        // A changed byte, a swapped file or a cut-off file is refused.
        let blob = v2.blob(&ids[3]).unwrap();
        let original = std::fs::read(&blob).unwrap();
        let mut changed = original.clone();
        changed[20] ^= 1;
        std::fs::write(&blob, &changed).unwrap();
        assert!(v2.export(&ids[3..4], &out, &|| false).is_err());
        std::fs::write(&blob, &original[..12 + CHUNK + TAG]).unwrap();
        assert!(v2.export(&ids[3..4], &out, &|| false).is_err(), "cut off after a whole chunk");
        std::fs::copy(v2.blob(&ids[2]).unwrap(), &blob).unwrap();
        assert!(v2.export(&ids[3..4], &out, &|| false).is_err(), "another file's data");
        std::fs::write(&blob, &original).unwrap();

        // Open a copy, then locking wipes it.
        let copy = v2.open_copy(&ids[1]).unwrap();
        assert!(copy.starts_with(dir.join("temp")));
        assert_eq!(std::fs::read(&copy).unwrap(), b"hello vault");
        v2.lock();
        assert!(!dir.join("temp").exists());
        assert!(v2.items().is_err());

        // Password change keeps the files as they are.
        let before = std::fs::read(&blob).unwrap();
        assert!(matches!(v2.change_password("nope nope", "another one"), Err(Error::WrongPassword(_))));
        v2.change_password("correct horse", "battery staple").unwrap();
        assert_eq!(std::fs::read(&blob).unwrap(), before);
        assert!(v2.unlock("correct horse").is_err());
        v2.unlock("battery staple").unwrap();
        v2.rename(&ids[0], "..\\evil/../x:y.txt").unwrap();
        assert_eq!(v2.items().unwrap().iter().find(|i| i["id"] == ids[0]).unwrap()["name"], "x_y.txt");
        v2.remove(&ids[..1]).unwrap();
        assert!(!v2.blob(&ids[0]).unwrap().exists());
        assert_eq!(v2.items().unwrap().len(), 3);

        // A header weakened to guess faster is refused.
        let mut h: Value = serde_json::from_slice(&std::fs::read(dir.join("vault/vault.json")).unwrap()).unwrap();
        h["kdf"]["n"] = json!(1024);
        std::fs::write(dir.join("vault/vault.json"), h.to_string()).unwrap();
        assert!(matches!(Vault::new(dir.join("vault"), dir.join("temp")).unlock("battery staple"), Err(Error::Msg(_))));
        done(&dir);
    }

    #[test]
    fn locking_in_with_delete_originals() {
        let dir = fresh();
        let v = Vault::new(dir.join("vault"), dir.join("temp"));
        v.create("correct horse").unwrap();
        let files = samples(&dir.join("in"));
        let ids = v.add(&[files[1].0.clone(), files[3].0.clone()], |_| {}, || false, true).unwrap();
        assert!(!files[1].0.exists() && !files[3].0.exists(), "originals are gone");
        let out = dir.join("out");
        std::fs::create_dir(&out).unwrap();
        let back = v.export(&ids, &out, &|| false).unwrap();
        assert_eq!(std::fs::read(&back[1]).unwrap(), files[3].1);
        // Cancelling leaves nothing half-written behind.
        assert!(matches!(v.add(&[files[2].0.clone()], |_| {}, || true, false), Err(Error::Cancelled)));
        assert_eq!(std::fs::read_dir(dir.join("vault/files")).unwrap().count(), 2);
        assert!(files[2].0.exists());
        done(&dir);
    }

    #[test]
    fn names() {
        assert_eq!(safe_name("C:\\Users\\me\\a<b>.txt"), "a_b_.txt");
        assert_eq!(safe_name(" .. "), "file");
        assert_eq!(safe_name("dir/"), "dir");
        assert_eq!(safe_name("C:name.txt"), "name.txt");
        assert_eq!(safe_name("trailing. "), "trailing");
        assert!(check_password("abcdefgh").is_none());
        assert!(check_password("abababab").is_some());
    }

    // ------------------------------------------------------------ compatibility with the Python version

    /// Runs Python with Source/toolkit importable. None if Python or `cryptography` isn't there.
    fn python(script: &str, args: &[&Path]) -> Option<String> {
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("Source");
        let mut cmd = Command::new("python3");
        // -B: never writes __pycache__ into the source tree
        cmd.arg("-B").arg("-c").arg(format!("import sys; sys.path.insert(0, sys.argv[1])\n{script}")).arg(&source);
        for a in args {
            cmd.arg(a);
        }
        let out = cmd.env("PYTHONDONTWRITEBYTECODE", "1").output().ok()?;
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr);
            if err.contains("No module named 'cryptography'") {
                return None;
            }
            panic!("Python failed: {err}");
        }
        Some(String::from_utf8_lossy(&out.stdout).to_string())
    }

    fn python_ready() -> bool {
        // The Python version now lives in the archived swiftlane-deprecated repo; these checks
        // only run when its vault.py is put back next to the sources.
        let vault_py = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("Source").join("toolkit").join("vault.py");
        if !vault_py.is_file() {
            eprintln!("skipped: Source/toolkit/vault.py (Python version) isn't here");
            return false;
        }
        let ok =Command::new("python3").args(["-B", "-c", "import cryptography"]).output().is_ok_and(|o| o.status.success());
        if !ok {
            eprintln!("skipped: python3 with the cryptography package isn't available");
        }
        ok
    }

    #[test]
    fn opens_vaults_made_by_python() {
        if !python_ready() {
            return;
        }
        let dir = fresh();
        let files = samples(&dir.join("in"));
        let script = r#"
from pathlib import Path
from toolkit import vault
v = vault.Vault(sys.argv[2])
v.create("made in python")
ins = sorted(Path(sys.argv[3]).iterdir())
ids = v.add([str(p) for p in ins])
for p, i in zip(ins, ids):
    print(p.name + "\t" + i)
"#;
        let out = python(script, &[&dir.join("vault"), &dir.join("in")]).expect("python");
        let v = Vault::new(dir.join("vault"), dir.join("temp"));
        assert!(matches!(v.unlock("made in rust"), Err(Error::WrongPassword(_))));
        v.unlock("made in python").unwrap();
        assert_eq!(v.items().unwrap().len(), files.len());
        let target = dir.join("out");
        std::fs::create_dir(&target).unwrap();
        for line in out.lines() {
            let (name, id) = line.split_once('\t').unwrap();
            let data = &files.iter().find(|f| f.0.file_name().unwrap() == name).unwrap().1;
            let got = v.export(&[id.to_string()], &target, &|| false).unwrap();
            assert_eq!(got[0].file_name().unwrap(), name);
            assert_eq!(&std::fs::read(&got[0]).unwrap(), data, "{name}");
        }
        // Rust changes the password and adds a file; Python still opens it all.
        v.change_password("made in python", "changed in rust").unwrap();
        let extra = dir.join("extra.txt");
        std::fs::write(&extra, "added by rust").unwrap();
        let new_id = v.add(&[extra], |_| {}, || false, false).unwrap().remove(0);
        let check = r#"
from toolkit import vault
v = vault.Vault(sys.argv[2])
v.unlock("changed in rust")
print(len(v.items()))
print(v.export([sys.argv[4]], sys.argv[3])[0].read_text())
"#;
        let target2 = dir.join("out2");
        std::fs::create_dir(&target2).unwrap();
        let out = python(check, &[&dir.join("vault"), &target2, Path::new(&new_id)]).unwrap();
        assert_eq!(out.trim().replace("\r\n", "\n"), format!("{}\nadded by rust", files.len() + 1));
        done(&dir);
    }

    #[test]
    fn python_opens_vaults_made_by_rust() {
        if !python_ready() {
            return;
        }
        let dir = fresh();
        let files = samples(&dir.join("in"));
        let v = Vault::new(dir.join("vault"), dir.join("temp"));
        v.create("made in rust").unwrap();
        let paths: Vec<PathBuf> = files.iter().map(|f| f.0.clone()).collect();
        v.add(&paths, |_| {}, || false, false).unwrap();
        v.lock();
        let script = r#"
from toolkit import vault
v = vault.Vault(sys.argv[2])
try:
    v.unlock("made in python")
    print("opened with the wrong password")
except vault.WrongPassword:
    pass
v.unlock("made in rust")
out = v.export([i["id"] for i in v.items()], sys.argv[3])
print(len(out))
v.change_password("made in rust", "changed in python")
"#;
        let out_dir = dir.join("out");
        std::fs::create_dir(&out_dir).unwrap();
        let out = python(script, &[&dir.join("vault"), &out_dir]).expect("python");
        assert_eq!(out.trim(), files.len().to_string());
        for (path, data) in &files {
            assert_eq!(&std::fs::read(out_dir.join(path.file_name().unwrap())).unwrap(), data);
        }
        // ...and the password Python set opens it in Rust.
        let v2 = Vault::new(dir.join("vault"), dir.join("temp"));
        v2.unlock("changed in python").unwrap();
        assert_eq!(v2.items().unwrap().len(), files.len());
        done(&dir);
    }
}
