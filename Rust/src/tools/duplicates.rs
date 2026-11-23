//! Duplicate file finder: scans a drive or folder for byte-identical files using a parallel
//! multi-stage pipeline (walk -> group by size -> parallel 4 KiB head+tail partial SHA-256 ->
//! parallel full 256 KiB-chunked SHA-256).
//!
//! Safety rules:
//! - Skips symlinks and Windows reparse points (junctions, mount points, cloud placeholders) so
//!   walks never loop or hydrate online-only files.
//! - Skips protected system folders during the walk and never offers deletion on protected paths.
//! - Only paths from the current scan may ever be revealed or moved to the Recycle Bin.
//! - Deletions go to the Recycle Bin only (`diskmap::to_recycle_bin`), never permanent delete.

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant, UNIX_EPOCH};
use tauri::{AppHandle, Manager};

use super::diskmap::{drives, protected, to_recycle_bin};
use super::{arg, ask_folder};
use crate::api::{push, AppState};
use crate::config;
use crate::jobs::JobError;

const PARTIAL_BYTES: usize = 4 * 1024; // 4 KiB head + 4 KiB tail
const FULL_CHUNK: usize = 256 * 1024; // 256 KiB full-hash read buffer
const MAX_THREADS: usize = 8;
const PROGRESS_INTERVAL: Duration = Duration::from_millis(80);

#[cfg(windows)]
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;

#[derive(Debug)]
pub enum ScanError {
    Cancelled,
}

struct FileEntry {
    path: PathBuf,
    size: u64,
    modified: u64,
}

struct DupFile {
    path: String,
    name: String,
    dir: String,
    modified: u64,
    protected: bool,
}

struct DupGroup {
    id: usize,
    sha256: String,
    size: u64,
    files: Vec<DupFile>,
}

impl DupGroup {
    fn reclaimable(&self) -> u64 {
        self.size.saturating_mul(self.files.len().saturating_sub(1) as u64)
    }

    fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "sha256": self.sha256,
            "size": self.size,
            "reclaimable": self.reclaimable(),
            "files": self.files.iter().map(|f| json!({
                "path": f.path,
                "name": f.name,
                "dir": f.dir,
                "modified": f.modified,
                "protected": f.protected,
            })).collect::<Vec<_>>(),
        })
    }
}

pub struct DupResult {
    root: String,
    scanned_files: u64,
    groups: Vec<DupGroup>,
    known_paths: HashSet<String>,
}

impl DupResult {
    fn in_scan(&self, path: &str) -> bool {
        !path.is_empty() && self.known_paths.contains(path)
    }

    fn remove_paths(&mut self, removed: &HashSet<String>) {
        if removed.is_empty() {
            return;
        }
        for g in &mut self.groups {
            g.files.retain(|f| !removed.contains(&f.path));
        }
        self.groups.retain(|g| g.files.len() >= 2);
        self.groups.sort_by(|a, b| {
            b.reclaimable()
                .cmp(&a.reclaimable())
                .then_with(|| b.size.cmp(&a.size))
                .then_with(|| a.sha256.cmp(&b.sha256))
        });
        self.known_paths.clear();
        for g in &self.groups {
            for f in &g.files {
                self.known_paths.insert(f.path.clone());
            }
        }
    }

    fn to_json(&self) -> Value {
        let total_groups = self.groups.len();
        let extra_files: usize = self.groups.iter().map(|g| g.files.len().saturating_sub(1)).sum();
        let reclaimable_bytes: u64 = self.groups.iter().map(DupGroup::reclaimable).sum();
        json!({
            "root": self.root,
            "scanned_files": self.scanned_files,
            "groups": self.groups.iter().map(DupGroup::to_json).collect::<Vec<_>>(),
            "total_groups": total_groups,
            "extra_files": extra_files,
            "reclaimable_bytes": reclaimable_bytes,
        })
    }
}

static DUPS: Mutex<Option<DupResult>> = Mutex::new(None);

// ------------------------------------------------------------------ walk safety helpers

#[cfg(windows)]
fn is_reparse_point(meta: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(windows))]
fn is_reparse_point(_meta: &std::fs::Metadata) -> bool {
    false
}

fn norm_parts(p: &Path) -> Vec<String> {
    let abs = std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf());
    let s = abs.to_string_lossy();
    let clean = if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        PathBuf::from(format!(r"\\{rest}"))
    } else if let Some(rest) = s.strip_prefix(r"\\?\") {
        PathBuf::from(rest)
    } else {
        abs
    };
    clean
        .components()
        .map(|c| {
            let part = c.as_os_str().to_string_lossy().to_string();
            if cfg!(windows) { part.to_lowercase() } else { part }
        })
        .collect()
}

fn resolve_parts(p: &Path) -> Vec<String> {
    let abs = std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf());
    let real = std::fs::canonicalize(&abs).unwrap_or(abs);
    norm_parts(&real)
}

/// Fast precomputed check for system directories to skip during the walk, backed by
/// `diskmap::protected`. Allows descending through `C:\Users`, the user's home folder,
/// and user folders like `Documents`/`Downloads`, while skipping Windows, Program Files,
/// ProgramData, AppData, `$Recycle.Bin`, and `System Volume Information`.
struct WalkGuard {
    temp: Option<Vec<String>>,
    skip_trees: Vec<Vec<String>>,
}

impl WalkGuard {
    fn new() -> Self {
        let env = |name: &str| std::env::var_os(name).filter(|v| !v.is_empty()).map(PathBuf::from);
        let temp = env("TEMP").map(|p| resolve_parts(&p));
        let defaults: &[(&str, &str)] = &[
            ("SystemRoot", r"C:\Windows"),
            ("ProgramFiles", r"C:\Program Files"),
            ("ProgramFiles(x86)", r"C:\Program Files (x86)"),
            ("ProgramData", r"C:\ProgramData"),
            ("APPDATA", ""),
            ("LOCALAPPDATA", ""),
        ];
        let mut skip_trees = Vec::new();
        for (name, default) in defaults {
            let guarded = env(name).or_else(|| (cfg!(windows) && !default.is_empty()).then(|| PathBuf::from(default)));
            if let Some(g) = guarded {
                skip_trees.push(resolve_parts(&g));
            }
        }
        if let Some(home) = dirs::home_dir() {
            skip_trees.push(resolve_parts(&home.join("AppData")));
        }
        WalkGuard { temp, skip_trees }
    }

    fn should_skip_dir(&self, path: &Path) -> bool {
        let name = path.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
        if matches!(name.as_str(), "$recycle.bin" | "system volume information") {
            return true;
        }
        let p = norm_parts(path);
        if let Some(temp) = &self.temp {
            if p.len() > temp.len() && p.starts_with(temp) {
                return false;
            }
        }
        for tree in &self.skip_trees {
            if p.len() >= tree.len() && p.starts_with(tree) {
                return protected(path);
            }
        }
        false
    }
}

// ------------------------------------------------------------------ hashing & progress helpers

struct ProgressGate<'a, F: Fn(Option<f64>, &str) + Sync> {
    report: &'a F,
    last: Mutex<Option<Instant>>,
}

impl<'a, F: Fn(Option<f64>, &str) + Sync> ProgressGate<'a, F> {
    fn new(report: &'a F) -> Self {
        ProgressGate {
            report,
            last: Mutex::new(None),
        }
    }

    fn emit(&self, force: bool, build: impl FnOnce() -> (Option<f64>, String)) {
        let now = Instant::now();
        {
            let mut guard = self.last.lock().unwrap();
            if !force && guard.is_some_and(|prev| now.duration_since(prev) < PROGRESS_INTERVAL) {
                return;
            }
            *guard = Some(now);
        }
        let (frac, msg) = build();
        (self.report)(frac, &msg);
    }
}

fn worker_count(items: usize) -> usize {
    let cpus = thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
    cpus.clamp(1, MAX_THREADS).min(items.max(1))
}

/// Computes either the full SHA-256 (for files `<= 8 KiB`) or the 4 KiB head + 4 KiB tail
/// partial SHA-256 (for files `> 8 KiB`). Returns `None` if the file cannot be read or changed
/// size mid-read.
fn partial_hash(path: &Path, size: u64, buf: &mut [u8; PARTIAL_BYTES * 2]) -> Option<[u8; 32]> {
    let mut file = File::open(path).ok()?;
    if size <= (PARTIAL_BYTES * 2) as u64 {
        let want = size as usize;
        let mut total = 0usize;
        while total < want {
            match file.read(&mut buf[total..want]) {
                Ok(0) => return None,
                Ok(n) => total += n,
                Err(_) => return None,
            }
        }
        let mut extra = [0u8; 1];
        if matches!(file.read(&mut extra), Ok(1..)) {
            return None;
        }
        let mut hasher = Sha256::new();
        hasher.update(&buf[..want]);
        return Some(hasher.finalize().into());
    }

    let (head, tail) = buf.split_at_mut(PARTIAL_BYTES);
    file.read_exact(head).ok()?;
    file.seek(SeekFrom::Start(size - PARTIAL_BYTES as u64)).ok()?;
    file.read_exact(tail).ok()?;
    let mut hasher = Sha256::new();
    hasher.update(head);
    hasher.update(tail);
    Some(hasher.finalize().into())
}

/// Computes the full SHA-256 in 256 KiB chunks, checking `cancelled()` inside the read loop.
fn full_hash(
    path: &Path,
    expected_size: u64,
    buf: &mut [u8],
    cancelled: &(impl Fn() -> bool + Sync),
) -> Result<Option<[u8; 32]>, ScanError> {
    let mut file = match File::open(path) {
        Ok(f) => f,
        Err(_) => return Ok(None),
    };
    let mut hasher = Sha256::new();
    let mut total = 0u64;
    loop {
        if cancelled() {
            return Err(ScanError::Cancelled);
        }
        let n = match file.read(buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(_) => return Ok(None),
        };
        hasher.update(&buf[..n]);
        total += n as u64;
        if total > expected_size {
            return Ok(None);
        }
    }
    if total != expected_size {
        return Ok(None);
    }
    Ok(Some(hasher.finalize().into()))
}

/// 1234567 -> "1,234,567"
fn grouped(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn tail_str(s: &str, n: usize) -> String {
    let count = s.chars().count();
    s.chars().skip(count.saturating_sub(n)).collect()
}

// ------------------------------------------------------------------ 3-stage parallel pipeline

pub fn scan(
    root: &Path,
    min_bytes: u64,
    progress: &(impl Fn(Option<f64>, &str) + Sync),
    cancelled: &(impl Fn() -> bool + Sync),
) -> Result<DupResult, ScanError> {
    let min_size = min_bytes.max(1);
    let root_clean: PathBuf = root.components().collect();
    let root_str = root_clean.to_string_lossy().to_string();
    let guard = WalkGuard::new();
    let gate = ProgressGate::new(progress);

    // Stage 1: iterative folder walk, collecting (path, size, modified_unix)
    let mut stack = vec![root_clean];
    let mut by_size: HashMap<u64, Vec<FileEntry>> = HashMap::new();
    let mut scanned_files = 0u64;

    while let Some(dir) = stack.pop() {
        if cancelled() {
            return Err(ScanError::Cancelled);
        }
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for (idx, entry) in rd.enumerate() {
            if idx & 0x1ff == 0 && cancelled() {
                return Err(ScanError::Cancelled);
            }
            let Ok(entry) = entry else { continue };
            let Ok(ft) = entry.file_type() else { continue };
            if ft.is_symlink() {
                continue;
            }
            let Ok(meta) = entry.metadata() else { continue };
            if is_reparse_point(&meta) {
                continue;
            }
            let path = entry.path();
            if ft.is_dir() {
                if !guard.should_skip_dir(&path) {
                    stack.push(path);
                }
                continue;
            }
            if !ft.is_file() {
                continue;
            }
            scanned_files += 1;
            let size = meta.len();
            if size == 0 || size < min_size {
                continue;
            }
            let modified = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);
            by_size.entry(size).or_default().push(FileEntry { path, size, modified });
        }
        gate.emit(false, || {
            (
                None,
                format!("Scanning… {} files · {}", grouped(scanned_files), tail_str(&dir.to_string_lossy(), 60)),
            )
        });
    }

    if cancelled() {
        return Err(ScanError::Cancelled);
    }

    // Stage 2: discard unique sizes
    let mut candidates: Vec<FileEntry> = Vec::new();
    for (_size, mut list) in by_size {
        if list.len() >= 2 {
            candidates.append(&mut list);
        }
    }

    if candidates.is_empty() {
        return Ok(DupResult {
            root: root_str,
            scanned_files,
            groups: Vec::new(),
            known_paths: HashSet::new(),
        });
    }

    // Stage 3: parallel partial SHA-256 (4 KiB head + 4 KiB tail; files <= 8 KiB hashed completely)
    let total_stage3 = candidates.len();
    gate.emit(true, || {
        (Some(0.10), format!("Quick-checking {} candidates…", grouped(total_stage3 as u64)))
    });

    let next3 = AtomicUsize::new(0);
    let done3 = AtomicUsize::new(0);
    let workers3 = worker_count(total_stage3);
    let stage3_chunks: Vec<Vec<(usize, [u8; 32])>> = thread::scope(|s| -> Result<_, ScanError> {
        let handles: Vec<_> = (0..workers3)
            .map(|_| {
                s.spawn(|| -> Result<Vec<(usize, [u8; 32])>, ScanError> {
                    let mut out = Vec::new();
                    let mut buf = [0u8; PARTIAL_BYTES * 2];
                    loop {
                        if cancelled() {
                            return Err(ScanError::Cancelled);
                        }
                        let i = next3.fetch_add(1, Ordering::Relaxed);
                        if i >= total_stage3 {
                            break;
                        }
                        let f = &candidates[i];
                        if let Some(h) = partial_hash(&f.path, f.size, &mut buf) {
                            out.push((i, h));
                        }
                        let d = done3.fetch_add(1, Ordering::Relaxed) + 1;
                        gate.emit(false, || {
                            let frac = 0.10 + 0.35 * (d as f64 / total_stage3 as f64);
                            (
                                Some(frac),
                                format!("Quick-checking {} of {} candidates…", grouped(d as u64), grouped(total_stage3 as u64)),
                            )
                        });
                    }
                    Ok(out)
                })
            })
            .collect();
        let mut all = Vec::with_capacity(handles.len());
        for h in handles {
            all.push(h.join().unwrap_or(Ok(Vec::new()))?);
        }
        Ok(all)
    })?;

    if cancelled() {
        return Err(ScanError::Cancelled);
    }

    let mut by_partial: HashMap<(u64, [u8; 32]), Vec<usize>> = HashMap::new();
    for chunk in stage3_chunks {
        for (idx, hash) in chunk {
            by_partial.entry((candidates[idx].size, hash)).or_default().push(idx);
        }
    }

    let mut verified_buckets: Vec<(u64, [u8; 32], Vec<usize>)> = Vec::new();
    let mut stage4_indices: Vec<usize> = Vec::new();
    for ((size, hash), indices) in by_partial {
        if indices.len() < 2 {
            continue;
        }
        if size <= (PARTIAL_BYTES * 2) as u64 {
            // Already fully hashed in Stage 3; skip Stage 4!
            verified_buckets.push((size, hash, indices));
        } else {
            stage4_indices.extend(indices);
        }
    }

    // Stage 4: parallel full SHA-256 (256 KiB chunks) for large files with matching partial hashes
    if !stage4_indices.is_empty() {
        let total_stage4 = stage4_indices.len();
        gate.emit(true, || {
            (Some(0.45), format!("Verifying {} potential duplicates…", grouped(total_stage4 as u64)))
        });

        let next4 = AtomicUsize::new(0);
        let done4 = AtomicUsize::new(0);
        let workers4 = worker_count(total_stage4);
        let stage4_chunks: Vec<Vec<(usize, [u8; 32])>> = thread::scope(|s| -> Result<_, ScanError> {
            let handles: Vec<_> = (0..workers4)
                .map(|_| {
                    s.spawn(|| -> Result<Vec<(usize, [u8; 32])>, ScanError> {
                        let mut out = Vec::new();
                        let mut buf = vec![0u8; FULL_CHUNK];
                        loop {
                            if cancelled() {
                                return Err(ScanError::Cancelled);
                            }
                            let pos = next4.fetch_add(1, Ordering::Relaxed);
                            if pos >= total_stage4 {
                                break;
                            }
                            let idx = stage4_indices[pos];
                            let f = &candidates[idx];
                            if let Some(h) = full_hash(&f.path, f.size, &mut buf, cancelled)? {
                                out.push((idx, h));
                            }
                            let d = done4.fetch_add(1, Ordering::Relaxed) + 1;
                            gate.emit(false, || {
                                let frac = 0.45 + 0.50 * (d as f64 / total_stage4 as f64);
                                (
                                    Some(frac),
                                    format!("Verifying {} of {} potential duplicates…", grouped(d as u64), grouped(total_stage4 as u64)),
                                )
                            });
                        }
                        Ok(out)
                    })
                })
                .collect();
            let mut all = Vec::with_capacity(handles.len());
            for h in handles {
                all.push(h.join().unwrap_or(Ok(Vec::new()))?);
            }
            Ok(all)
        })?;

        if cancelled() {
            return Err(ScanError::Cancelled);
        }

        let mut by_full: HashMap<(u64, [u8; 32]), Vec<usize>> = HashMap::new();
        for chunk in stage4_chunks {
            for (idx, hash) in chunk {
                by_full.entry((candidates[idx].size, hash)).or_default().push(idx);
            }
        }
        for ((size, hash), indices) in by_full {
            if indices.len() >= 2 {
                verified_buckets.push((size, hash, indices));
            }
        }
    }

    // Build final groups sorted by reclaimable bytes descending
    let mut groups: Vec<DupGroup> = Vec::with_capacity(verified_buckets.len());
    let mut known_paths = HashSet::new();

    for (size, hash, indices) in verified_buckets {
        let mut files: Vec<DupFile> = indices
            .into_iter()
            .map(|idx| {
                let f = &candidates[idx];
                let path_str = f.path.to_string_lossy().to_string();
                let name = f.path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                let dir = f.path.parent().map(|p| p.to_string_lossy().to_string()).unwrap_or_default();
                let prot = protected(&f.path);
                known_paths.insert(path_str.clone());
                DupFile {
                    path: path_str,
                    name,
                    dir,
                    modified: f.modified,
                    protected: prot,
                }
            })
            .collect();
        files.sort_by(|a, b| b.modified.cmp(&a.modified).then_with(|| a.path.cmp(&b.path)));
        groups.push(DupGroup {
            id: 0,
            sha256: hex::encode(hash),
            size,
            files,
        });
    }

    groups.sort_by(|a, b| {
        b.reclaimable()
            .cmp(&a.reclaimable())
            .then_with(|| b.size.cmp(&a.size))
            .then_with(|| a.sha256.cmp(&b.sha256))
    });
    for (i, g) in groups.iter_mut().enumerate() {
        g.id = i + 1;
    }

    Ok(DupResult {
        root: root_str,
        scanned_files,
        groups,
        known_paths,
    })
}

// ------------------------------------------------------------------ the page's calls

fn scan_job(app: &AppHandle, path: &str, min_bytes: u64) -> Value {
    let root = PathBuf::from(path);
    if path.is_empty() || !root.is_dir() {
        return json!({ "error": "That folder doesn't exist." });
    }
    *DUPS.lock().unwrap() = None;
    let name = root
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| path.to_string());
    let id = app.state::<AppState>().jobs.start(
        app,
        "duplicates",
        &format!("Find duplicates in {name}"),
        false,
        move |ctx| {
            let res = scan(
                &root,
                min_bytes,
                &|frac, text| ctx.report(frac, Some(text)),
                &|| ctx.cancelled(),
            )
            .map_err(|ScanError::Cancelled| JobError::Cancelled)?;
            ctx.check()?;
            let view = res.to_json();
            *DUPS.lock().unwrap() = Some(res);
            push(ctx.app(), "duplicates", view);
            Ok(Vec::new())
        },
    );
    json!(id)
}

fn dup_view() -> Value {
    DUPS.lock().unwrap().as_ref().map(DupResult::to_json).unwrap_or(Value::Null)
}

fn dup_trash(paths_val: &Value) -> Value {
    let Some(arr) = paths_val.as_array() else {
        return json!({ "trashed": [], "errors": [], "view": dup_view() });
    };

    let mut unique_paths: Vec<String> = Vec::new();
    let mut seen = HashSet::new();
    for item in arr {
        if let Some(p) = item.as_str() {
            if !p.is_empty() && seen.insert(p.to_string()) {
                unique_paths.push(p.to_string());
            }
        }
    }

    let mut trashed: Vec<String> = Vec::new();
    let mut errors: Vec<Value> = Vec::new();

    for p in &unique_paths {
        let allowed = DUPS.lock().unwrap().as_ref().is_some_and(|d| d.in_scan(p));
        if !allowed {
            errors.push(json!({ "path": p, "error": "Not in the current scan. Scan again first." }));
            continue;
        }
        let path = Path::new(p);
        if protected(path) {
            errors.push(json!({
                "path": p,
                "error": "Swiftlane doesn't delete this: it belongs to Windows, a program, or your user folder.",
            }));
            continue;
        }
        match to_recycle_bin(path) {
            Ok(()) => {
                config::log(&format!("Duplicate finder: moved to the Recycle Bin: {p}"));
                trashed.push(p.clone());
            }
            Err(e) => {
                errors.push(json!({ "path": p, "error": e }));
            }
        }
    }

    let trashed_set: HashSet<String> = trashed.iter().cloned().collect();
    let view = {
        let mut dups = DUPS.lock().unwrap();
        if let Some(res) = dups.as_mut() {
            res.remove_paths(&trashed_set);
            res.to_json()
        } else {
            Value::Null
        }
    };

    json!({
        "trashed": trashed,
        "errors": errors,
        "view": view,
    })
}

pub fn dispatch(app: &AppHandle, method: &str, args: &[Value]) -> Option<Value> {
    Some(match method {
        "dup_drives" => drives(),
        "dup_pick" => {
            let home = dirs::home_dir().unwrap_or_default();
            ask_folder(app, &home).map(|p| json!(p.to_string_lossy())).unwrap_or(Value::Null)
        }
        "dup_scan" => {
            let path = arg(args, 0).as_str().unwrap_or("");
            let min_bytes = arg(args, 1).as_u64().unwrap_or(1).max(1);
            scan_job(app, path, min_bytes)
        }
        "dup_view" => dup_view(),
        "dup_reveal" => {
            let s = arg(args, 0).as_str().unwrap_or("");
            let ok = DUPS.lock().unwrap().as_ref().is_some_and(|d| d.in_scan(s))
                && Path::new(s).symlink_metadata().is_ok();
            if ok {
                crate::winapi::reveal(Path::new(s));
            }
            Value::Null
        }
        "dup_trash" => dup_trash(arg(args, 0)),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempDir(PathBuf);

    impl TempDir {
        fn fresh() -> Self {
            let mut b = [0u8; 6];
            getrandom::getrandom(&mut b).unwrap();
            let dir = std::env::temp_dir().join(format!("swl-dup-test-{}", hex::encode(b)));
            std::fs::create_dir(&dir).unwrap();
            TempDir(dir)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn groups_identical_small_and_large_files() {
        let tmp = TempDir::fresh();
        let dir = tmp.path();
        std::fs::create_dir_all(dir.join("a/b")).unwrap();

        // Small duplicates (<= 8 KiB, verified in stage 3 without stage 4)
        std::fs::write(dir.join("small1.txt"), b"hello duplicate world").unwrap();
        std::fs::write(dir.join("a/small2.txt"), b"hello duplicate world").unwrap();

        // Large duplicates (> 8 KiB, goes through stage 3 partial hash + stage 4 full hash)
        let big_payload = vec![0x5Au8; 32 * 1024];
        std::fs::write(dir.join("big1.bin"), &big_payload).unwrap();
        std::fs::write(dir.join("a/big2.bin"), &big_payload).unwrap();
        std::fs::write(dir.join("a/b/big3.bin"), &big_payload).unwrap();

        // Unique file
        std::fs::write(dir.join("unique.txt"), b"only one copy").unwrap();

        let res = scan(dir, 1, &|_, _| {}, &|| false).unwrap();
        let view = res.to_json();

        assert_eq!(view["scanned_files"], 6);
        assert_eq!(view["total_groups"], 2);
        assert_eq!(view["extra_files"], 3); // 2 extra big + 1 extra small
        assert_eq!(
            view["reclaimable_bytes"],
            (32 * 1024 * 2) as u64 + b"hello duplicate world".len() as u64
        );

        let groups = view["groups"].as_array().unwrap();
        // Sorted by reclaimable bytes descending: big group first, small group second
        assert_eq!(groups[0]["size"], (32 * 1024) as u64);
        assert_eq!(groups[0]["files"].as_array().unwrap().len(), 3);
        assert_eq!(groups[0]["reclaimable"], (32 * 1024 * 2) as u64);
        assert_eq!(groups[1]["size"], b"hello duplicate world".len() as u64);
        assert_eq!(groups[1]["files"].as_array().unwrap().len(), 2);

        // in_scan & remove_paths
        let big1 = dir.join("big1.bin").to_string_lossy().to_string();
        assert!(res.in_scan(&big1));
        assert!(!res.in_scan(&dir.join("unique.txt").to_string_lossy()));

        let mut res = res;
        let removed: HashSet<String> = [big1.clone()].into_iter().collect();
        res.remove_paths(&removed);
        assert!(!res.in_scan(&big1));
        assert_eq!(res.groups[0].files.len(), 2);
        assert_eq!(res.groups[0].reclaimable(), (32 * 1024) as u64);
    }

    #[test]
    fn same_size_different_content_not_grouped() {
        let tmp = TempDir::fresh();
        let dir = tmp.path();

        // Small same-size different content
        std::fs::write(dir.join("s1.bin"), b"aaaa").unwrap();
        std::fs::write(dir.join("s2.bin"), b"aaab").unwrap();

        // Large files with identical 4 KiB head AND identical 4 KiB tail, differing ONLY in middle
        let mut mid_a = vec![0x11u8; 24 * 1024];
        let mut mid_b = vec![0x11u8; 24 * 1024];
        mid_a[12 * 1024] = 0xAA;
        mid_b[12 * 1024] = 0xBB;
        std::fs::write(dir.join("m1.bin"), &mid_a).unwrap();
        std::fs::write(dir.join("m2.bin"), &mid_b).unwrap();

        let res = scan(dir, 1, &|_, _| {}, &|| false).unwrap();
        assert_eq!(res.groups.len(), 0);
        assert_eq!(res.scanned_files, 4);
    }

    #[test]
    fn zero_byte_files_ignored_and_min_bytes_respected() {
        let tmp = TempDir::fresh();
        let dir = tmp.path();

        // Multiple 0-byte files must never be grouped
        std::fs::write(dir.join("empty1.txt"), b"").unwrap();
        std::fs::write(dir.join("empty2.txt"), b"").unwrap();

        // 500-byte duplicates
        std::fs::write(dir.join("mid1.bin"), vec![7u8; 500]).unwrap();
        std::fs::write(dir.join("mid2.bin"), vec![7u8; 500]).unwrap();

        // 2000-byte duplicates
        std::fs::write(dir.join("large1.bin"), vec![9u8; 2000]).unwrap();
        std::fs::write(dir.join("large2.bin"), vec![9u8; 2000]).unwrap();

        // Even with min_bytes = 0, 0-byte files are skipped
        let res_all = scan(dir, 0, &|_, _| {}, &|| false).unwrap();
        assert_eq!(res_all.groups.len(), 2);

        // With min_bytes = 1000, only the 2000-byte pair is grouped
        let res_filtered = scan(dir, 1000, &|_, _| {}, &|| false).unwrap();
        assert_eq!(res_filtered.groups.len(), 1);
        assert_eq!(res_filtered.groups[0].size, 2000);
    }

    #[test]
    fn cancellation_stops_cleanly() {
        let tmp = TempDir::fresh();
        let dir = tmp.path();
        std::fs::write(dir.join("a.bin"), vec![3u8; 16 * 1024]).unwrap();
        std::fs::write(dir.join("b.bin"), vec![3u8; 16 * 1024]).unwrap();

        let err = scan(dir, 1, &|_, _| {}, &|| true);
        assert!(matches!(err, Err(ScanError::Cancelled)));
    }
}
