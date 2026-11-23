//! Storage & Cleanup: native Rust port of DiskCleaner.ps1 for Swiftlane 2.0.
//!
//! Scans safe-to-clear caches, Downloads, and Steam libraries (across all configured Steam library
//! folders on any Windows PC), measures real on-disk sizes (respecting NTFS compression via
//! GetCompressedFileSizeW and never following symlinks or reparse points), and cleans only items
//! from the last scan: Downloads & leftover game folders go strictly to the Recycle Bin, while
//! allowlisted cache folders are emptied in-place with locked/in-use files cleanly skipped.

use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::UNIX_EPOCH;
use tauri::AppHandle;

use super::arg;
use super::diskmap::{drives, to_recycle_bin};
use crate::{config, winapi};

const STEAM_LEFTOVER_MIN_BYTES: u64 = 50 * 1024 * 1024;
const STEAMWORKS_REDIST_APPID: &str = "228980";

// ------------------------------------------------------------------ on-disk size & reparse guards

#[cfg(windows)]
#[link(name = "kernel32")]
extern "system" {
    fn GetCompressedFileSizeW(lpFileName: *const u16, lpFileSizeHigh: *mut u32) -> u32;
}

#[cfg(windows)]
fn file_size_on_disk(path: &Path, fallback: u64) -> u64 {
    use std::os::windows::ffi::OsStrExt;
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    let mut hi: u32 = 0;
    let lo = unsafe { GetCompressedFileSizeW(wide.as_ptr(), &mut hi) };
    if lo == u32::MAX {
        let err = unsafe { windows_sys::Win32::Foundation::GetLastError() };
        if err != 0 {
            return fallback;
        }
    }
    ((hi as u64) << 32) | (lo as u64)
}

#[cfg(not(windows))]
fn file_size_on_disk(_path: &Path, fallback: u64) -> u64 {
    fallback
}

/// True for symbolic links and Windows reparse points (junctions, mount points, etc.).
fn is_reparse_or_symlink(meta: &std::fs::Metadata) -> bool {
    if meta.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
        if meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return true;
        }
    }
    false
}

/// Measures a file or folder's on-disk size without following symlinks or Windows reparse points.
fn measure_path(path: &Path) -> u64 {
    let Ok(meta) = std::fs::symlink_metadata(path) else { return 0 };
    if is_reparse_or_symlink(&meta) {
        return 0;
    }
    if meta.is_file() {
        return file_size_on_disk(path, meta.len());
    }
    if !meta.is_dir() {
        return 0;
    }
    let mut total = 0u64;
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for entry in rd.flatten() {
            let Ok(m) = entry.metadata() else { continue };
            if is_reparse_or_symlink(&m) {
                continue;
            }
            let p = entry.path();
            if m.is_dir() {
                stack.push(p);
            } else if m.is_file() {
                total = total.saturating_add(file_size_on_disk(&p, m.len()));
            }
        }
    }
    total
}

/// Measures multiple path groups in parallel with a bounded scoped thread pool.
fn measure_paths_parallel(path_groups: &[Vec<PathBuf>]) -> Vec<u64> {
    if path_groups.is_empty() {
        return Vec::new();
    }
    let out: Vec<AtomicU64> = (0..path_groups.len()).map(|_| AtomicU64::new(0)).collect();
    let next = AtomicUsize::new(0);
    let workers = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .clamp(2, 8)
        .min(path_groups.len());

    std::thread::scope(|s| {
        for _ in 0..workers {
            s.spawn(|| loop {
                let idx = next.fetch_add(1, Ordering::Relaxed);
                if idx >= path_groups.len() {
                    break;
                }
                let sum = path_groups[idx]
                    .iter()
                    .fold(0u64, |acc, p| acc.saturating_add(measure_path(p)));
                out[idx].store(sum, Ordering::Relaxed);
            });
        }
    });
    out.into_iter().map(AtomicU64::into_inner).collect()
}

fn existing_non_reparse_dir(path: &Path) -> bool {
    std::fs::symlink_metadata(path)
        .map(|m| m.is_dir() && !is_reparse_or_symlink(&m))
        .unwrap_or(false)
}

fn modified_unix_secs(meta: &std::fs::Metadata) -> u64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn normalize_key(path: &Path) -> String {
    let mut s = path.to_string_lossy().replace('/', "\\");
    while s.len() > 3 && s.ends_with('\\') {
        s.pop();
    }
    if cfg!(windows) {
        s.to_ascii_lowercase()
    } else {
        path.to_string_lossy().trim_end_matches('/').to_string()
    }
}

fn drive_of(path: &Path) -> String {
    if let Some(Component::Prefix(prefix)) = path.components().next() {
        match prefix.kind() {
            std::path::Prefix::Disk(l) | std::path::Prefix::VerbatimDisk(l) => {
                return format!("{}:", (l as char).to_ascii_uppercase());
            }
            _ => {}
        }
    }
    let s = path.to_string_lossy();
    let bytes = s.as_bytes();
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        return format!("{}:", (bytes[0] as char).to_ascii_uppercase());
    }
    String::new()
}

// ------------------------------------------------------------------ Steam VDF & ACF parser

#[derive(Debug, Clone, PartialEq, Eq)]
struct SteamManifest {
    appid: String,
    name: String,
    installdir: String,
    size_on_disk: u64,
    last_played: u64,
}

#[derive(Debug, Clone)]
struct SteamGame {
    id: String,
    appid: String,
    name: String,
    size: u64,
    last_played: u64,
    drive: String,
    path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SteamLeftover {
    name: String,
    path: PathBuf,
    size: u64,
    has_userdata: bool,
    drive: String,
}

fn validate_steam_appid(appid: &str) -> bool {
    !appid.is_empty() && appid.bytes().all(|b| b.is_ascii_digit())
}

/// Reads a single Valve KeyValues (VDF/ACF) `"key" "value"` line, unescaping `\\` and `\"`.
fn parse_vdf_kv_line(line: &str) -> Option<(String, String)> {
    let mut chars = line.chars().peekable();
    let read_quoted = |it: &mut std::iter::Peekable<std::str::Chars<'_>>| -> Option<String> {
        while let Some(&c) = it.peek() {
            if c == '"' {
                it.next();
                break;
            }
            if c == '/' {
                return None; // comment
            }
            if !c.is_whitespace() {
                return None;
            }
            it.next();
        }
        let mut out = String::new();
        while let Some(c) = it.next() {
            if c == '\\' {
                match it.next() {
                    Some('\\') => out.push('\\'),
                    Some('"') => out.push('"'),
                    Some('n') => out.push('\n'),
                    Some('t') => out.push('\t'),
                    Some(other) => {
                        out.push('\\');
                        out.push(other);
                    }
                    None => return None,
                }
            } else if c == '"' {
                return Some(out);
            } else {
                out.push(c);
            }
        }
        None
    };

    let key = read_quoted(&mut chars)?;
    let val = read_quoted(&mut chars)?;
    Some((key, val))
}

/// Extracts all unique Steam library root folders from `libraryfolders.vdf`, always including `steam_root`.
fn parse_library_folders(vdf: &str, steam_root: &Path) -> Vec<PathBuf> {
    let mut libs = Vec::new();
    let mut seen = HashSet::new();

    let mut push_lib = |raw: &str| {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return;
        }
        let mut normalized = if cfg!(windows) {
            trimmed.replace('/', "\\")
        } else {
            trimmed.to_string()
        };
        while normalized.len() > 3 && (normalized.ends_with('\\') || normalized.ends_with('/')) {
            normalized.pop();
        }
        let p = PathBuf::from(&normalized);
        let key = normalize_key(&p);
        if !key.is_empty() && seen.insert(key) {
            libs.push(p);
        }
    };

    if !steam_root.as_os_str().is_empty() {
        push_lib(&steam_root.to_string_lossy());
    }

    for line in vdf.lines() {
        if let Some((k, v)) = parse_vdf_kv_line(line) {
            if k.eq_ignore_ascii_case("path") {
                push_lib(&v);
            }
        }
    }
    libs
}

/// Parses a Steam `appmanifest_<appid>.acf` file for its top-level game metadata.
fn parse_appmanifest(acf: &str) -> Option<SteamManifest> {
    let mut depth: usize = 0;
    let mut appid = String::new();
    let mut name = String::new();
    let mut installdir = String::new();
    let mut size_on_disk = 0u64;
    let mut last_played = 0u64;

    for line in acf.lines() {
        if depth <= 1 {
            if let Some((k, v)) = parse_vdf_kv_line(line) {
                match k.to_ascii_lowercase().as_str() {
                    "appid" => appid = v,
                    "name" => name = v,
                    "installdir" => installdir = v,
                    "sizeondisk" => size_on_disk = v.parse().unwrap_or(0),
                    "lastplayed" => last_played = v.parse().unwrap_or(0),
                    _ => {}
                }
            }
        }
        let mut in_quotes = false;
        let mut escaped = false;
        for c in line.chars() {
            if escaped {
                escaped = false;
                continue;
            }
            if c == '\\' && in_quotes {
                escaped = true;
                continue;
            }
            if c == '"' {
                in_quotes = !in_quotes;
                continue;
            }
            if !in_quotes {
                if c == '{' {
                    depth = depth.saturating_add(1);
                } else if c == '}' {
                    depth = depth.saturating_sub(1);
                }
            }
        }
    }

    if !validate_steam_appid(&appid) || name.is_empty() || installdir.is_empty() {
        return None;
    }
    Some(SteamManifest { appid, name, installdir, size_on_disk, last_played })
}

#[cfg(windows)]
fn steam_root_path() -> PathBuf {
    use winreg::enums::{HKEY_CURRENT_USER, KEY_READ};
    use winreg::RegKey;
    if let Ok(key) = RegKey::predef(HKEY_CURRENT_USER).open_subkey_with_flags(r"Software\Valve\Steam", KEY_READ) {
        if let Ok(val) = key.get_value::<String, _>("SteamPath") {
            let clean = val.trim().trim_matches('"').replace('/', "\\");
            if !clean.is_empty() {
                return PathBuf::from(clean);
            }
        }
    }
    PathBuf::from(r"C:\Program Files (x86)\Steam")
}

#[cfg(not(windows))]
fn steam_root_path() -> PathBuf {
    dirs::home_dir()
        .map(|h| h.join(".local").join("share").join("Steam"))
        .unwrap_or_default()
}

/// Finds leftover directories inside `steamapps/common` that do not match any installed manifest.
fn find_steam_leftovers(common_dir: &Path, installed_dirs: &HashSet<String>, min_bytes: u64) -> Vec<SteamLeftover> {
    let Ok(rd) = std::fs::read_dir(common_dir) else { return Vec::new() };
    let mut candidates: Vec<(String, PathBuf)> = Vec::new();
    for entry in rd.flatten() {
        let Ok(meta) = entry.metadata() else { continue };
        if !meta.is_dir() || is_reparse_or_symlink(&meta) {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        let lower = name.to_ascii_lowercase();
        if installed_dirs.contains(&lower)
            || lower == "steam controller configs"
            || lower == "steamworks shared"
        {
            continue;
        }
        candidates.push((name, entry.path()));
    }
    if candidates.is_empty() {
        return Vec::new();
    }

    let groups: Vec<Vec<PathBuf>> = candidates.iter().map(|(_, p)| vec![p.clone()]).collect();
    let sizes = measure_paths_parallel(&groups);
    let drive = drive_of(common_dir);

    let mut out = Vec::new();
    for ((name, path), size) in candidates.into_iter().zip(sizes) {
        if size < min_bytes {
            continue;
        }
        let has_userdata = path.join("UserData").exists();
        out.push(SteamLeftover { name, path, size, has_userdata, drive: drive.clone() });
    }
    out.sort_by(|a, b| b.size.cmp(&a.size).then_with(|| a.name.to_ascii_lowercase().cmp(&b.name.to_ascii_lowercase())));
    out
}

fn scan_steam() -> (Vec<SteamGame>, Vec<SteamLeftover>) {
    let root = steam_root_path();
    let vdf_path = root.join("steamapps").join("libraryfolders.vdf");
    let vdf = std::fs::read_to_string(&vdf_path).unwrap_or_default();
    let libraries = parse_library_folders(&vdf, &root);

    let mut games: Vec<SteamGame> = Vec::new();
    let mut seen_appids: HashSet<String> = HashSet::new();
    let mut leftovers: Vec<SteamLeftover> = Vec::new();

    for lib in libraries {
        let steamapps = lib.join("steamapps");
        if !existing_non_reparse_dir(&steamapps) {
            continue;
        }
        let drive = drive_of(&lib);
        let mut installed_dirs: HashSet<String> = HashSet::new();

        if let Ok(rd) = std::fs::read_dir(&steamapps) {
            for entry in rd.flatten() {
                let fname = entry.file_name().to_string_lossy().to_string();
                let lower = fname.to_ascii_lowercase();
                if !lower.starts_with("appmanifest_") || !lower.ends_with(".acf") {
                    continue;
                }
                let Ok(content) = std::fs::read_to_string(entry.path()) else { continue };
                let Some(m) = parse_appmanifest(&content) else { continue };
                installed_dirs.insert(m.installdir.to_ascii_lowercase());
                if m.appid == STEAMWORKS_REDIST_APPID {
                    continue;
                }
                if !seen_appids.insert(m.appid.clone()) {
                    continue;
                }
                let game_path = steamapps.join("common").join(&m.installdir);
                games.push(SteamGame {
                    id: format!("game-{}", m.appid),
                    appid: m.appid,
                    name: m.name,
                    size: m.size_on_disk,
                    last_played: m.last_played,
                    drive: drive.clone(),
                    path: game_path,
                });
            }
        }

        let common = steamapps.join("common");
        if existing_non_reparse_dir(&common) {
            leftovers.extend(find_steam_leftovers(&common, &installed_dirs, STEAM_LEFTOVER_MIN_BYTES));
        }
    }

    // Measure any game whose manifest reported 0 bytes on disk
    let unmeasured_idx: Vec<usize> = games
        .iter()
        .enumerate()
        .filter(|(_, g)| g.size == 0 && existing_non_reparse_dir(&g.path))
        .map(|(i, _)| i)
        .collect();
    if !unmeasured_idx.is_empty() {
        let groups: Vec<Vec<PathBuf>> = unmeasured_idx.iter().map(|&i| vec![games[i].path.clone()]).collect();
        let measured = measure_paths_parallel(&groups);
        for (i, sz) in unmeasured_idx.into_iter().zip(measured) {
            games[i].size = sz;
        }
    }

    games.sort_by(|a, b| b.size.cmp(&a.size).then_with(|| a.name.to_ascii_lowercase().cmp(&b.name.to_ascii_lowercase())));
    leftovers.sort_by(|a, b| b.size.cmp(&a.size).then_with(|| a.name.to_ascii_lowercase().cmp(&b.name.to_ascii_lowercase())));
    (games, leftovers)
}

// ------------------------------------------------------------------ Quick Clean definitions & allowlist

fn local_app_data() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(dirs::data_local_dir)
}

struct CacheSpec {
    id: &'static str,
    group: &'static str,
    name: &'static str,
    detail: &'static str,
    paths: Vec<PathBuf>,
}

fn build_cache_specs() -> Vec<CacheSpec> {
    let Some(local) = local_app_data() else { return Vec::new() };
    vec![
        CacheSpec {
            id: "temp",
            group: "safe",
            name: "Temporary files",
            detail: "Leftovers from installs and updates. Files currently in use are skipped.",
            paths: vec![local.join("Temp")],
        },
        CacheSpec {
            id: "shaders",
            group: "safe",
            name: "GPU shader caches",
            detail: "Games rebuild these automatically; first launch may briefly stutter.",
            paths: vec![
                local.join("NVIDIA").join("DXCache"),
                local.join("NVIDIA").join("GLCache"),
                local.join("D3DSCache"),
                local.join("AMD").join("DxCache"),
                local.join("AMD").join("GLCache"),
                local.join("Intel").join("ShaderCache"),
            ],
        },
        CacheSpec {
            id: "crash",
            group: "safe",
            name: "Crash dumps",
            detail: "Memory dumps from apps that crashed.",
            paths: vec![local.join("CrashDumps")],
        },
        CacheSpec {
            id: "spotify",
            group: "consider",
            name: "Spotify cache",
            detail: "Cached music for faster startup. Close Spotify first.",
            paths: vec![local.join("Spotify").join("Data")],
        },
        CacheSpec {
            id: "browsers",
            group: "consider",
            name: "Browser caches",
            detail: "Web caches that rebuild automatically. Close browsers first.",
            paths: vec![
                local.join("Microsoft").join("Edge").join("User Data").join("Default").join("Cache").join("Cache_Data"),
                local.join("Microsoft").join("Edge").join("User Data").join("Default").join("Code Cache"),
                local.join("Google").join("Chrome").join("User Data").join("Default").join("Cache").join("Cache_Data"),
                local.join("Google").join("Chrome").join("User Data").join("Default").join("Code Cache"),
                local.join("BraveSoftware").join("Brave-Browser").join("User Data").join("Default").join("Cache").join("Cache_Data"),
                local.join("BraveSoftware").join("Brave-Browser").join("User Data").join("Default").join("Code Cache"),
            ],
        },
    ]
}

fn is_allowed_cache_dir(path: &Path) -> bool {
    let target = normalize_key(path);
    if target.is_empty() {
        return false;
    }
    build_cache_specs()
        .into_iter()
        .flat_map(|s| s.paths)
        .any(|allowed| normalize_key(&allowed) == target)
}

/// Guard against ever emptying a drive root, Windows system directory, or top-level user folder.
fn is_protected_cache_root(path: &Path) -> bool {
    if path.as_os_str().is_empty() || !path.is_absolute() || path.parent().is_none() {
        return true;
    }
    let norm = normalize_key(path);
    if norm.len() <= 3 {
        return true;
    }
    let mut blocked: Vec<String> = [
        r"c:\windows",
        r"c:\windows\system32",
        r"c:\windows\winsxs",
        r"c:\windows\installer",
        r"c:\program files",
        r"c:\program files (x86)",
        r"c:\programdata",
        r"c:\users",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();

    if let Some(home) = dirs::home_dir() {
        blocked.push(normalize_key(&home));
        for sub in ["AppData", r"AppData\Local", r"AppData\Roaming", "Documents", "Desktop", "Downloads", "OneDrive"] {
            blocked.push(normalize_key(&home.join(sub)));
        }
    }
    if blocked.iter().any(|b| b == &norm) {
        return true;
    }
    if norm.starts_with(r"c:\windows\") && norm != r"c:\windows\temp" && !norm.starts_with(r"c:\windows\temp\") {
        return true;
    }
    false
}

#[derive(Default, Debug)]
struct EmptyStats {
    freed: u64,
    failed: u64,
    first_error: Option<String>,
}

impl EmptyStats {
    fn fail(&mut self, err: impl ToString) {
        self.failed += 1;
        if self.first_error.is_none() {
            self.first_error = Some(err.to_string());
        }
    }
}

fn clear_readonly(path: &Path) {
    if let Ok(meta) = std::fs::symlink_metadata(path) {
        let mut perms = meta.permissions();
        if perms.readonly() {
            perms.set_readonly(false);
            let _ = std::fs::set_permissions(path, perms);
        }
    }
}

/// Empties all regular files & subdirectories inside `root` without removing `root` itself.
/// Skips symlinks, Windows reparse points, and locked/in-use files cleanly.
fn empty_cache_dir_inner(root: &Path) -> Result<EmptyStats, String> {
    if is_protected_cache_root(root) {
        return Err("Refused to empty a protected system or user directory.".into());
    }
    let meta = std::fs::symlink_metadata(root).map_err(|e| e.to_string())?;
    if is_reparse_or_symlink(&meta) {
        return Err("Refused to empty a symbolic link or reparse point.".into());
    }
    if !meta.is_dir() {
        return Err("Target cache path is not a directory.".into());
    }

    fn walk_delete(dir: &Path, remove_self: bool, stats: &mut EmptyStats) {
        let entries = match std::fs::read_dir(dir) {
            Ok(rd) => rd,
            Err(e) => {
                stats.fail(e);
                return;
            }
        };
        for entry in entries.flatten() {
            let child = entry.path();
            let Ok(m) = std::fs::symlink_metadata(&child) else { continue };
            if is_reparse_or_symlink(&m) {
                continue;
            }
            if m.is_dir() {
                walk_delete(&child, true, stats);
            } else if m.is_file() {
                let sz = file_size_on_disk(&child, m.len());
                clear_readonly(&child);
                match std::fs::remove_file(&child) {
                    Ok(()) => stats.freed = stats.freed.saturating_add(sz),
                    Err(e) => stats.fail(e),
                }
            }
        }
        if remove_self {
            clear_readonly(dir);
            let _ = std::fs::remove_dir(dir);
        }
    }

    let mut stats = EmptyStats::default();
    walk_delete(root, false, &mut stats);
    Ok(stats)
}

fn empty_cache_dir(root: &Path) -> Result<EmptyStats, String> {
    if !is_allowed_cache_dir(root) {
        return Err("Refused: directory is not an allowlisted cache folder.".into());
    }
    empty_cache_dir_inner(root)
}

// ------------------------------------------------------------------ Downloads classifier & scan

fn classify_download(name: &str, is_dir: bool) -> &'static str {
    if is_dir {
        return "folder";
    }
    let ext = Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "zip" | "rar" | "7z" | "tar" | "gz" | "xz" => "archive",
        "exe" | "msi" | "msix" | "iso" => "installer",
        _ => "file",
    }
}

struct DownloadEntry {
    id: String,
    name: String,
    path: PathBuf,
    size: u64,
    kind: &'static str,
    modified: u64,
}

fn scan_downloads() -> Vec<DownloadEntry> {
    let Some(dl_dir) = dirs::download_dir() else { return Vec::new() };
    let Ok(rd) = std::fs::read_dir(&dl_dir) else { return Vec::new() };

    let mut raw: Vec<(String, PathBuf, bool, u64)> = Vec::new();
    for entry in rd.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.eq_ignore_ascii_case("desktop.ini") {
            continue;
        }
        let path = entry.path();
        let Ok(meta) = std::fs::symlink_metadata(&path) else { continue };
        let is_dir = meta.is_dir() && !is_reparse_or_symlink(&meta);
        let modified = modified_unix_secs(&meta);
        raw.push((name, path, is_dir, modified));
    }

    let groups: Vec<Vec<PathBuf>> = raw.iter().map(|(_, p, _, _)| vec![p.clone()]).collect();
    let sizes = measure_paths_parallel(&groups);

    let mut out: Vec<DownloadEntry> = raw
        .into_iter()
        .zip(sizes)
        .map(|((name, path, is_dir, modified), size)| {
            let kind = classify_download(&name, is_dir);
            let id = format!("dl-{name}");
            DownloadEntry { id, name, path, size, kind, modified }
        })
        .collect();

    out.sort_by(|a, b| b.size.cmp(&a.size).then_with(|| a.name.to_ascii_lowercase().cmp(&b.name.to_ascii_lowercase())));
    out
}

// ------------------------------------------------------------------ System drive stats

fn system_drive_colon() -> String {
    #[cfg(windows)]
    {
        if let Ok(sd) = std::env::var("SystemDrive") {
            let trimmed = sd.trim().trim_end_matches('\\');
            if trimmed.len() == 2 && trimmed.as_bytes()[0].is_ascii_alphabetic() && trimmed.as_bytes()[1] == b':' {
                return trimmed.to_ascii_uppercase();
            }
        }
        "C:".to_string()
    }
    #[cfg(not(windows))]
    {
        "/".to_string()
    }
}

fn system_drive_stats() -> Value {
    let target_root = if cfg!(windows) {
        format!("{}\\", system_drive_colon())
    } else {
        "/".to_string()
    };
    let all = drives();
    let picked = all.as_array().and_then(|arr| {
        arr.iter()
            .find(|d| d["path"].as_str().is_some_and(|p| p.eq_ignore_ascii_case(&target_root)))
            .or_else(|| arr.first())
    });
    match picked {
        Some(d) => json!({
            "path": d["path"].as_str().unwrap_or(&target_root),
            "total": d["total"].as_u64().unwrap_or(0),
            "used": d["used"].as_u64().unwrap_or(0),
            "free": d["free"].as_u64().unwrap_or(0),
        }),
        None => json!({
            "path": target_root,
            "total": 0,
            "used": 0,
            "free": 0,
        }),
    }
}

// ------------------------------------------------------------------ Snapshot state & actions

#[derive(Clone)]
enum OpenAction {
    CleanMgr,
    MsApps,
    MsStorage,
}

#[derive(Clone)]
enum StoredKind {
    EmptyCache,
    Recycle,
    Open(OpenAction),
    Game { appid: String },
}

#[derive(Clone)]
struct StoredItem {
    name: String,
    kind: StoredKind,
    paths: Vec<PathBuf>,
    reveal_path: Option<PathBuf>,
}

struct CleanSnapshot {
    items: HashMap<String, StoredItem>,
}

static CLEAN_STATE: Mutex<Option<CleanSnapshot>> = Mutex::new(None);

fn clean_scan() -> Value {
    let drive = system_drive_stats();

    // Filter quick cache specs to existing non-reparse directories
    let mut active_caches: Vec<CacheSpec> = Vec::new();
    for mut spec in build_cache_specs() {
        spec.paths.retain(|p| existing_non_reparse_dir(p));
        if !spec.paths.is_empty() {
            active_caches.push(spec);
        }
    }

    // Measure quick cache items, Steam libraries, and Downloads concurrently
    let (cache_sizes, (games, leftovers), downloads) = std::thread::scope(|s| {
        let h_caches = s.spawn(|| {
            let groups: Vec<Vec<PathBuf>> = active_caches.iter().map(|c| c.paths.clone()).collect();
            measure_paths_parallel(&groups)
        });
        let h_steam = s.spawn(scan_steam);
        let h_dl = s.spawn(scan_downloads);

        (
            h_caches.join().unwrap_or_default(),
            h_steam.join().unwrap_or_default(),
            h_dl.join().unwrap_or_default(),
        )
    });

    let mut stored: HashMap<String, StoredItem> = HashMap::new();
    let mut quick_json: Vec<Value> = Vec::new();

    // Safe cache items first
    let mut consider_caches: Vec<(CacheSpec, u64)> = Vec::new();
    for (spec, size) in active_caches.into_iter().zip(cache_sizes) {
        if spec.group == "consider" {
            consider_caches.push((spec, size));
            continue;
        }
        let first_path = spec.paths.first().cloned();
        let path_strs: Vec<String> = spec.paths.iter().map(|p| p.to_string_lossy().to_string()).collect();
        quick_json.push(json!({
            "id": spec.id,
            "group": spec.group,
            "name": spec.name,
            "kind": "empty_cache",
            "size": size,
            "detail": spec.detail,
            "path": first_path.as_ref().map(|p| p.to_string_lossy().to_string()),
            "paths": path_strs,
        }));
        stored.insert(
            spec.id.to_string(),
            StoredItem {
                name: spec.name.to_string(),
                kind: StoredKind::EmptyCache,
                paths: spec.paths,
                reveal_path: first_path,
            },
        );
    }

    // Leftover Steam game folders in "safe" group (sent to Recycle Bin)
    let mut leftover_ids: HashSet<String> = HashSet::new();
    for (idx, lf) in leftovers.into_iter().enumerate() {
        let mut id = format!("steamleft-{}", lf.name);
        if !leftover_ids.insert(id.clone()) {
            id = format!("steamleft-{idx}-{}", lf.name);
            leftover_ids.insert(id.clone());
        }
        let drive_label = if lf.drive.is_empty() { "this drive" } else { &lf.drive };
        let mut detail = format!("Steam no longer lists this game as installed on {drive_label}, so this folder is left over.");
        if lf.has_userdata {
            detail.push_str(" It has a UserData folder (saves/setups) — copy it somewhere first if you want to keep those.");
        }
        let display_name = format!("Leftover game folder: {}", lf.name);
        let p_str = lf.path.to_string_lossy().to_string();
        quick_json.push(json!({
            "id": id,
            "group": "safe",
            "name": display_name,
            "kind": "recycle",
            "size": lf.size,
            "detail": detail,
            "path": p_str,
            "paths": [p_str],
        }));
        stored.insert(
            id,
            StoredItem {
                name: display_name,
                kind: StoredKind::Recycle,
                paths: vec![lf.path.clone()],
                reveal_path: Some(lf.path),
            },
        );
    }

    // "Consider clearing" cache items
    for (spec, size) in consider_caches {
        let first_path = spec.paths.first().cloned();
        let path_strs: Vec<String> = spec.paths.iter().map(|p| p.to_string_lossy().to_string()).collect();
        quick_json.push(json!({
            "id": spec.id,
            "group": spec.group,
            "name": spec.name,
            "kind": "empty_cache",
            "size": size,
            "detail": spec.detail,
            "path": first_path.as_ref().map(|p| p.to_string_lossy().to_string()),
            "paths": path_strs,
        }));
        stored.insert(
            spec.id.to_string(),
            StoredItem {
                name: spec.name.to_string(),
                kind: StoredKind::EmptyCache,
                paths: spec.paths,
                reveal_path: first_path,
            },
        );
    }

    // "Consider clearing" built-in Windows open actions
    let open_items: &[(&str, &str, &str, &str, &str, OpenAction)] = &[
        (
            "cleanmgr",
            "Windows Disk Cleanup",
            "cleanmgr",
            "Open Disk Cleanup",
            "Safely clear Windows Update & upgrade leftovers via Windows' own tool.",
            OpenAction::CleanMgr,
        ),
        (
            "apps",
            "Installed apps",
            "ms_apps",
            "Open Installed apps",
            "Uninstall large apps and games cleanly in Windows Settings.",
            OpenAction::MsApps,
        ),
        (
            "storage_sense",
            "Windows Storage Sense",
            "ms_storage",
            "Open Storage Sense",
            "Configure automatic temporary file cleanup in Windows.",
            OpenAction::MsStorage,
        ),
    ];
    for (id, name, action, button, detail, act) in open_items {
        quick_json.push(json!({
            "id": id,
            "group": "consider",
            "name": name,
            "kind": "open",
            "action": action,
            "button": button,
            "size": 0,
            "detail": detail,
            "path": Value::Null,
            "paths": [],
        }));
        stored.insert(
            (*id).to_string(),
            StoredItem {
                name: (*name).to_string(),
                kind: StoredKind::Open(act.clone()),
                paths: Vec::new(),
                reveal_path: None,
            },
        );
    }

    // Downloads JSON + state
    let mut downloads_json: Vec<Value> = Vec::with_capacity(downloads.len());
    for dl in downloads {
        let p_str = dl.path.to_string_lossy().to_string();
        downloads_json.push(json!({
            "id": dl.id,
            "name": dl.name,
            "path": p_str,
            "size": dl.size,
            "kind": dl.kind,
            "modified": dl.modified,
        }));
        stored.insert(
            dl.id,
            StoredItem {
                name: dl.name,
                kind: StoredKind::Recycle,
                paths: vec![dl.path.clone()],
                reveal_path: Some(dl.path),
            },
        );
    }

    // Games JSON + state
    let mut games_json: Vec<Value> = Vec::with_capacity(games.len());
    for g in games {
        let p_str = g.path.to_string_lossy().to_string();
        games_json.push(json!({
            "id": g.id,
            "appid": g.appid,
            "name": g.name,
            "size": g.size,
            "last_played": g.last_played,
            "drive": g.drive,
            "path": p_str,
            "store": "Steam",
        }));
        stored.insert(
            g.id,
            StoredItem {
                name: g.name,
                kind: StoredKind::Game { appid: g.appid },
                paths: Vec::new(),
                reveal_path: Some(g.path),
            },
        );
    }

    *CLEAN_STATE.lock().unwrap() = Some(CleanSnapshot { items: stored });

    json!({
        "drive": drive,
        "quick": quick_json,
        "downloads": downloads_json,
        "games": games_json,
    })
}

fn clean_run(ids: &[String]) -> Value {
    if ids.is_empty() {
        return json!({ "error": "Nothing selected." });
    }

    // Validate EVERY requested id against CLEAN_STATE before touching disk.
    let work: Vec<(String, StoredItem)> = {
        let guard = CLEAN_STATE.lock().unwrap();
        let Some(snap) = guard.as_ref() else {
            return json!({ "error": "Scan first before cleaning." });
        };
        let mut seen = HashSet::new();
        let mut selected = Vec::new();
        for id in ids {
            if !seen.insert(id.as_str()) {
                continue;
            }
            let Some(item) = snap.items.get(id) else {
                return json!({ "error": format!("Unknown item id: {id}") });
            };
            match item.kind {
                StoredKind::Recycle | StoredKind::EmptyCache => selected.push((id.clone(), item.clone())),
                StoredKind::Open(_) | StoredKind::Game { .. } => {
                    return json!({ "error": format!("Item cannot be cleaned directly: {id}") });
                }
            }
        }
        selected
    };

    let mut freed = 0u64;
    let mut failed = 0u64;
    let mut first_error: Option<String> = None;

    for (id, item) in work {
        match item.kind {
            StoredKind::Recycle => {
                for p in &item.paths {
                    let before = measure_path(p);
                    match to_recycle_bin(p) {
                        Ok(()) => {
                            freed = freed.saturating_add(before);
                            config::log(&format!("Cleaner: moved to Recycle Bin ({id}): {}", p.display()));
                        }
                        Err(e) => {
                            failed += 1;
                            if first_error.is_none() {
                                first_error = Some(e.clone());
                            }
                            config::log(&format!("Cleaner: failed to recycle ({id}) {}: {e}", p.display()));
                        }
                    }
                }
            }
            StoredKind::EmptyCache => {
                for p in &item.paths {
                    match empty_cache_dir(p) {
                        Ok(stats) => {
                            freed = freed.saturating_add(stats.freed);
                            failed = failed.saturating_add(stats.failed);
                            if first_error.is_none() {
                                first_error = stats.first_error;
                            }
                            config::log(&format!(
                                "Cleaner: emptied cache ({id} · {}): {} freed {} B, {} skipped",
                                item.name,
                                p.display(),
                                stats.freed,
                                stats.failed
                            ));
                        }
                        Err(e) => {
                            failed += 1;
                            if first_error.is_none() {
                                first_error = Some(e.clone());
                            }
                            config::log(&format!("Cleaner: refused/failed cache ({id}) {}: {e}", p.display()));
                        }
                    }
                }
            }
            StoredKind::Open(_) | StoredKind::Game { .. } => {}
        }
    }

    let scan = clean_scan();
    json!({
        "freed": freed,
        "failed": failed,
        "first_error": first_error,
        "scan": scan,
    })
}

#[cfg(windows)]
fn open_allowlisted_uri(uri: &str) -> bool {
    use std::os::windows::ffi::OsStrExt;
    let verb: Vec<u16> = "open".encode_utf16().chain(std::iter::once(0)).collect();
    let target: Vec<u16> = std::ffi::OsStr::new(uri).encode_wide().chain(std::iter::once(0)).collect();
    let r = unsafe {
        windows_sys::Win32::UI::Shell::ShellExecuteW(
            std::ptr::null_mut(),
            verb.as_ptr(),
            target.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            1,
        )
    };
    r as usize > 32
}

#[cfg(not(windows))]
fn open_allowlisted_uri(_uri: &str) -> bool {
    false
}

fn clean_open(id: &str, which: &str) -> Value {
    let item = {
        let guard = CLEAN_STATE.lock().unwrap();
        guard.as_ref().and_then(|s| s.items.get(id).cloned())
    };
    let Some(item) = item else {
        return json!({ "ok": false, "error": "Unknown item." });
    };

    let ok = match item.kind {
        StoredKind::Open(OpenAction::CleanMgr) => {
            let drive = system_drive_colon();
            config::log(&format!("Cleaner: opening cleanmgr.exe /d {drive}"));
            std::process::Command::new("cleanmgr.exe").args(["/d", &drive]).spawn().is_ok()
        }
        StoredKind::Open(OpenAction::MsApps) => {
            config::log("Cleaner: opening ms-settings:appsfeatures");
            open_allowlisted_uri("ms-settings:appsfeatures")
        }
        StoredKind::Open(OpenAction::MsStorage) => {
            config::log("Cleaner: opening ms-settings:storagesense");
            open_allowlisted_uri("ms-settings:storagesense")
        }
        StoredKind::Game { ref appid } => {
            if !validate_steam_appid(appid) {
                return json!({ "ok": false, "error": "Invalid Steam app ID." });
            }
            let uri = if which == "secondary" {
                format!("steam://nav/games/details/{appid}")
            } else {
                format!("steam://uninstall/{appid}")
            };
            config::log(&format!("Cleaner: opening {uri}"));
            open_allowlisted_uri(&uri)
        }
        _ => return json!({ "ok": false, "error": "Item has no external action." }),
    };

    json!({ "ok": ok })
}

fn clean_reveal(id: &str) -> Value {
    let path = {
        let guard = CLEAN_STATE.lock().unwrap();
        guard
            .as_ref()
            .and_then(|s| s.items.get(id))
            .and_then(|item| item.reveal_path.clone())
    };
    let Some(path) = path else {
        return json!({ "ok": false });
    };
    let ok = path.symlink_metadata().is_ok() && winapi::reveal(&path);
    json!({ "ok": ok })
}

pub fn dispatch(_app: &AppHandle, method: &str, args: &[Value]) -> Option<Value> {
    Some(match method {
        "clean_scan" => clean_scan(),
        "clean_run" => {
            let ids: Vec<String> = arg(args, 0)
                .as_array()
                .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                .unwrap_or_default();
            clean_run(&ids)
        }
        "clean_open" => {
            let id = arg(args, 0).as_str().unwrap_or("");
            let which = arg(args, 1).as_str().unwrap_or("primary");
            clean_open(id, which)
        }
        "clean_reveal" => {
            let id = arg(args, 0).as_str().unwrap_or("");
            clean_reveal(id)
        }
        _ => return None,
    })
}

// ------------------------------------------------------------------ tests

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh_dir(tag: &str) -> PathBuf {
        let mut b = [0u8; 6];
        getrandom::getrandom(&mut b).unwrap();
        let dir = std::env::temp_dir().join(format!("swl-clean-{tag}-{}", hex::encode(b)));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn parses_library_folders_vdf() {
        let vdf = r#"
"libraryfolders"
{
	"0"
	{
		"path"		"C:\\Program Files (x86)\\Steam"
		"label"		""
		"apps"
		{
			"730"		"45000000000"
		}
	}
	"1"
	{
		"path"		"D:\\SteamLibrary"
		"label"		"Games"
	}
	"2"
	{
		"path"		"d:\\steamlibrary\\"
		"label"		"Duplicate"
	}
	"3"
	{
		"path"		"E:\\Games\\Steam"
	}
}
"#;
        let root = Path::new(r"C:\Program Files (x86)\Steam");
        let libs = parse_library_folders(vdf, root);
        assert_eq!(libs.len(), 3, "expected deduplicated libraries, got: {libs:?}");
        assert_eq!(libs[0], PathBuf::from(r"C:\Program Files (x86)\Steam"));
        assert_eq!(libs[1], PathBuf::from(r"D:\SteamLibrary"));
        assert_eq!(libs[2], PathBuf::from(r"E:\Games\Steam"));
    }

    #[test]
    fn parses_appmanifest_acf() {
        let acf = r#"
"AppState"
{
	"appid"		"730"
	"Universe"		"1"
	"name"		"Counter-Strike 2"
	"StateFlags"		"4"
	"installdir"		"Counter-Strike Global Offensive"
	"LastUpdated"		"1710000000"
	"SizeOnDisk"		"45123456789"
	"LastPlayed"		"1712345678"
	"InstalledDepots"
	{
		"731"
		{
			"name"		"should-not-overwrite"
			"manifest"		"12345"
		}
	}
}
"#;
        let m = parse_appmanifest(acf).expect("valid acf should parse");
        assert_eq!(m.appid, "730");
        assert_eq!(m.name, "Counter-Strike 2");
        assert_eq!(m.installdir, "Counter-Strike Global Offensive");
        assert_eq!(m.size_on_disk, 45_123_456_789);
        assert_eq!(m.last_played, 1_712_345_678);

        // Non-numeric appid must be rejected
        let bad = "\"AppState\"\n{\n\"appid\" \"730/../bad\"\n\"name\" \"Bad\"\n\"installdir\" \"Bad\"\n}\n";
        assert!(parse_appmanifest(bad).is_none());
        assert!(validate_steam_appid("730"));
        assert!(!validate_steam_appid(""));
        assert!(!validate_steam_appid("730/evil"));
    }

    #[test]
    fn detects_leftover_steam_folders() {
        let dir = fresh_dir("steam");
        let common = dir.join("common");
        std::fs::create_dir_all(common.join("InstalledGame")).unwrap();
        std::fs::write(common.join("InstalledGame/data.bin"), vec![1u8; 4096]).unwrap();

        std::fs::create_dir_all(common.join("Steamworks Shared")).unwrap();
        std::fs::write(common.join("Steamworks Shared/redist.bin"), vec![1u8; 4096]).unwrap();

        std::fs::create_dir_all(common.join("Steam Controller Configs")).unwrap();
        std::fs::write(common.join("Steam Controller Configs/cfg.bin"), vec![1u8; 4096]).unwrap();

        std::fs::create_dir_all(common.join("TinyLeftover")).unwrap();
        std::fs::write(common.join("TinyLeftover/small.txt"), b"tiny").unwrap();

        std::fs::create_dir_all(common.join("OldGame/UserData")).unwrap();
        std::fs::write(common.join("OldGame/pak.bin"), vec![2u8; 4096]).unwrap();

        let mut installed = HashSet::new();
        installed.insert("installedgame".to_string());

        let leftovers = find_steam_leftovers(&common, &installed, 1024);
        assert_eq!(leftovers.len(), 1);
        assert_eq!(leftovers[0].name, "OldGame");
        assert!(leftovers[0].has_userdata);
        assert!(leftovers[0].size >= 4096);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn classifies_download_kinds() {
        assert_eq!(classify_download("my-folder", true), "folder");
        for name in ["a.zip", "b.RAR", "c.7z", "d.tar", "e.gz", "f.xz"] {
            assert_eq!(classify_download(name, false), "archive", "failed for {name}");
        }
        for name in ["setup.exe", "pkg.MSI", "app.msix", "win.iso"] {
            assert_eq!(classify_download(name, false), "installer", "failed for {name}");
        }
        for name in ["report.pdf", "photo.png", "notes", "archive.zip.part"] {
            assert_eq!(classify_download(name, false), "file", "failed for {name}");
        }
    }

    #[test]
    fn empties_cache_safely_and_rejects_unknown_ids() {
        let dir = fresh_dir("cache");
        let sub = dir.join("nested");
        std::fs::create_dir_all(&sub).unwrap();
        let f1 = dir.join("a.tmp");
        let f2 = sub.join("b.tmp");
        std::fs::write(&f1, vec![7u8; 2048]).unwrap();
        std::fs::write(&f2, vec![8u8; 1024]).unwrap();

        // Mark one file read-only to confirm cache cleaner handles it
        let mut perms = std::fs::metadata(&f2).unwrap().permissions();
        perms.set_readonly(true);
        std::fs::set_permissions(&f2, perms).unwrap();

        // Optional symlink outside target to verify symlinks are skipped, never followed
        let outside = fresh_dir("outside");
        let sentinel = outside.join("keep.txt");
        std::fs::write(&sentinel, b"do not delete").unwrap();
        #[cfg(unix)]
        let _ = std::os::unix::fs::symlink(&outside, dir.join("link_out"));
        #[cfg(windows)]
        let _ = std::os::windows::fs::symlink_dir(&outside, dir.join("link_out"));

        let stats = empty_cache_dir_inner(&dir).expect("should empty temp cache dir");
        assert!(dir.is_dir(), "cache root directory must be preserved");
        assert!(!f1.exists(), "top-level cache file should be removed");
        assert!(!f2.exists(), "nested read-only cache file should be removed");
        assert!(!sub.exists(), "empty nested cache dir should be removed");
        assert!(stats.freed >= 3072, "expected freed >= 3072, got {}", stats.freed);
        assert!(sentinel.exists(), "symlink target must never be touched");

        // Protected roots must always be refused
        assert!(is_protected_cache_root(Path::new(r"C:\")));
        assert!(is_protected_cache_root(Path::new(r"C:\Windows")));
        if let Some(home) = dirs::home_dir() {
            assert!(is_protected_cache_root(&home));
            assert!(is_protected_cache_root(&home.join("Downloads")));
        }

        // Non-allowlisted folder must be refused by empty_cache_dir
        assert!(empty_cache_dir(&dir).is_err());

        // Unknown IDs must be rejected by clean_run
        {
            let mut guard = CLEAN_STATE.lock().unwrap();
            *guard = Some(CleanSnapshot { items: HashMap::new() });
        }
        let res = clean_run(&["does-not-exist".to_string()]);
        assert!(res["error"].as_str().is_some(), "expected error for unknown ID, got {res}");

        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&outside);
    }
}
