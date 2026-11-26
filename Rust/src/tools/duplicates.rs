//! Duplicate file finder: scans a drive or folder for byte-identical files using a parallel
//! multi-stage pipeline:
//!   1. Parallel `FindFirstFileExW` (`FindExInfoBasic` + `FIND_FIRST_EX_LARGE_FETCH` + `\\?\` paths)
//!      across worker threads, reading size + modified time directly from the directory entry
//!      without per-file `metadata()` syscalls.
//!   2. Group by exact file size (discarding unique sizes).
//!   3. Parallel 4 KiB head + 4 KiB tail partial SHA-256.
//!   4. Parallel full 256 KiB-chunked SHA-256.
//!
//! Safety rules:
//! - Skips symlinks and Windows reparse points (junctions, mount points, cloud placeholders) so
//!   walks never loop or hydrate online-only files.
//! - Skips protected system folders during the walk and never offers deletion on protected paths.
//! - Only paths from the current scan may ever be revealed or moved to the Recycle Bin.
//! - Deletions go to the Recycle Bin only (`diskmap::to_recycle_bin`), never permanent delete.

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};
#[cfg(not(windows))]
use std::time::UNIX_EPOCH;
use tauri::{AppHandle, Manager};

use super::diskmap::{drives, protected, to_recycle_bin};
use super::{arg, ask_folder};
use crate::api::{push, AppState};
use crate::config;
use crate::jobs::JobError;

const PARTIAL_BYTES: usize = 4 * 1024; // 4 KiB head + 4 KiB tail
const FULL_CHUNK: usize = 256 * 1024; // 256 KiB full-hash read buffer
const MAX_HASH_THREADS: usize = 16;
const PROGRESS_INTERVAL: Duration = Duration::from_millis(80);
const WALK_PROGRESS_STEP: u64 = 2000;

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
    cpus.clamp(1, MAX_HASH_THREADS).min(items.max(1))
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

// ------------------------------------------------------------------ parallel directory walk (Windows FindFirstFileExW)

struct DirWalkOut {
    subdirs: Vec<PathBuf>,
    files: Vec<FileEntry>,
    scanned: u64,
}

#[cfg(windows)]
mod win_walk {
    use super::*;
    use std::borrow::Cow;
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{GetLastError, HANDLE, INVALID_HANDLE_VALUE};

    const FIND_EX_INFO_BASIC: i32 = 1;
    const FIND_EX_SEARCH_NAME_MATCH: i32 = 0;
    const FIND_FIRST_EX_LARGE_FETCH: u32 = 0x0000_0002;
    const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x0000_0010;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
    const ERROR_FILE_NOT_FOUND: u32 = 2;
    const ERROR_NO_MORE_FILES: u32 = 18;
    const ERROR_INVALID_PARAMETER: u32 = 87;
    const WINDOWS_TO_UNIX_EPOCH_SECS: u64 = 11_644_473_600;

    #[repr(C)]
    #[allow(dead_code)]
    struct FileTime {
        dw_low_date_time: u32,
        dw_high_date_time: u32,
    }

    #[repr(C)]
    #[allow(dead_code)]
    struct Win32FindDataW {
        dw_file_attributes: u32,
        ft_creation_time: FileTime,
        ft_last_access_time: FileTime,
        ft_last_write_time: FileTime,
        n_file_size_high: u32,
        n_file_size_low: u32,
        dw_reserved0: u32,
        dw_reserved1: u32,
        c_file_name: [u16; 260],
        c_alternate_file_name: [u16; 14],
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn FindFirstFileExW(
            lp_file_name: *const u16,
            f_info_level_id: i32,
            lp_find_file_data: *mut Win32FindDataW,
            f_search_op: i32,
            lp_search_filter: *const c_void,
            dw_additional_flags: u32,
        ) -> HANDLE;
        fn FindNextFileW(h_find_file: HANDLE, lp_find_file_data: *mut Win32FindDataW) -> i32;
        fn FindClose(h_find_file: HANDLE) -> i32;
    }

    struct FindHandleGuard(HANDLE);
    impl Drop for FindHandleGuard {
        fn drop(&mut self) {
            unsafe {
                FindClose(self.0);
            }
        }
    }

    fn to_extended_search_pattern(dir: &Path) -> Vec<u16> {
        let abs = if dir.is_absolute() {
            Cow::Borrowed(dir)
        } else {
            Cow::Owned(std::path::absolute(dir).unwrap_or_else(|_| dir.to_path_buf()))
        };
        let raw: Vec<u16> = abs
            .as_os_str()
            .encode_wide()
            .map(|c| if c == b'/' as u16 { b'\\' as u16 } else { c })
            .collect();
        let verbatim = [b'\\' as u16, b'\\' as u16, b'?' as u16, b'\\' as u16];
        let device = [b'\\' as u16, b'\\' as u16, b'.' as u16, b'\\' as u16];
        let unc = [b'\\' as u16, b'\\' as u16];

        let mut out = Vec::with_capacity(raw.len() + 12);
        if raw.starts_with(&verbatim) || raw.starts_with(&device) {
            out.extend_from_slice(&raw);
        } else if raw.starts_with(&unc) {
            out.extend(r"\\?\UNC\".encode_utf16());
            out.extend_from_slice(&raw[2..]);
        } else {
            out.extend(r"\\?\".encode_utf16());
            out.extend_from_slice(&raw);
        }
        if !out.ends_with(&[b'\\' as u16]) {
            out.push(b'\\' as u16);
        }
        out.push(b'*' as u16);
        out.push(0);
        out
    }

    fn filetime_to_unix_secs(ft: &FileTime) -> u64 {
        let ticks = ((ft.dw_high_date_time as u64) << 32) | (ft.dw_low_date_time as u64);
        (ticks / 10_000_000).saturating_sub(WINDOWS_TO_UNIX_EPOCH_SECS)
    }

    pub fn scan_dir(
        dir: &Path,
        min_size: u64,
        guard: &WalkGuard,
        seen: &AtomicU64,
        on_progress: &(impl Fn(u64, &Path) + Sync),
        cancelled: &(impl Fn() -> bool + Sync),
    ) -> Result<DirWalkOut, ScanError> {
        let pattern = to_extended_search_pattern(dir);
        let mut data: Win32FindDataW = unsafe { std::mem::zeroed() };
        let mut handle = unsafe {
            FindFirstFileExW(
                pattern.as_ptr(),
                FIND_EX_INFO_BASIC,
                &mut data,
                FIND_EX_SEARCH_NAME_MATCH,
                std::ptr::null(),
                FIND_FIRST_EX_LARGE_FETCH,
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            let err = unsafe { GetLastError() };
            if err == ERROR_INVALID_PARAMETER {
                handle = unsafe {
                    FindFirstFileExW(
                        pattern.as_ptr(),
                        FIND_EX_INFO_BASIC,
                        &mut data,
                        FIND_EX_SEARCH_NAME_MATCH,
                        std::ptr::null(),
                        0,
                    )
                };
            }
        }
        if handle == INVALID_HANDLE_VALUE {
            let err = unsafe { GetLastError() };
            if err == ERROR_FILE_NOT_FOUND || err == ERROR_NO_MORE_FILES {
                return Ok(DirWalkOut { subdirs: Vec::new(), files: Vec::new(), scanned: 0 });
            }
            return Ok(DirWalkOut { subdirs: Vec::new(), files: Vec::new(), scanned: 0 });
        }
        let _guard = FindHandleGuard(handle);

        let mut out = DirWalkOut {
            subdirs: Vec::new(),
            files: Vec::new(),
            scanned: 0,
        };
        let mut batch = 0u64;
        let mut iter_count = 0u32;

        loop {
            iter_count = iter_count.wrapping_add(1);
            if iter_count & 0x3FF == 0 && cancelled() {
                return Err(ScanError::Cancelled);
            }

            let name_len = data
                .c_file_name
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(data.c_file_name.len());
            let name_u16 = &data.c_file_name[..name_len];
            let is_dot = name_u16 == [b'.' as u16] || name_u16 == [b'.' as u16, b'.' as u16];

            if !is_dot && !name_u16.is_empty() {
                let attrs = data.dw_file_attributes;
                // Skip all reparse points (symlinks, junctions, mount points, cloud placeholders).
                if (attrs & FILE_ATTRIBUTE_REPARSE_POINT) == 0 {
                    let name = String::from_utf16_lossy(name_u16);
                    let path = dir.join(&name);
                    if (attrs & FILE_ATTRIBUTE_DIRECTORY) != 0 {
                        if !guard.should_skip_dir(&path) {
                            out.subdirs.push(path);
                        }
                    } else {
                        out.scanned += 1;
                        batch += 1;
                        if batch >= WALK_PROGRESS_STEP {
                            let prev = seen.fetch_add(batch, Ordering::Relaxed);
                            let now = prev + batch;
                            batch = 0;
                            if now / WALK_PROGRESS_STEP != prev / WALK_PROGRESS_STEP {
                                on_progress(now, dir);
                            }
                        }
                        let size = ((data.n_file_size_high as u64) << 32)
                            | (data.n_file_size_low as u64);
                        if size >= min_size {
                            let modified = filetime_to_unix_secs(&data.ft_last_write_time);
                            out.files.push(FileEntry { path, size, modified });
                        }
                    }
                }
            }

            if unsafe { FindNextFileW(handle, &mut data) } == 0 {
                break;
            }
        }

        if batch > 0 {
            let prev = seen.fetch_add(batch, Ordering::Relaxed);
            let now = prev + batch;
            if now / WALK_PROGRESS_STEP != prev / WALK_PROGRESS_STEP {
                on_progress(now, dir);
            }
        }
        Ok(out)
    }
}

#[cfg(not(windows))]
mod unix_walk {
    use super::*;

    pub fn scan_dir(
        dir: &Path,
        min_size: u64,
        guard: &WalkGuard,
        seen: &AtomicU64,
        on_progress: &(impl Fn(u64, &Path) + Sync),
        cancelled: &(impl Fn() -> bool + Sync),
    ) -> Result<DirWalkOut, ScanError> {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return Ok(DirWalkOut { subdirs: Vec::new(), files: Vec::new(), scanned: 0 });
        };
        let mut out = DirWalkOut { subdirs: Vec::new(), files: Vec::new(), scanned: 0 };
        let mut batch = 0u64;
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
                    out.subdirs.push(path);
                }
                continue;
            }
            if !ft.is_file() {
                continue;
            }
            out.scanned += 1;
            batch += 1;
            if batch >= WALK_PROGRESS_STEP {
                let prev = seen.fetch_add(batch, Ordering::Relaxed);
                let now = prev + batch;
                batch = 0;
                if now / WALK_PROGRESS_STEP != prev / WALK_PROGRESS_STEP {
                    on_progress(now, dir);
                }
            }
            let size = meta.len();
            if size >= min_size {
                let modified = meta
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                out.files.push(FileEntry { path, size, modified });
            }
        }
        if batch > 0 {
            let prev = seen.fetch_add(batch, Ordering::Relaxed);
            let now = prev + batch;
            if now / WALK_PROGRESS_STEP != prev / WALK_PROGRESS_STEP {
                on_progress(now, dir);
            }
        }
        Ok(out)
    }
}

struct WalkQueue {
    queue: VecDeque<PathBuf>,
    active: usize,
    cancelled: bool,
}

fn parallel_walk(
    root: PathBuf,
    min_size: u64,
    guard: &WalkGuard,
    gate: &ProgressGate<impl Fn(Option<f64>, &str) + Sync>,
    cancelled: &(impl Fn() -> bool + Sync),
) -> Result<(HashMap<u64, Vec<FileEntry>>, u64), ScanError> {
    let mut initial = VecDeque::with_capacity(256);
    initial.push_back(root);
    let state = Mutex::new(WalkQueue {
        queue: initial,
        active: 0,
        cancelled: false,
    });
    let cvar = Condvar::new();
    let seen = AtomicU64::new(0);

    let workers = thread::scope(|s| -> Result<Vec<(HashMap<u64, Vec<FileEntry>>, u64)>, ScanError> {
        let num_workers = thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4)
            .saturating_mul(2)
            .clamp(4, 16);

        let handles: Vec<_> = (0..num_workers)
            .map(|_| {
                s.spawn(|| -> Result<(HashMap<u64, Vec<FileEntry>>, u64), ScanError> {
                    let mut local_by_size: HashMap<u64, Vec<FileEntry>> = HashMap::new();
                    let mut local_scanned = 0u64;

                    loop {
                        let dir = {
                            let mut st = state.lock().unwrap();
                            loop {
                                if st.cancelled || cancelled() {
                                    st.cancelled = true;
                                    cvar.notify_all();
                                    return Err(ScanError::Cancelled);
                                }
                                if let Some(d) = st.queue.pop_front() {
                                    st.active += 1;
                                    break d;
                                }
                                if st.active == 0 {
                                    return Ok((local_by_size, local_scanned));
                                }
                                st = cvar.wait(st).unwrap();
                            }
                        };

                        let on_progress = |count: u64, cur_dir: &Path| {
                            gate.emit(false, || {
                                (
                                    None,
                                    format!(
                                        "Scanning… {} files · {}",
                                        grouped(count),
                                        tail_str(&cur_dir.to_string_lossy(), 60)
                                    ),
                                )
                            });
                        };

                        #[cfg(windows)]
                        let res = win_walk::scan_dir(&dir, min_size, guard, &seen, &on_progress, cancelled);
                        #[cfg(not(windows))]
                        let res = unix_walk::scan_dir(&dir, min_size, guard, &seen, &on_progress, cancelled);

                        match res {
                            Ok(out) => {
                                local_scanned += out.scanned;
                                for f in out.files {
                                    local_by_size.entry(f.size).or_default().push(f);
                                }
                                let mut st = state.lock().unwrap();
                                st.active -= 1;
                                if !out.subdirs.is_empty() {
                                    for sub in out.subdirs {
                                        st.queue.push_back(sub);
                                    }
                                    cvar.notify_all();
                                } else if st.queue.is_empty() && st.active == 0 {
                                    cvar.notify_all();
                                }
                            }
                            Err(ScanError::Cancelled) => {
                                let mut st = state.lock().unwrap();
                                st.active -= 1;
                                st.cancelled = true;
                                cvar.notify_all();
                                return Err(ScanError::Cancelled);
                            }
                        }
                    }
                })
            })
            .collect();

        let mut results = Vec::with_capacity(handles.len());
        for h in handles {
            results.push(h.join().unwrap_or_else(|_| {
                Ok((HashMap::new(), 0))
            })?);
        }
        Ok(results)
    })?;

    let mut by_size: HashMap<u64, Vec<FileEntry>> = HashMap::new();
    let mut scanned_files = 0u64;
    for (local_map, local_count) in workers {
        scanned_files += local_count;
        for (size, mut list) in local_map {
            by_size.entry(size).or_default().append(&mut list);
        }
    }
    Ok((by_size, scanned_files))
}

// ------------------------------------------------------------------ 3-stage parallel pipeline

pub fn scan(
    root: &Path,
    min_bytes: u64,
    progress: &(impl Fn(Option<f64>, &str) + Sync),
    cancelled: &(impl Fn() -> bool + Sync),
) -> Result<DupResult, ScanError> {
    if cancelled() {
        return Err(ScanError::Cancelled);
    }
    let min_size = min_bytes.max(1);
    let root_clean: PathBuf = root.components().collect();
    let root_str = root_clean.to_string_lossy().to_string();
    let guard = WalkGuard::new();
    let gate = ProgressGate::new(progress);

    // Stage 1: parallel FindFirstFileExW walk, collecting (path, size, modified_unix)
    let (by_size, scanned_files) = parallel_walk(root_clean, min_size, &guard, &gate, cancelled)?;

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
