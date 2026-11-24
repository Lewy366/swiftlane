//! Disk space map: what takes up the room on a drive or in a folder. A port of
//! Source/toolkit/diskmap.py plus the disk_* methods of Source/swiftlane.py.
//!
//! Layer 1 (default, no admin): parallel work-stealing directory walk using Win32
//! `FindFirstFileExW` (`FindExInfoBasic` + `FIND_FIRST_EX_LARGE_FETCH` + `\\?\` extended paths)
//! with zero per-file `metadata()` syscalls. Never follows junctions, mount points, or symlinks.
//!
//! Layer 2 (optional, admin): WizTree-speed raw NTFS `$MFT` sequential stream via a short-lived,
//! strictly read-only elevated CLI helper (`--disk-mft-scan <DRIVE> <temp_out_path>`).
//!
//! Files under 1 MB are only counted per folder to keep memory small; bigger ones are kept by name.
//! The only way to remove anything is the Recycle Bin, and never for Windows, programs, app data,
//! your user folders, or drives without a Recycle Bin.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::cmp::Reverse;
use std::collections::{BinaryHeap, VecDeque};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager};

use super::arg;
use crate::api::{push, AppState};
use crate::config;
use crate::jobs::JobError;
use crate::winapi;

const BIG: u64 = 1 << 20; // files at least this size are listed by name
const TOP_FILES: usize = 60;
const NONE: usize = usize::MAX;
const PROGRESS_STEP: u64 = 4000;
const DMFT_TIMEOUT_SECS: u64 = 120;
const ADVICE_LIMIT: usize = 25;

#[derive(Serialize, Deserialize)]
struct Node {
    name: String,
    parent: usize,
    size: u64,
    files: u64,
    dirs: Vec<usize>,
    big: Vec<(String, u64)>,
    small_size: u64,
    small_count: u64,
    denied: bool,
}

impl Node {
    fn new(name: String, parent: usize) -> Self {
        Node {
            name,
            parent,
            size: 0,
            files: 0,
            dirs: Vec::new(),
            big: Vec::new(),
            small_size: 0,
            small_count: 0,
            denied: false,
        }
    }
}

/// A scanned folder tree; node 0 is the folder that was scanned (its name is its full path).
#[derive(Serialize, Deserialize)]
pub struct Tree {
    nodes: Vec<Node>,
    top: Vec<(u64, String)>, // biggest files, biggest first
}

impl Tree {
    fn root_name(&self) -> &str {
        &self.nodes[0].name
    }

    fn path(&self, i: usize) -> PathBuf {
        path_of(&self.nodes, i)
    }

    /// The folder for a path inside the scanned tree, or None.
    fn find(&self, path: &str) -> Option<usize> {
        let rel = Path::new(path).strip_prefix(self.root_name()).ok()?;
        let mut node = 0;
        for part in rel.components() {
            match part {
                Component::CurDir => {}
                Component::Normal(name) => {
                    let name = name.to_string_lossy();
                    node = *self.nodes[node].dirs.iter().find(|&&d| self.nodes[d].name == name)?;
                }
                _ => return None,
            }
        }
        Some(node)
    }

    /// Children of a folder, biggest first, as the page wants them.
    fn view(&self, i: usize, limit: usize) -> Value {
        let node = &self.nodes[i];
        let base = self.path(i);
        let mut items: Vec<Value> = node
            .dirs
            .iter()
            .map(|&d| {
                let d = &self.nodes[d];
                json!({ "name": d.name, "path": base.join(&d.name).to_string_lossy(), "size": d.size, "kind": "dir", "count": d.files, "denied": d.denied })
            })
            .collect();
        items.extend(node.big.iter().map(|(n, s)| json!({ "name": n, "path": base.join(n).to_string_lossy(), "size": s, "kind": "file" })));
        if node.small_count > 0 {
            items.push(json!({ "name": "Smaller files", "path": null, "size": node.small_size, "kind": "small", "count": node.small_count }));
        }
        let size = |v: &Value| v["size"].as_u64().unwrap_or(0);
        items.sort_by_key(|v| Reverse(size(v)));
        if items.len() > limit {
            let rest = items.split_off(limit);
            items.push(json!({ "name": format!("{} more", rest.len()), "path": null, "size": rest.iter().map(size).sum::<u64>(), "kind": "small", "count": rest.len() }));
        }
        let mut crumbs = Vec::new();
        let mut n = i;
        while n != NONE {
            crumbs.push(json!({ "name": self.nodes[n].name, "path": self.path(n).to_string_lossy() }));
            n = self.nodes[n].parent;
        }
        crumbs.reverse();
        json!({ "path": base.to_string_lossy(), "size": node.size, "files": node.files, "denied": node.denied, "items": items, "crumbs": crumbs })
    }

    /// After something went to the Recycle Bin: take it out of the tree and the totals.
    fn remove(&mut self, path: &str, size: u64) {
        let p = Path::new(path);
        let (Some(parent), Some(name)) = (p.parent().and_then(|x| self.find(&x.to_string_lossy())), p.file_name()) else { return };
        let name = name.to_string_lossy();
        let files = match self.nodes[parent].dirs.iter().position(|&d| self.nodes[d].name == name) {
            Some(at) => {
                let d = self.nodes[parent].dirs.remove(at);
                self.nodes[d].files
            }
            None => {
                self.nodes[parent].big.retain(|b| b.0 != name);
                1
            }
        };
        let mut n = parent;
        while n != NONE {
            self.nodes[n].size = self.nodes[n].size.saturating_sub(size);
            self.nodes[n].files = self.nodes[n].files.saturating_sub(files);
            n = self.nodes[n].parent;
        }
        self.top.retain(|t| !Path::new(&t.1).starts_with(p));
    }
}

fn path_of(nodes: &[Node], mut i: usize) -> PathBuf {
    let mut parts = Vec::new();
    while i != NONE {
        parts.push(nodes[i].name.as_str());
        i = nodes[i].parent;
    }
    parts.iter().rev().collect()
}

pub enum ScanError {
    Cancelled,
}

// ------------------------------------------------------------------ Layer 1: parallel scanner

#[derive(Default)]
struct DirScan {
    subdirs: Vec<(String, PathBuf)>,
    big: Vec<(String, u64)>,
    size: u64,
    files: u64,
    small_size: u64,
    small_count: u64,
    denied: bool,
}

fn push_top(heap: &mut BinaryHeap<Reverse<(u64, String)>>, size: u64, path: impl FnOnce() -> String) {
    if heap.len() < TOP_FILES {
        heap.push(Reverse((size, path())));
    } else if heap.peek().is_some_and(|m| size > m.0 .0) {
        heap.pop();
        heap.push(Reverse((size, path())));
    }
}

#[cfg(windows)]
mod win_scan {
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
    const IO_REPARSE_TAG_SYMLINK: u32 = 0xA000_000C;
    const ERROR_FILE_NOT_FOUND: u32 = 2;
    const ERROR_NO_MORE_FILES: u32 = 18;
    const ERROR_INVALID_PARAMETER: u32 = 87;

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

    /// Converts a directory path into a `\\?\...\*` (or `\\?\UNC\server\share\...\*`) UTF-16
    /// search pattern so paths >260 chars never fail.
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

    pub fn scan_dir(
        dir: &Path,
        local_top: &mut BinaryHeap<Reverse<(u64, String)>>,
        seen: &AtomicU64,
        progress: &(impl Fn(u64, &Path) + Sync),
        cancelled: &(impl Fn() -> bool + Sync),
    ) -> Result<DirScan, ScanError> {
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
                return Ok(DirScan::default());
            }
            return Ok(DirScan {
                denied: true,
                ..Default::default()
            });
        }
        let _guard = FindHandleGuard(handle);

        let mut out = DirScan::default();
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
                batch += 1;
                if batch >= PROGRESS_STEP {
                    let prev = seen.fetch_add(batch, Ordering::Relaxed);
                    let now = prev + batch;
                    batch = 0;
                    if now / PROGRESS_STEP != prev / PROGRESS_STEP {
                        progress(now, dir);
                    }
                }

                let attrs = data.dw_file_attributes;
                let is_dir = (attrs & FILE_ATTRIBUTE_DIRECTORY) != 0;
                let is_reparse = (attrs & FILE_ATTRIBUTE_REPARSE_POINT) != 0;

                // Skip directory symlinks, junctions, and mount points so nothing loops or double-counts.
                // Skip file symlinks, while keeping real content like OneDrive cloud placeholders.
                let skip = (is_dir && is_reparse)
                    || (!is_dir && is_reparse && data.dw_reserved0 == IO_REPARSE_TAG_SYMLINK);
                if !skip {
                    let name = String::from_utf16_lossy(name_u16);
                    if is_dir {
                        let child_path = dir.join(&name);
                        out.subdirs.push((name, child_path));
                    } else {
                        let size =
                            ((data.n_file_size_high as u64) << 32) | (data.n_file_size_low as u64);
                        out.files += 1;
                        out.size = out.size.saturating_add(size);
                        if size >= BIG {
                            out.big.push((name.clone(), size));
                            push_top(local_top, size, || {
                                dir.join(&name).to_string_lossy().to_string()
                            });
                        } else {
                            out.small_size = out.small_size.saturating_add(size);
                            out.small_count += 1;
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
            if now / PROGRESS_STEP != prev / PROGRESS_STEP {
                progress(now, dir);
            }
        }
        Ok(out)
    }
}

#[cfg(not(windows))]
mod unix_scan {
    use super::*;

    pub fn scan_dir(
        dir: &Path,
        local_top: &mut BinaryHeap<Reverse<(u64, String)>>,
        seen: &AtomicU64,
        progress: &(impl Fn(u64, &Path) + Sync),
        cancelled: &(impl Fn() -> bool + Sync),
    ) -> Result<DirScan, ScanError> {
        let iter = match std::fs::read_dir(dir) {
            Ok(it) => it,
            Err(_) => {
                return Ok(DirScan {
                    denied: true,
                    ..Default::default()
                });
            }
        };

        let mut out = DirScan::default();
        let mut batch = 0u64;
        let mut iter_count = 0u32;

        for entry in iter.filter_map(Result::ok) {
            iter_count = iter_count.wrapping_add(1);
            if iter_count & 0x3FF == 0 && cancelled() {
                return Err(ScanError::Cancelled);
            }
            batch += 1;
            if batch >= PROGRESS_STEP {
                let prev = seen.fetch_add(batch, Ordering::Relaxed);
                let now = prev + batch;
                batch = 0;
                if now / PROGRESS_STEP != prev / PROGRESS_STEP {
                    progress(now, dir);
                }
            }

            let Ok(kind) = entry.file_type() else { continue };
            if kind.is_symlink() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            if kind.is_dir() {
                out.subdirs.push((name, entry.path()));
                continue;
            }
            let Ok(meta) = entry.metadata() else { continue };
            let size = meta.len();
            out.files += 1;
            out.size = out.size.saturating_add(size);
            if size >= BIG {
                out.big.push((name, size));
                push_top(local_top, size, || {
                    entry.path().to_string_lossy().to_string()
                });
            } else {
                out.small_size = out.small_size.saturating_add(size);
                out.small_count += 1;
            }
        }

        if batch > 0 {
            let prev = seen.fetch_add(batch, Ordering::Relaxed);
            let now = prev + batch;
            if now / PROGRESS_STEP != prev / PROGRESS_STEP {
                progress(now, dir);
            }
        }
        Ok(out)
    }
}

struct QueueState {
    nodes: Vec<Node>,
    queue: VecDeque<(usize, PathBuf)>,
    active: usize,
    cancelled: bool,
}

/// Scans a folder in parallel. `progress(entries seen, current folder)` comes every ~4000 entries.
pub fn scan(
    root: &Path,
    progress: impl Fn(u64, &Path) + Sync,
    cancelled: impl Fn() -> bool + Sync,
) -> Result<Tree, ScanError> {
    if cancelled() {
        return Err(ScanError::Cancelled);
    }

    let root_name: PathBuf = root.components().collect();
    let mut initial_queue = VecDeque::with_capacity(256);
    initial_queue.push_back((0usize, root_name.clone()));

    let state = Mutex::new(QueueState {
        nodes: vec![Node::new(root_name.to_string_lossy().to_string(), NONE)],
        queue: initial_queue,
        active: 0,
        cancelled: false,
    });
    let cvar = Condvar::new();
    let seen = AtomicU64::new(0);
    let global_top: Mutex<BinaryHeap<Reverse<(u64, String)>>> = Mutex::new(BinaryHeap::new());

    let workers = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .clamp(1, 12);

    std::thread::scope(|s| {
        for _ in 0..workers {
            s.spawn(|| {
                let mut local_top: BinaryHeap<Reverse<(u64, String)>> = BinaryHeap::new();
                loop {
                    let task = {
                        let mut st = state.lock().unwrap();
                        loop {
                            if st.cancelled || cancelled() {
                                st.cancelled = true;
                                cvar.notify_all();
                                return;
                            }
                            if let Some(item) = st.queue.pop_front() {
                                st.active += 1;
                                break Some(item);
                            }
                            if st.active == 0 {
                                cvar.notify_all();
                                break None;
                            }
                            st = cvar.wait(st).unwrap();
                        }
                    };
                    let Some((node_idx, dir_path)) = task else {
                        break;
                    };

                    #[cfg(windows)]
                    let res = win_scan::scan_dir(&dir_path, &mut local_top, &seen, &progress, &cancelled);
                    #[cfg(not(windows))]
                    let res = unix_scan::scan_dir(&dir_path, &mut local_top, &seen, &progress, &cancelled);

                    match res {
                        Ok(dir_scan) => {
                            let mut st = state.lock().unwrap();
                            st.active -= 1;
                            if st.cancelled || cancelled() {
                                st.cancelled = true;
                                cvar.notify_all();
                                return;
                            }
                            let node = &mut st.nodes[node_idx];
                            node.size = dir_scan.size;
                            node.files = dir_scan.files;
                            node.big = dir_scan.big;
                            node.small_size = dir_scan.small_size;
                            node.small_count = dir_scan.small_count;
                            node.denied = dir_scan.denied;

                            let pushed = !dir_scan.subdirs.is_empty();
                            for (name, child_path) in dir_scan.subdirs {
                                let child_idx = st.nodes.len();
                                st.nodes.push(Node::new(name, node_idx));
                                st.nodes[node_idx].dirs.push(child_idx);
                                st.queue.push_back((child_idx, child_path));
                            }
                            if pushed || (st.active == 0 && st.queue.is_empty()) {
                                cvar.notify_all();
                            }
                        }
                        Err(ScanError::Cancelled) => {
                            let mut st = state.lock().unwrap();
                            st.active -= 1;
                            st.cancelled = true;
                            cvar.notify_all();
                            return;
                        }
                    }
                }

                if !local_top.is_empty() {
                    let mut gt = global_top.lock().unwrap();
                    for Reverse((size, path)) in local_top {
                        push_top(&mut gt, size, || path);
                    }
                }
            });
        }
    });

    let st = state.into_inner().unwrap();
    if st.cancelled || cancelled() {
        return Err(ScanError::Cancelled);
    }

    let mut nodes = st.nodes;
    // Children are always appended after their parents, so reverse order rolls totals upward cleanly.
    for i in (1..nodes.len()).rev() {
        let (size, files, parent) = (nodes[i].size, nodes[i].files, nodes[i].parent);
        nodes[parent].size = nodes[parent].size.saturating_add(size);
        nodes[parent].files = nodes[parent].files.saturating_add(files);
    }

    let mut top: Vec<(u64, String)> = global_top.into_inner().unwrap().into_iter().map(|r| r.0).collect();
    top.sort_by(|a, b| b.cmp(a));
    Ok(Tree { nodes, top })
}

// ------------------------------------------------------------------ Layer 2: NTFS $MFT scan (Admin)

/// Accepts `"C"`, `"C:"`, `"C:\\"`, or `"C:/"` from the UI and returns uppercase `'C'`.
fn parse_drive_letter(input: &str) -> Option<char> {
    let bytes = input.trim().as_bytes();
    let b = match bytes {
        [a] if a.is_ascii_alphabetic() => *a,
        [a, b':'] if a.is_ascii_alphabetic() => *a,
        [a, b':', b'\\' | b'/'] if a.is_ascii_alphabetic() => *a,
        _ => return None,
    };
    Some((b as char).to_ascii_uppercase())
}

/// Strictly validates `--disk-mft-scan` CLI `<DRIVE>`: exactly 1 ASCII letter A..=Z.
fn validate_dmft_cli_drive(input: &str) -> Option<char> {
    let bytes = input.as_bytes();
    if bytes.len() == 1 && bytes[0].is_ascii_alphabetic() {
        Some((bytes[0] as char).to_ascii_uppercase())
    } else {
        None
    }
}

/// Strictly validates `--disk-mft-scan` CLI `<temp_out_path>`:
/// must be a single `swl-dmft-*.swl-dmft` file directly inside canonical `std::env::temp_dir()`.
fn validate_dmft_out_path(raw: &str) -> Option<PathBuf> {
    if raw.is_empty() {
        return None;
    }
    let p = Path::new(raw);
    if p.components().any(|c| matches!(c, Component::ParentDir)) {
        return None;
    }
    let fname = p.file_name()?.to_str()?;
    if !fname.starts_with("swl-dmft-") || !fname.ends_with(".swl-dmft") {
        return None;
    }
    if fname.len() <= "swl-dmft-.swl-dmft".len() {
        return None;
    }
    if !fname
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.')
    {
        return None;
    }
    let parent = p.parent()?;
    let canon_parent = std::fs::canonicalize(parent).ok()?;
    let canon_temp = std::fs::canonicalize(std::env::temp_dir()).ok()?;
    if canon_parent != canon_temp {
        return None;
    }
    Some(canon_temp.join(fname))
}

fn parse_dmft_cli_args<I: IntoIterator<Item = String>>(args: I) -> Option<(String, String)> {
    let mut it = args.into_iter().skip(1);
    while let Some(a) = it.next() {
        if a == "--disk-mft-scan" {
            let drive = it.next().unwrap_or_default();
            let out = it.next().unwrap_or_default();
            return Some((drive, out));
        }
    }
    None
}

fn done_sentinel_path(out_path: &Path) -> PathBuf {
    let mut os = out_path.as_os_str().to_os_string();
    os.push(".done");
    PathBuf::from(os)
}

fn temp_stage_path(out_path: &Path) -> PathBuf {
    let mut os = out_path.as_os_str().to_os_string();
    os.push(".tmp");
    PathBuf::from(os)
}

#[cfg(any(windows, test))]
mod ntfs_mft {
    use super::*;
    use std::collections::HashMap;
    use std::io::{BufWriter, Write};

    pub const NTFS_ROOT_FRN: u64 = 5;
    pub const FRN_MASK: u64 = 0x0000_FFFF_FFFF_FFFF;
    pub const MAX_MFT_BYTES: u64 = 16 * 1024 * 1024 * 1024; // 16 GiB safety cap

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct NtfsBoot {
        pub bytes_per_sector: u32,
        pub cluster_size: u32,
        pub record_size: u32,
        pub mft_byte_offset: u64,
    }

    #[derive(Debug, Clone)]
    pub struct MftEntry {
        pub frn: u64,
        pub parent_frn: u64,
        pub name: String,
        pub is_dir: bool,
        pub size: u64,
    }

    #[derive(Debug)]
    pub enum ParsedRecord {
        Base(MftEntry),
        ExtData(u64, u64), // (base_frn, size)
        Skip,
    }

    pub fn parse_ntfs_boot_sector(buf: &[u8], drive: char) -> Result<NtfsBoot, String> {
        if buf.len() < 512 {
            return Err(format!("Couldn't read boot sector on {drive}:."));
        }
        if &buf[3..11] != b"NTFS    " {
            return Err(format!("Drive {drive}: is not an NTFS volume."));
        }
        let bytes_per_sector = u16::from_le_bytes([buf[11], buf[12]]) as u32;
        if !(512..=4096).contains(&bytes_per_sector) || !bytes_per_sector.is_power_of_two() {
            return Err(format!("Unsupported sector size on {drive}:."));
        }
        let sectors_per_cluster = buf[13] as u32;
        if sectors_per_cluster == 0 {
            return Err(format!("Invalid cluster size on {drive}:."));
        }
        let cluster_size = bytes_per_sector
            .checked_mul(sectors_per_cluster)
            .ok_or_else(|| format!("Invalid cluster size on {drive}:."))?;
        if !(512..=2 * 1024 * 1024).contains(&cluster_size) {
            return Err(format!("Unsupported cluster size on {drive}:."));
        }
        let mft_lcn = u64::from_le_bytes(buf[48..56].try_into().unwrap());
        if mft_lcn == 0 {
            return Err(format!("Invalid NTFS MFT location on {drive}:."));
        }
        let mft_byte_offset = mft_lcn
            .checked_mul(cluster_size as u64)
            .ok_or_else(|| format!("Invalid NTFS MFT offset on {drive}:."))?;

        let raw_cpr = buf[64] as i8;
        let record_size = if raw_cpr > 0 {
            (raw_cpr as u32)
                .checked_mul(cluster_size)
                .ok_or_else(|| format!("Invalid MFT record size on {drive}:."))?
        } else if raw_cpr < 0 {
            let shift = (-(raw_cpr as i32)) as u32;
            if shift > 16 {
                return Err(format!("Invalid MFT record size on {drive}:."));
            }
            1u32 << shift
        } else {
            return Err(format!("Invalid MFT record size on {drive}:."));
        };

        if !(512..=4096).contains(&record_size) || record_size % 512 != 0 {
            return Err(format!("Unsupported MFT record size on {drive}:."));
        }

        Ok(NtfsBoot {
            bytes_per_sector,
            cluster_size,
            record_size,
            mft_byte_offset,
        })
    }

    /// Applies the NTFS Update Sequence Array (USA) fixup in-place across 512-byte strides.
    pub fn apply_usa_fixup(rec: &mut [u8]) -> bool {
        if rec.len() < 48 || &rec[0..4] != b"FILE" {
            return false;
        }
        let usa_offset = u16::from_le_bytes([rec[4], rec[5]]) as usize;
        let usa_count = u16::from_le_bytes([rec[6], rec[7]]) as usize;
        if usa_count < 1 || usa_offset < 42 {
            return false;
        }
        let Some(usa_end) = usa_offset.checked_add(usa_count.saturating_mul(2)) else {
            return false;
        };
        if usa_end > rec.len() {
            return false;
        }
        let sectors = usa_count - 1;
        if sectors.saturating_mul(512) > rec.len() {
            return false;
        }
        let usn0 = rec[usa_offset];
        let usn1 = rec[usa_offset + 1];
        for i in 1..usa_count {
            let end = i * 512;
            if rec[end - 2] != usn0 || rec[end - 1] != usn1 {
                return false;
            }
            rec[end - 2] = rec[usa_offset + i * 2];
            rec[end - 1] = rec[usa_offset + i * 2 + 1];
        }
        true
    }

    /// Decodes an NTFS non-resident data-run list into `(physical_byte_offset, byte_len)` pairs.
    /// Skips sparse runs (offset size == 0 or LCN == 0) and caps total size at `MAX_MFT_BYTES`.
    pub fn decode_data_runs(runs: &[u8], cluster_size: u64) -> Result<Vec<(u64, u64)>, String> {
        let mut out = Vec::new();
        let mut pos = 0usize;
        let mut current_lcn: i64 = 0;
        let mut total_bytes: u64 = 0;

        while pos < runs.len() {
            let header = runs[pos];
            if header == 0 {
                break;
            }
            pos += 1;
            let len_size = (header & 0x0F) as usize;
            let off_size = ((header >> 4) & 0x0F) as usize;
            if len_size == 0 || len_size > 8 || off_size > 8 {
                return Err("Corrupt NTFS data-run header.".into());
            }
            if pos + len_size + off_size > runs.len() {
                return Err("Truncated NTFS data-run list.".into());
            }

            let mut run_clusters = 0u64;
            for i in 0..len_size {
                run_clusters |= (runs[pos + i] as u64) << (i * 8);
            }
            pos += len_size;

            if off_size == 0 {
                // Sparse run: no physical clusters allocated.
                continue;
            }

            let mut delta: i64 = 0;
            for i in 0..off_size {
                delta |= (runs[pos + i] as i64) << (i * 8);
            }
            if off_size < 8 && (runs[pos + off_size - 1] & 0x80) != 0 {
                delta |= !0i64 << (off_size * 8);
            }
            pos += off_size;

            current_lcn = current_lcn
                .checked_add(delta)
                .ok_or_else(|| "NTFS data-run LCN overflow.".to_string())?;
            if current_lcn < 0 {
                return Err("Invalid negative NTFS data-run LCN.".into());
            }
            if current_lcn == 0 || run_clusters == 0 {
                continue;
            }

            let byte_len = run_clusters
                .checked_mul(cluster_size)
                .ok_or_else(|| "NTFS data-run length overflow.".to_string())?;
            let phys_offset = (current_lcn as u64)
                .checked_mul(cluster_size)
                .ok_or_else(|| "NTFS data-run offset overflow.".to_string())?;

            total_bytes = total_bytes
                .checked_add(byte_len)
                .ok_or_else(|| "NTFS MFT size overflow.".to_string())?;
            if total_bytes > MAX_MFT_BYTES {
                return Err("NTFS MFT exceeds the 16 GiB safety limit.".into());
            }

            out.push((phys_offset, byte_len));
        }

        Ok(out)
    }

    /// Extracts `(data_runs, real_size)` from MFT record #0 (`$MFT`).
    pub fn extract_mft_runs(rec0: &mut [u8], cluster_size: u64) -> Result<(Vec<(u64, u64)>, u64), String> {
        if !apply_usa_fixup(rec0) {
            return Err("Couldn't validate NTFS $MFT record #0.".into());
        }
        let first_attr = u16::from_le_bytes([rec0[20], rec0[21]]) as usize;
        let used = u32::from_le_bytes(rec0[24..28].try_into().unwrap()) as usize;
        let limit = if used >= first_attr && used <= rec0.len() { used } else { rec0.len() };
        let mut off = first_attr;

        while off + 4 <= limit {
            let attr_type = u32::from_le_bytes(rec0[off..off + 4].try_into().unwrap());
            if attr_type == 0xFFFF_FFFF {
                break;
            }
            if off + 16 > limit {
                break;
            }
            let attr_len = u32::from_le_bytes(rec0[off + 4..off + 8].try_into().unwrap()) as usize;
            if attr_len < 16 || off + attr_len > limit {
                break;
            }
            let non_res = rec0[off + 8];
            let name_len = rec0[off + 9];

            if attr_type == 0x80 && name_len == 0 && non_res == 1 && attr_len >= 64 {
                let lowest_vcn = u64::from_le_bytes(rec0[off + 16..off + 24].try_into().unwrap());
                if lowest_vcn == 0 {
                    let runs_off = u16::from_le_bytes([rec0[off + 32], rec0[off + 33]]) as usize;
                    let real_size = u64::from_le_bytes(rec0[off + 48..off + 56].try_into().unwrap());
                    if runs_off >= 64 && runs_off < attr_len {
                        let runs = decode_data_runs(&rec0[off + runs_off..off + attr_len], cluster_size)?;
                        if !runs.is_empty() {
                            return Ok((runs, real_size));
                        }
                    }
                }
            }
            off += attr_len;
        }

        Err("Couldn't locate NTFS $MFT data runs. Try the normal drive scan instead.".into())
    }

    fn namespace_priority(ns: u8) -> u8 {
        match ns {
            1 | 3 => 3, // Win32 or Win32+DOS (preferred)
            0 => 2,     // POSIX
            2 => 1,     // DOS 8.3 short name
            _ => 0,
        }
    }

    /// Parses one raw MFT record (applying USA fixup in-place).
    pub fn parse_mft_record(frn: u64, rec: &mut [u8]) -> ParsedRecord {
        if rec.len() < 48 || &rec[0..4] != b"FILE" {
            return ParsedRecord::Skip;
        }
        let flags = u16::from_le_bytes([rec[22], rec[23]]);
        if flags & 0x0001 == 0 {
            return ParsedRecord::Skip; // not in use
        }
        if !apply_usa_fixup(rec) {
            return ParsedRecord::Skip;
        }

        let is_dir = (flags & 0x0002) != 0;
        let base_raw = u64::from_le_bytes(rec[32..40].try_into().unwrap());
        let base_frn = base_raw & FRN_MASK;
        let is_ext = base_raw != 0;
        let first_attr = u16::from_le_bytes([rec[20], rec[21]]) as usize;
        let used = u32::from_le_bytes(rec[24..28].try_into().unwrap()) as usize;
        let limit = if used >= first_attr && used <= rec.len() { used } else { rec.len() };

        let mut best_name: Option<(u64, String, u8)> = None;
        let mut data_size: Option<u64> = None;

        let mut off = first_attr;
        while off + 4 <= limit {
            let attr_type = u32::from_le_bytes(rec[off..off + 4].try_into().unwrap());
            if attr_type == 0xFFFF_FFFF {
                break;
            }
            if off + 16 > limit {
                break;
            }
            let attr_len = u32::from_le_bytes(rec[off + 4..off + 8].try_into().unwrap()) as usize;
            if attr_len < 16 || off + attr_len > limit {
                break;
            }
            let non_res = rec[off + 8];
            let name_len = rec[off + 9];

            if attr_type == 0x30 && !is_ext && non_res == 0 && attr_len >= 24 {
                let val_len = u32::from_le_bytes(rec[off + 16..off + 20].try_into().unwrap()) as usize;
                let val_off = u16::from_le_bytes([rec[off + 20], rec[off + 21]]) as usize;
                if val_off >= 24 && val_len >= 66 && val_off + val_len <= attr_len {
                    let val = &rec[off + val_off..off + val_off + val_len];
                    let fn_len = val[64] as usize;
                    let ns = val[65];
                    let prio = namespace_priority(ns);
                    let need_take = match &best_name {
                        None => true,
                        Some(b) => prio > b.2,
                    };
                    if need_take && fn_len > 0 && 66 + fn_len * 2 <= val_len {
                        let parent_frn = u64::from_le_bytes(val[0..8].try_into().unwrap()) & FRN_MASK;
                        let u16s: Vec<u16> = val[66..66 + fn_len * 2]
                            .chunks_exact(2)
                            .map(|c| u16::from_le_bytes([c[0], c[1]]))
                            .collect();
                        let name = String::from_utf16_lossy(&u16s);
                        if !name.is_empty() {
                            best_name = Some((parent_frn, name, prio));
                        }
                    }
                }
            } else if attr_type == 0x80 && name_len == 0 {
                if non_res == 0 && attr_len >= 20 {
                    let sz = u32::from_le_bytes(rec[off + 16..off + 20].try_into().unwrap()) as u64;
                    data_size = Some(sz);
                } else if non_res == 1 && attr_len >= 56 {
                    let lowest_vcn = u64::from_le_bytes(rec[off + 16..off + 24].try_into().unwrap());
                    if lowest_vcn == 0 {
                        let real_size = u64::from_le_bytes(rec[off + 48..off + 56].try_into().unwrap());
                        data_size = Some(real_size);
                    }
                }
            }

            off += attr_len;
        }

        if is_ext {
            return match data_size {
                Some(sz) if base_frn >= 16 => ParsedRecord::ExtData(base_frn, sz),
                _ => ParsedRecord::Skip,
            };
        }

        // Skip NTFS system metadata records (FRN 0..15, including root FRN 5 which is added as node 0).
        if frn < 16 {
            return ParsedRecord::Skip;
        }
        let Some((parent_frn, name, _)) = best_name else {
            return ParsedRecord::Skip;
        };
        if parent_frn == NTFS_ROOT_FRN && name.starts_with('$') {
            return ParsedRecord::Skip;
        }

        ParsedRecord::Base(MftEntry {
            frn,
            parent_frn,
            name,
            is_dir,
            size: if is_dir { 0 } else { data_size.unwrap_or(0) },
        })
    }

    fn resolve_dir_node(
        frn: u64,
        dir_map: &HashMap<u64, (u64, &str)>,
        frn_to_node: &mut HashMap<u64, Option<usize>>,
        nodes: &mut Vec<Node>,
    ) -> Option<usize> {
        if let Some(&hit) = frn_to_node.get(&frn) {
            return hit;
        }
        let mut chain = Vec::new();
        let mut cur = frn;
        let base = loop {
            if let Some(&hit) = frn_to_node.get(&cur) {
                break hit;
            }
            if chain.len() >= 128 || chain.contains(&cur) {
                break None; // cycle or runaway depth guard
            }
            let Some(&(parent, _)) = dir_map.get(&cur) else {
                break None;
            };
            chain.push(cur);
            cur = parent;
        };
        let Some(mut parent_idx) = base else {
            for id in chain {
                frn_to_node.insert(id, None);
            }
            return None;
        };
        for id in chain.into_iter().rev() {
            let name = dir_map[&id].1;
            let idx = nodes.len();
            nodes.push(Node::new(name.to_string(), parent_idx));
            nodes[parent_idx].dirs.push(idx);
            frn_to_node.insert(id, Some(idx));
            parent_idx = idx;
        }
        Some(parent_idx)
    }

    /// Builds a `Tree` from collected MFT entries with iterative cycle-safe directory resolution
    /// and bottom-up size & file count rollup.
    pub fn build_tree_from_mft(drive: char, entries: &[MftEntry]) -> Tree {
        let mut nodes = vec![Node::new(format!("{drive}:\\"), NONE)];
        let mut dir_map: HashMap<u64, (u64, &str)> = HashMap::new();
        for e in entries {
            if e.is_dir && e.frn != NTFS_ROOT_FRN {
                dir_map.insert(e.frn, (e.parent_frn, e.name.as_str()));
            }
        }

        let mut frn_to_node: HashMap<u64, Option<usize>> = HashMap::with_capacity(dir_map.len() + 1);
        frn_to_node.insert(NTFS_ROOT_FRN, Some(0));

        // Ensure all reachable directories (including empty ones) exist in `nodes`.
        for e in entries {
            if e.is_dir && e.frn != NTFS_ROOT_FRN {
                let _ = resolve_dir_node(e.frn, &dir_map, &mut frn_to_node, &mut nodes);
            }
        }

        let mut top: BinaryHeap<Reverse<(u64, String)>> = BinaryHeap::new();
        for e in entries {
            if e.is_dir {
                continue;
            }
            let Some(parent_idx) = resolve_dir_node(e.parent_frn, &dir_map, &mut frn_to_node, &mut nodes) else {
                continue;
            };
            let size = e.size;
            {
                let node = &mut nodes[parent_idx];
                node.files += 1;
                node.size = node.size.saturating_add(size);
                if size >= BIG {
                    node.big.push((e.name.clone(), size));
                } else {
                    node.small_size = node.small_size.saturating_add(size);
                    node.small_count += 1;
                }
            }
            if size >= BIG {
                push_top(&mut top, size, || {
                    path_of(&nodes, parent_idx).join(&e.name).to_string_lossy().to_string()
                });
            }
        }

        // Ancestors are always created before descendants, so reverse rollup is exact.
        for i in (1..nodes.len()).rev() {
            let (size, files, parent) = (nodes[i].size, nodes[i].files, nodes[i].parent);
            nodes[parent].size = nodes[parent].size.saturating_add(size);
            nodes[parent].files = nodes[parent].files.saturating_add(files);
        }

        let mut top: Vec<(u64, String)> = top.into_iter().map(|r| r.0).collect();
        top.sort_by(|a, b| b.cmp(a));
        Tree { nodes, top }
    }

    pub fn write_tree_file(tree: &Tree, out_path: &Path) -> Result<(), String> {
        let stage_path = temp_stage_path(out_path);
        let write_res = (|| -> Result<(), String> {
            let file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&stage_path)
                .map_err(|e| format!("Couldn't create temp scan file: {e}"))?;
            let mut w = BufWriter::with_capacity(256 * 1024, file);
            serde_json::to_writer(&mut w, tree)
                .map_err(|e| format!("Couldn't serialize scan result: {e}"))?;
            w.flush().map_err(|e| format!("Couldn't flush scan result: {e}"))?;
            Ok(())
        })();

        if let Err(e) = write_res {
            let _ = std::fs::remove_file(&stage_path);
            return Err(e);
        }
        if let Err(e) = std::fs::rename(&stage_path, out_path) {
            let _ = std::fs::remove_file(&stage_path);
            return Err(format!("Couldn't finalize scan output: {e}"));
        }
        Ok(())
    }
}

#[cfg(windows)]
mod mft_disk_win {
    use super::ntfs_mft::*;
    use super::*;
    use std::collections::HashMap;
    use std::ffi::c_void;
    use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    };

    const GENERIC_READ: u32 = 0x8000_0000;
    const FILE_FLAG_SEQUENTIAL_SCAN: u32 = 0x0800_0000;
    const FILE_BEGIN: u32 = 0;
    const ERROR_ACCESS_DENIED: u32 = 5;
    const CHUNK_SIZE: usize = 4 * 1024 * 1024; // 4 MiB aligned sequential reads

    #[link(name = "kernel32")]
    extern "system" {
        fn SetFilePointerEx(
            h_file: HANDLE,
            li_distance_to_move: i64,
            lp_new_file_pointer: *mut i64,
            dw_move_method: u32,
        ) -> i32;
        fn ReadFile(
            h_file: HANDLE,
            lp_buffer: *mut c_void,
            n_number_of_bytes_to_read: u32,
            lp_number_of_bytes_read: *mut u32,
            lp_overlapped: *mut c_void,
        ) -> i32;
    }

    struct HandleGuard(HANDLE);

    impl Drop for HandleGuard {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }

    fn read_at(handle: HANDLE, offset: u64, buf: &mut [u8]) -> Result<usize, String> {
        let dist = i64::try_from(offset).map_err(|_| "Volume offset out of range.".to_string())?;
        let seek_ok = unsafe { SetFilePointerEx(handle, dist, std::ptr::null_mut(), FILE_BEGIN) };
        if seek_ok == 0 {
            let err = unsafe { GetLastError() };
            return Err(format!("Couldn't seek NTFS volume (Windows error {err})."));
        }
        let mut done = 0usize;
        while done < buf.len() {
            let to_read = (buf.len() - done).min(u32::MAX as usize) as u32;
            let mut got = 0u32;
            let ok = unsafe {
                ReadFile(
                    handle,
                    buf[done..].as_mut_ptr().cast::<c_void>(),
                    to_read,
                    &mut got,
                    std::ptr::null_mut(),
                )
            };
            if ok == 0 {
                let err = unsafe { GetLastError() };
                return Err(format!("Couldn't read NTFS volume (Windows error {err})."));
            }
            if got == 0 {
                break;
            }
            done += got as usize;
        }
        Ok(done)
    }

    pub fn scan_and_write(drive: char, out_path: &Path) -> Result<(), String> {
        let vol_str = format!(r"\\.\{drive}:");
        let wide: Vec<u16> = vol_str.encode_utf16().chain(std::iter::once(0)).collect();
        let handle = unsafe {
            CreateFileW(
                wide.as_ptr(),
                GENERIC_READ,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_SEQUENTIAL_SCAN,
                std::ptr::null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            let err = unsafe { GetLastError() };
            return Err(match err {
                ERROR_ACCESS_DENIED => {
                    format!("Access denied opening {drive}: (administrator permission is required).")
                }
                _ => format!("Couldn't open drive {drive}: (Windows error {err})."),
            });
        }
        let _guard = HandleGuard(handle);

        // 1. Read boot sector (4096 bytes covers both 512B and 4Kn sector drives).
        let mut boot_buf = [0u8; 4096];
        let got_boot = read_at(handle, 0, &mut boot_buf)?;
        if got_boot < 512 {
            return Err(format!("Couldn't read boot sector on {drive}:."));
        }
        let boot = parse_ntfs_boot_sector(&boot_buf[..got_boot], drive)?;

        // 2. Read MFT record #0 ($MFT) and decode its non-resident data runs.
        let rec_size = boot.record_size as usize;
        let rec0_len = rec_size.max(boot.bytes_per_sector as usize);
        let mut rec0_buf = vec![0u8; rec0_len];
        let got_rec0 = read_at(handle, boot.mft_byte_offset, &mut rec0_buf)?;
        if got_rec0 < rec_size {
            return Err(format!("Couldn't read NTFS $MFT record #0 on {drive}:."));
        }
        let (runs, mft_real_size) = extract_mft_runs(&mut rec0_buf[..rec_size], boot.cluster_size as u64)?;
        let max_bytes = if mft_real_size > 0 {
            mft_real_size.min(MAX_MFT_BYTES)
        } else {
            MAX_MFT_BYTES
        };

        // 3. Stream every MFT data run sequentially in 4 MiB aligned chunks.
        let est_records = ((max_bytes / rec_size as u64) as usize).min(2_000_000);
        let mut entries: Vec<MftEntry> = Vec::with_capacity(est_records / 2);
        let mut ext_sizes: HashMap<u64, u64> = HashMap::new();
        let mut chunk_buf = vec![0u8; CHUNK_SIZE];
        let mut carry: Vec<u8> = Vec::with_capacity(rec_size);
        let mut frn_index = 0u64;
        let mut bytes_processed = 0u64;

        'runs: for (phys_offset, run_len) in runs {
            let mut run_pos = 0u64;
            while run_pos < run_len {
                if bytes_processed >= max_bytes {
                    break 'runs;
                }
                let to_read = (run_len - run_pos).min(CHUNK_SIZE as u64) as usize;
                let got = read_at(handle, phys_offset + run_pos, &mut chunk_buf[..to_read])?;
                if got == 0 {
                    break;
                }
                run_pos += got as u64;

                let usable = ((max_bytes - bytes_processed) as usize).min(got);
                bytes_processed += usable as u64;
                let slice = &mut chunk_buf[..usable];

                let mut off = 0usize;
                if !carry.is_empty() {
                    let need = rec_size - carry.len();
                    if slice.len() < need {
                        carry.extend_from_slice(slice);
                        continue;
                    }
                    carry.extend_from_slice(&slice[..need]);
                    match parse_mft_record(frn_index, &mut carry) {
                        ParsedRecord::Base(e) => entries.push(e),
                        ParsedRecord::ExtData(base, sz) => {
                            ext_sizes.entry(base).or_insert(sz);
                        }
                        ParsedRecord::Skip => {}
                    }
                    frn_index += 1;
                    carry.clear();
                    off = need;
                }

                while off + rec_size <= slice.len() {
                    match parse_mft_record(frn_index, &mut slice[off..off + rec_size]) {
                        ParsedRecord::Base(e) => entries.push(e),
                        ParsedRecord::ExtData(base, sz) => {
                            ext_sizes.entry(base).or_insert(sz);
                        }
                        ParsedRecord::Skip => {}
                    }
                    frn_index += 1;
                    off += rec_size;
                }

                if off < slice.len() {
                    carry.extend_from_slice(&slice[off..]);
                }
            }
        }

        if !ext_sizes.is_empty() {
            for e in &mut entries {
                if !e.is_dir && e.size == 0 {
                    if let Some(&sz) = ext_sizes.get(&e.frn) {
                        e.size = sz;
                    }
                }
            }
        }

        let tree = build_tree_from_mft(drive, &entries);
        write_tree_file(&tree, out_path)
    }
}

/// Entry point called at the very top of `main.rs` before Tauri starts.
/// Returns `true` if `--disk-mft-scan` was requested (so `main()` exits immediately).
pub fn run_mft_disk_cli_if_requested() -> bool {
    let Some((drive_raw, out_raw)) = parse_dmft_cli_args(std::env::args()) else {
        return false;
    };

    // Refuse arbitrary file writes: only touch `swl-dmft-*.swl-dmft` directly inside temp_dir().
    let Some(out_path) = validate_dmft_out_path(&out_raw) else {
        return true;
    };
    let done_path = done_sentinel_path(&out_path);

    let Some(drive) = validate_dmft_cli_drive(&drive_raw) else {
        let _ = std::fs::write(&done_path, "ERR:Invalid drive letter.");
        return true;
    };

    #[cfg(windows)]
    {
        match mft_disk_win::scan_and_write(drive, &out_path) {
            Ok(()) => {
                let _ = std::fs::write(&done_path, "OK");
            }
            Err(msg) => {
                let _ = std::fs::remove_file(&out_path);
                let _ = std::fs::write(&done_path, format!("ERR:{msg}"));
            }
        }
    }

    #[cfg(not(windows))]
    {
        let _ = drive;
        let _ = std::fs::write(&done_path, "ERR:NTFS MFT scan is Windows-only.");
    }

    true
}

struct TempCleanup {
    paths: Vec<PathBuf>,
}

impl Drop for TempCleanup {
    fn drop(&mut self) {
        for p in &self.paths {
            let _ = std::fs::remove_file(p);
        }
    }
}

// ------------------------------------------------------------------ what may never be deleted

/// The real, absolute path (links followed) without Windows' \\?\ prefix, like Python's resolve().
fn resolve(p: &Path) -> PathBuf {
    let abs = std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf());
    let real = std::fs::canonicalize(&abs).ok().or_else(|| {
        let parent = std::fs::canonicalize(abs.parent()?).ok()?;
        Some(parent.join(abs.file_name()?))
    });
    let real = real.unwrap_or(abs);
    let s = real.to_string_lossy();
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        PathBuf::from(format!(r"\\{rest}"))
    } else if let Some(rest) = s.strip_prefix(r"\\?\") {
        PathBuf::from(rest)
    } else {
        real
    }
}

/// Path parts for comparing (Windows ignores upper/lower case).
fn parts(p: &Path) -> Vec<String> {
    p.components()
        .map(|c| {
            let s = c.as_os_str().to_string_lossy().to_string();
            if cfg!(windows) { s.to_lowercase() } else { s }
        })
        .collect()
}

fn same(a: &[String], b: &[String]) -> bool {
    a == b
}

fn inside(p: &[String], folder: &[String]) -> bool {
    p.len() > folder.len() && p.starts_with(folder)
}

/// Things the map never offers to delete: drive roots, Windows, programs, app data, your
/// profile's own folders (Documents, Desktop, ...) and Windows' own special files.
pub fn protected(path: &Path) -> bool {
    let real = resolve(path);
    if real.parent().is_none() {
        return true;
    }
    let p = parts(&real);
    let env = |name: &str| std::env::var_os(name).filter(|v| !v.is_empty()).map(PathBuf::from);
    if let Some(temp) = env("TEMP") {
        if inside(&p, &parts(&resolve(&temp))) {
            return false; // leftovers inside the Temp folder are fair game
        }
    }
    let defaults: &[(&str, &str)] = &[
        ("SystemRoot", r"C:\Windows"),
        ("ProgramFiles", r"C:\Program Files"),
        ("ProgramFiles(x86)", r"C:\Program Files (x86)"),
        ("ProgramData", r"C:\ProgramData"),
        ("APPDATA", ""),
        ("LOCALAPPDATA", ""),
    ];
    for (name, default) in defaults {
        let guarded = env(name).or_else(|| (cfg!(windows) && !default.is_empty()).then(|| PathBuf::from(default)));
        if let Some(g) = guarded {
            let g = parts(&resolve(&g));
            if same(&p, &g) || inside(&p, &g) {
                return true;
            }
        }
    }
    if let Some(home) = dirs::home_dir() {
        let home = resolve(&home);
        let h = parts(&home);
        let parent_is_home = real.parent().is_some_and(|x| same(&parts(x), &h));
        let is_users = home.parent().is_some_and(|x| same(&p, &parts(x)));
        if same(&p, &h) || parent_is_home || is_users {
            return true; // C:\Users, your profile, Documents/Desktop/...
        }
    }
    let name = real.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
    matches!(name.as_str(), "$recycle.bin" | "system volume information" | "pagefile.sys" | "hiberfil.sys" | "swapfile.sys")
}

// ------------------------------------------------------------------ Cleanup Advisor

struct AdviceItem {
    path: String,
    path_parts: Vec<String>,
    name: String,
    kind: &'static str,
    size: u64,
    reason: &'static str,
    is_protected: bool,
}

fn path_has_downloads(p: &Path) -> bool {
    p.components().any(|c| {
        matches!(c, Component::Normal(n) if n.to_string_lossy().eq_ignore_ascii_case("downloads"))
    })
}

fn classify_safe_dir(name_lc: &str, parent_lc: Option<&str>) -> Option<&'static str> {
    match name_lc {
        ".cache" => Some(if parent_lc == Some("node_modules") {
            "Node.js package cache (auto-rebuilt)"
        } else {
            "Rebuildable cache folder"
        }),
        "__pycache__" => Some("Python bytecode cache (auto-rebuilt)"),
        ".pytest_cache" => Some("Pytest cache (auto-rebuilt)"),
        ".parcel-cache" => Some("Parcel bundler cache (auto-rebuilt)"),
        ".turbo" => Some("Turborepo build cache (auto-rebuilt)"),
        "obj" => Some("Compiled build intermediates (auto-rebuilt)"),
        "shadercache" => Some("Graphics shader cache (auto-rebuilt)"),
        "gpucache" => Some("GPU cache (auto-rebuilt)"),
        "code cache" => Some("Code cache (auto-rebuilt)"),
        "cache" if parent_lc == Some(".next") => Some("Next.js build cache (auto-rebuilt)"),
        "caches" if parent_lc == Some(".gradle") => Some("Gradle dependency cache (auto-rebuilt)"),
        "debug" if parent_lc == Some("target") => Some("Debug build output (auto-rebuilt)"),
        _ => None,
    }
}

fn classify_safe_file(ext_lc: &str) -> Option<&'static str> {
    match ext_lc {
        "tmp" | "temp" => Some("Temporary file"),
        "bak" | "old" => Some("Backup or old copy"),
        "dmp" => Some("Crash dump file"),
        "log" => Some("Log file"),
        "crdownload" | "part" => Some("Incomplete browser download"),
        _ => None,
    }
}

fn classify_consider_dir(name_lc: &str, size: u64) -> Option<&'static str> {
    if size < (100 << 20) {
        return None;
    }
    match name_lc {
        "node_modules" => Some("Node.js packages (can be reinstalled)"),
        "target" => Some("Build output folder (can be rebuilt)"),
        "dist" | "build" => Some("Build output folder (can be rebuilt)"),
        "venv" | ".venv" => Some("Python virtual environment (can be recreated)"),
        _ => None,
    }
}

fn classify_consider_file(ext_lc: &str, size: u64, in_downloads: bool) -> Option<&'static str> {
    match ext_lc {
        "exe" | "msi" | "msix" | "cab" if size >= (20 << 20) => Some(if in_downloads {
            "Installer in Downloads (often safe to remove after installing)"
        } else {
            "Large installer file"
        }),
        "iso" | "img" | "dmg" if size >= (20 << 20) => Some(if in_downloads {
            "Disk image in Downloads"
        } else {
            "Large disk image"
        }),
        "zip" | "7z" | "rar" | "tar" | "gz" | "xz" if size >= (50 << 20) => Some(if in_downloads {
            "Large archive in Downloads"
        } else {
            "Large compressed archive"
        }),
        "mp4" | "mkv" | "mov" | "avi" | "webm" if size >= (250 << 20) => {
            Some("Large video or screen recording")
        }
        _ => None,
    }
}

fn classify_keep_dir(
    name_lc: &str,
    depth: usize,
    parent_lc: Option<&str>,
    grandparent_lc: Option<&str>,
    is_protected: bool,
) -> Option<&'static str> {
    match name_lc {
        "windows" if is_protected || depth == 1 => Some("Windows system files"),
        "program files" | "program files (x86)" if is_protected || depth == 1 => {
            Some("Installed programs")
        }
        "programdata" if is_protected || depth == 1 => Some("Shared program data"),
        "appdata" if is_protected || depth <= 3 => Some("App settings & data"),
        "system volume information" => Some("Windows restore points & volume metadata"),
        "$recycle.bin" => Some("Windows Recycle Bin"),
        "documents" | "pictures" | "desktop" | "music"
            if is_protected || depth == 1 || grandparent_lc == Some("users") =>
        {
            Some("Personal files")
        }
        "swiftlane" | "vault" if is_protected || depth <= 3 || parent_lc == Some("documents") => {
            Some("Personal files")
        }
        _ => None,
    }
}

fn classify_keep_file(name_lc: &str) -> Option<&'static str> {
    match name_lc {
        "pagefile.sys" | "hiberfil.sys" | "swapfile.sys" => {
            Some("Windows virtual memory / hibernation")
        }
        _ => None,
    }
}

/// Removes children whose ancestor directory is already present in the same tier,
/// then returns the top `ADVICE_LIMIT` items sorted by size descending.
fn finalize_tier(mut items: Vec<AdviceItem>) -> Vec<Value> {
    // Shallowest first so ancestor directories are kept before descendants.
    items.sort_by(|a, b| {
        a.path_parts
            .len()
            .cmp(&b.path_parts.len())
            .then_with(|| b.size.cmp(&a.size))
    });

    let mut kept_dirs: Vec<Vec<String>> = Vec::new();
    let mut deduped: Vec<AdviceItem> = Vec::with_capacity(items.len());
    for item in items {
        let shadowed = kept_dirs
            .iter()
            .any(|dir_parts| inside(&item.path_parts, dir_parts));
        if shadowed {
            continue;
        }
        if item.kind == "dir" {
            kept_dirs.push(item.path_parts.clone());
        }
        deduped.push(item);
    }

    deduped.sort_by(|a, b| b.size.cmp(&a.size).then_with(|| a.path.cmp(&b.path)));
    deduped.truncate(ADVICE_LIMIT);

    deduped
        .into_iter()
        .map(|it| {
            json!({
                "path": it.path,
                "name": it.name,
                "kind": it.kind,
                "size": it.size,
                "reason": it.reason,
                "protected": it.is_protected,
            })
        })
        .collect()
}

fn cached_dir_path<'a>(slot: &'a mut Option<PathBuf>, tree: &Tree, idx: usize) -> &'a Path {
    slot.get_or_insert_with(|| tree.path(idx)).as_path()
}

/// Computes the conservative, security-first Cleanup Advisor tiers for the scanned tree.
/// Any path where `protected(&path)` is true is NEVER placed in `safe` or `consider`.
fn advice_for(tree: &Tree) -> Value {
    const MIN_SAFE: u64 = 1 << 20;
    const MIN_KEEP: u64 = 10 << 20;

    let mut safe = Vec::new();
    let mut consider = Vec::new();
    let mut keep = Vec::new();

    // Traverse from root (0) via live `dirs` links so items removed via Recycle Bin never reappear.
    let mut stack: Vec<(usize, usize)> = vec![(0, 0)];
    while let Some((idx, depth)) = stack.pop() {
        let node = &tree.nodes[idx];
        let parent_idx = node.parent;
        let parent_lc = (parent_idx != NONE).then(|| tree.nodes[parent_idx].name.to_lowercase());
        let grandparent_lc = (parent_idx != NONE && tree.nodes[parent_idx].parent != NONE)
            .then(|| tree.nodes[tree.nodes[parent_idx].parent].name.to_lowercase());

        let mut dir_path_opt: Option<PathBuf> = None;

        if idx != 0 {
            let name_lc = node.name.to_lowercase();

            // Check `keep` directories (>= 10 MB).
            if node.size >= MIN_KEEP
                && matches!(
                    name_lc.as_str(),
                    "windows"
                        | "program files"
                        | "program files (x86)"
                        | "programdata"
                        | "appdata"
                        | "system volume information"
                        | "$recycle.bin"
                        | "documents"
                        | "pictures"
                        | "desktop"
                        | "music"
                        | "swiftlane"
                        | "vault"
                )
            {
                let p = cached_dir_path(&mut dir_path_opt, tree, idx);
                let is_prot = protected(p);
                if let Some(reason) = classify_keep_dir(
                    &name_lc,
                    depth,
                    parent_lc.as_deref(),
                    grandparent_lc.as_deref(),
                    is_prot,
                ) {
                    keep.push(AdviceItem {
                        path: p.to_string_lossy().to_string(),
                        path_parts: parts(p),
                        name: node.name.clone(),
                        kind: "dir",
                        size: node.size,
                        reason,
                        is_protected: is_prot,
                    });
                }
            }

            // Check `safe` directories (>= 1 MB, strictly !protected).
            if node.size >= MIN_SAFE {
                if let Some(reason) = classify_safe_dir(&name_lc, parent_lc.as_deref()) {
                    let p = cached_dir_path(&mut dir_path_opt, tree, idx);
                    if !protected(p) {
                        safe.push(AdviceItem {
                            path: p.to_string_lossy().to_string(),
                            path_parts: parts(p),
                            name: node.name.clone(),
                            kind: "dir",
                            size: node.size,
                            reason,
                            is_protected: false,
                        });
                    }
                }
            }

            // Check `consider` directories (>= 100 MB, strictly !protected).
            if let Some(reason) = classify_consider_dir(&name_lc, node.size) {
                let p = cached_dir_path(&mut dir_path_opt, tree, idx);
                if !protected(p) {
                    consider.push(AdviceItem {
                        path: p.to_string_lossy().to_string(),
                        path_parts: parts(p),
                        name: node.name.clone(),
                        kind: "dir",
                        size: node.size,
                        reason,
                        is_protected: false,
                    });
                }
            }
        }

        // Check big files (>= 1 MB) inside this folder.
        for (fname, fsize) in &node.big {
            let fsize = *fsize;
            if fsize < MIN_SAFE {
                continue;
            }
            let fname_lc = fname.to_lowercase();
            if fsize >= MIN_KEEP {
                if let Some(reason) = classify_keep_file(&fname_lc) {
                    let fp = cached_dir_path(&mut dir_path_opt, tree, idx).join(fname);
                    let is_prot = protected(&fp);
                    keep.push(AdviceItem {
                        path: fp.to_string_lossy().to_string(),
                        path_parts: parts(&fp),
                        name: fname.clone(),
                        kind: "file",
                        size: fsize,
                        reason,
                        is_protected: is_prot,
                    });
                    continue;
                }
            }

            let ext_lc = Path::new(&fname_lc)
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("");
            if ext_lc.is_empty() {
                continue;
            }

            if let Some(reason) = classify_safe_file(ext_lc) {
                let fp = cached_dir_path(&mut dir_path_opt, tree, idx).join(fname);
                if !protected(&fp) {
                    safe.push(AdviceItem {
                        path: fp.to_string_lossy().to_string(),
                        path_parts: parts(&fp),
                        name: fname.clone(),
                        kind: "file",
                        size: fsize,
                        reason,
                        is_protected: false,
                    });
                }
                continue;
            }

            // Pre-check size before constructing path for `consider` files.
            if fsize >= (20 << 20) {
                let dir_p = cached_dir_path(&mut dir_path_opt, tree, idx);
                let in_dl = path_has_downloads(dir_p);
                if let Some(reason) = classify_consider_file(ext_lc, fsize, in_dl) {
                    let fp = dir_p.join(fname);
                    if !protected(&fp) {
                        consider.push(AdviceItem {
                            path: fp.to_string_lossy().to_string(),
                            path_parts: parts(&fp),
                            name: fname.clone(),
                            kind: "file",
                            size: fsize,
                            reason,
                            is_protected: false,
                        });
                    }
                }
            }
        }

        for &child in &node.dirs {
            stack.push((child, depth + 1));
        }
    }

    json!({
        "safe": finalize_tier(safe),
        "consider": finalize_tier(consider),
        "keep": finalize_tier(keep),
    })
}

/// Moves a file or folder to the Recycle Bin (it can be restored from there). Never deletes for good.
pub fn to_recycle_bin(path: &Path) -> Result<(), String> {
    if protected(path) {
        return Err("Swiftlane doesn't delete this: it belongs to Windows, a program, or your user folder.".into());
    }
    if path.symlink_metadata().is_err() {
        return Err("It isn't there any more.".into());
    }
    if !has_recycle_bin(&resolve(path)) {
        // USB sticks and network drives have no Recycle Bin: it would be gone for good
        return Err("This drive has no Recycle Bin, so Swiftlane won't delete from it. Use File Explorer if you're sure.".into());
    }
    recycle::move_to_bin(path)?;
    if path.symlink_metadata().is_ok() {
        return Err("Windows couldn't move it to the Recycle Bin. It may be open in another program.".into());
    }
    Ok(())
}

#[cfg(windows)]
fn has_recycle_bin(real: &Path) -> bool {
    use std::os::windows::ffi::OsStrExt;
    let Some(Component::Prefix(prefix)) = real.components().next() else { return false };
    let drive = match prefix.kind() {
        std::path::Prefix::Disk(l) | std::path::Prefix::VerbatimDisk(l) => format!("{}:\\", l as char),
        _ => return false, // network paths
    };
    let wide: Vec<u16> = std::ffi::OsStr::new(&drive).encode_wide().chain(std::iter::once(0)).collect();
    unsafe { windows_sys::Win32::Storage::FileSystem::GetDriveTypeW(wide.as_ptr()) == 3 } // DRIVE_FIXED
}

#[cfg(not(windows))]
fn has_recycle_bin(_real: &Path) -> bool {
    false
}

#[cfg(windows)]
mod recycle {
    //! IFileOperation with "recycle", plus a watcher that stops the operation if Windows would delete
    //! something for good instead (too big for the Recycle Bin, or the Bin is turned off).
    use std::path::Path;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use windows::core::{implement, Ref, Result, HRESULT, HSTRING, PCWSTR};
    use windows::Win32::Foundation::E_ABORT;
    use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_ALL, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE};
    use windows::Win32::UI::Shell::*;

    #[implement(IFileOperationProgressSink)]
    struct Guard {
        refused: Arc<AtomicBool>,
    }

    #[allow(non_snake_case)]
    impl IFileOperationProgressSink_Impl for Guard_Impl {
        fn StartOperations(&self) -> Result<()> {
            Ok(())
        }
        fn FinishOperations(&self, _hr: HRESULT) -> Result<()> {
            Ok(())
        }
        fn PreRenameItem(&self, _f: u32, _i: Ref<IShellItem>, _n: &PCWSTR) -> Result<()> {
            Ok(())
        }
        fn PostRenameItem(&self, _f: u32, _i: Ref<IShellItem>, _n: &PCWSTR, _hr: HRESULT, _c: Ref<IShellItem>) -> Result<()> {
            Ok(())
        }
        fn PreMoveItem(&self, _f: u32, _i: Ref<IShellItem>, _d: Ref<IShellItem>, _n: &PCWSTR) -> Result<()> {
            Ok(())
        }
        fn PostMoveItem(&self, _f: u32, _i: Ref<IShellItem>, _d: Ref<IShellItem>, _n: &PCWSTR, _hr: HRESULT, _c: Ref<IShellItem>) -> Result<()> {
            Ok(())
        }
        fn PreCopyItem(&self, _f: u32, _i: Ref<IShellItem>, _d: Ref<IShellItem>, _n: &PCWSTR) -> Result<()> {
            Ok(())
        }
        fn PostCopyItem(&self, _f: u32, _i: Ref<IShellItem>, _d: Ref<IShellItem>, _n: &PCWSTR, _hr: HRESULT, _c: Ref<IShellItem>) -> Result<()> {
            Ok(())
        }
        /// Windows says here whether it will recycle the item; if not, stop before anything is lost.
        fn PreDeleteItem(&self, flags: u32, _i: Ref<IShellItem>) -> Result<()> {
            if flags & TSF_DELETE_RECYCLE_IF_POSSIBLE.0 as u32 == 0 {
                self.refused.store(true, Ordering::SeqCst);
                return Err(E_ABORT.into());
            }
            Ok(())
        }
        fn PostDeleteItem(&self, _f: u32, _i: Ref<IShellItem>, _hr: HRESULT, _c: Ref<IShellItem>) -> Result<()> {
            Ok(())
        }
        fn PreNewItem(&self, _f: u32, _d: Ref<IShellItem>, _n: &PCWSTR) -> Result<()> {
            Ok(())
        }
        fn PostNewItem(&self, _f: u32, _d: Ref<IShellItem>, _n: &PCWSTR, _t: &PCWSTR, _a: u32, _hr: HRESULT, _c: Ref<IShellItem>) -> Result<()> {
            Ok(())
        }
        fn UpdateProgress(&self, _total: u32, _so_far: u32) -> Result<()> {
            Ok(())
        }
        fn ResetTimer(&self) -> Result<()> {
            Ok(())
        }
        fn PauseTimer(&self) -> Result<()> {
            Ok(())
        }
        fn ResumeTimer(&self) -> Result<()> {
            Ok(())
        }
    }

    unsafe fn run(path: &Path, refused: &Arc<AtomicBool>) -> Result<bool> {
        let op: IFileOperation = CoCreateInstance(&FileOperation, None, CLSCTX_ALL)?;
        let flags = FOF_ALLOWUNDO.0 | FOFX_RECYCLEONDELETE.0 | FOF_NOCONFIRMATION.0 | FOF_SILENT.0 | FOF_NOERRORUI.0 | FOF_WANTNUKEWARNING.0 | FOFX_EARLYFAILURE.0;
        op.SetOperationFlags(FILEOPERATION_FLAGS(flags))?;
        let sink: IFileOperationProgressSink = Guard { refused: refused.clone() }.into();
        let cookie = op.Advise(&sink)?; // without the watcher, nothing happens
        let item: IShellItem = SHCreateItemFromParsingName(&HSTRING::from(path.as_os_str()), None)?;
        op.DeleteItem(&item, None)?;
        let done = op.PerformOperations();
        let aborted = op.GetAnyOperationsAborted().map(|b| b.as_bool()).unwrap_or(true);
        let _ = op.Unadvise(cookie);
        done?;
        Ok(!aborted)
    }

    pub fn move_to_bin(path: &Path) -> std::result::Result<(), String> {
        let path = path.to_path_buf();
        let refused = Arc::new(AtomicBool::new(false));
        let flag = refused.clone();
        // On its own thread, so the shell gets the single-threaded COM setup it expects.
        let ok = std::thread::spawn(move || unsafe {
            let init = CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE);
            let r = run(&path, &flag);
            if init.is_ok() {
                CoUninitialize();
            }
            r.unwrap_or(false)
        })
        .join()
        .unwrap_or(false);
        if refused.load(Ordering::SeqCst) {
            return Err("This is too big for the Recycle Bin (or the Recycle Bin is turned off for this drive), so Swiftlane didn't delete it. Use File Explorer if you're sure.".into());
        }
        if !ok {
            return Err("Windows couldn't move it to the Recycle Bin. It may be open in another program.".into());
        }
        Ok(())
    }
}

#[cfg(not(windows))]
mod recycle {
    pub fn move_to_bin(_path: &std::path::Path) -> Result<(), String> {
        Err("Moving to the Recycle Bin only works on Windows.".into())
    }
}

// ------------------------------------------------------------------ drives

fn usage(root: &Path) -> Option<Value> {
    let total = fs4::total_space(root).ok()?;
    let free_all = fs4::free_space(root).ok()?;
    // Like Python's shutil.disk_usage: Windows counts all free space, elsewhere what you may use.
    let free = if cfg!(windows) { free_all } else { fs4::available_space(root).ok()? };
    Some(json!({ "total": total, "used": total.saturating_sub(free_all), "free": free }))
}

fn merge(mut a: Value, b: Value) -> Value {
    if let (Value::Object(a), Value::Object(b)) = (&mut a, b) {
        a.extend(b);
    }
    a
}

#[cfg(windows)]
pub fn drives() -> Value {
    use windows_sys::Win32::Storage::FileSystem::{GetDriveTypeW, GetLogicalDrives, GetVolumeInformationW};
    let mut out = Vec::new();
    let mask = unsafe { GetLogicalDrives() };
    for i in 0..26u32 {
        if mask & (1 << i) == 0 {
            continue;
        }
        let root = format!("{}:\\", (b'A' + i as u8) as char);
        let wide: Vec<u16> = root.encode_utf16().chain(std::iter::once(0)).collect();
        let kind = unsafe { GetDriveTypeW(wide.as_ptr()) };
        if kind != 2 && kind != 3 {
            continue; // only removable and fixed (no network, CD or RAM disks)
        }
        let mut name = [0u16; 261];
        let ok = unsafe {
            GetVolumeInformationW(wide.as_ptr(), name.as_mut_ptr(), 261, std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut(), 0)
        };
        let len = name.iter().position(|&c| c == 0).unwrap_or(0);
        let label = if ok != 0 && len > 0 { String::from_utf16_lossy(&name[..len]) } else if kind == 2 { "USB drive".into() } else { "Local disk".into() };
        if let Some(u) = usage(Path::new(&root)) {
            out.push(merge(json!({ "path": root, "label": label, "removable": kind == 2 }), u));
        }
    }
    json!(out)
}

#[cfg(not(windows))]
pub fn drives() -> Value {
    json!(usage(Path::new("/")).map(|u| merge(json!({ "path": "/", "label": "/" }), u)).into_iter().collect::<Vec<_>>())
}

// ------------------------------------------------------------------ the page's calls

static DISK: Mutex<Option<Tree>> = Mutex::new(None); // the last scan

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

fn tail(s: &str, n: usize) -> String {
    let count = s.chars().count();
    s.chars().skip(count.saturating_sub(n)).collect()
}

fn view_of(tree: &Tree, path: Option<&str>) -> Value {
    let i = path.and_then(|p| tree.find(p)).unwrap_or(0);
    let mut v = tree.view(i, 150);
    v["root"] = json!(tree.root_name());
    v["advice"] = advice_for(tree);
    v["top"] = if i == 0 {
        json!(tree.top.iter().take(40).map(|(s, p)| json!({
            "name": Path::new(p).file_name().map(|n| n.to_string_lossy()).unwrap_or_default(),
            "path": p, "size": s, "protected": protected(Path::new(p)),
        })).collect::<Vec<_>>())
    } else {
        json!([])
    };
    if let Some(items) = v["items"].as_array_mut() {
        for item in items {
            let prot = item["path"].as_str().is_some_and(|p| protected(Path::new(p)));
            item["protected"] = json!(prot);
        }
    }
    v
}

fn disk_view(path: Option<&str>) -> Value {
    DISK.lock().unwrap().as_ref().map(|t| view_of(t, path)).unwrap_or(Value::Null)
}

/// Only paths from the current scan are ever revealed or deleted.
fn in_scan(tree: &Tree, path: &str) -> bool {
    !path.is_empty() && Path::new(path).parent().is_some_and(|p| tree.find(&p.to_string_lossy()).is_some())
}

fn scan_job(app: &AppHandle, path: &str) -> Value {
    let root = PathBuf::from(path);
    if path.is_empty() || !root.is_dir() {
        return json!({ "error": "That folder doesn't exist." });
    }
    *DISK.lock().unwrap() = None;
    let name = root.file_name().map(|n| n.to_string_lossy().to_string()).filter(|n| !n.is_empty()).unwrap_or_else(|| path.to_string());
    let id = app.state::<AppState>().jobs.start(app, "disk", &format!("Scan {name}"), false, move |ctx| {
        let tree = scan(&root, |n, current| ctx.report(None, Some(&format!("{} items · {}", grouped(n), tail(&current.to_string_lossy(), 70)))), || ctx.cancelled())
            .map_err(|ScanError::Cancelled| JobError::Cancelled)?;
        ctx.check()?; // cancelled right at the end: keep the old result off the screen
        let v = view_of(&tree, None);
        *DISK.lock().unwrap() = Some(tree);
        push(ctx.app(), "disk", v);
        Ok(Vec::new())
    });
    json!(id)
}

fn scan_mft_job(app: &AppHandle, raw_drive: &str) -> Value {
    let Some(drive) = parse_drive_letter(raw_drive) else {
        return json!({ "error": "Pick a valid drive letter (A–Z)." });
    };
    if !cfg!(windows) {
        return json!({ "error": "Fast NTFS MFT scan is only available on Windows." });
    }

    let mut rand_bytes = [0u8; 16];
    if getrandom::getrandom(&mut rand_bytes).is_err() {
        return json!({ "error": "Couldn't create temporary scan file." });
    }
    let token = hex::encode(rand_bytes);
    let out_path = std::env::temp_dir().join(format!("swl-dmft-{token}.swl-dmft"));
    let done_path = done_sentinel_path(&out_path);
    let stage_path = temp_stage_path(&out_path);

    let Ok(exe) = std::env::current_exe() else {
        return json!({ "error": "Couldn't locate Swiftlane.exe." });
    };
    let dir = exe
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(std::env::temp_dir);
    let params = format!("--disk-mft-scan {drive} \"{}\"", out_path.display());

    if !winapi::run_elevated(&exe, &params, &dir) {
        return json!({
            "error": "Administrator permission was cancelled. Click a drive card above to run the normal fast scan without admin."
        });
    }

    *DISK.lock().unwrap() = None;
    let id = app.state::<AppState>().jobs.start(
        app,
        "disk",
        &format!("Fast NTFS scan ({drive}:\\)"),
        false,
        move |ctx| {
            let _cleanup = TempCleanup {
                paths: vec![out_path.clone(), done_path.clone(), stage_path],
            };
            let started = Instant::now();
            let deadline = started + Duration::from_secs(DMFT_TIMEOUT_SECS);
            let mut last_report = Instant::now() - Duration::from_secs(1);

            loop {
                ctx.check()?;
                if done_path.exists() {
                    break;
                }
                if Instant::now() >= deadline {
                    return Err(JobError::Failed("NTFS MFT scan timed out.".into()));
                }
                if last_report.elapsed() >= Duration::from_millis(500) {
                    let secs = started.elapsed().as_secs();
                    ctx.report(
                        None,
                        Some(&format!("Reading NTFS $MFT on {drive}:\\ ({secs}s)…")),
                    );
                    last_report = Instant::now();
                }
                std::thread::sleep(Duration::from_millis(100));
            }

            let status = std::fs::read_to_string(&done_path)
                .map_err(|_| JobError::Failed("Couldn't read NTFS MFT scan status.".into()))?;
            let status = status.trim();
            if let Some(err) = status.strip_prefix("ERR:") {
                return Err(JobError::Failed(err.trim().to_string()));
            }
            if status != "OK" {
                return Err(JobError::Failed("NTFS MFT scan didn't finish cleanly.".into()));
            }

            ctx.report(None, Some("Loading scan results…"));
            let bytes = std::fs::read(&out_path)
                .map_err(|_| JobError::Failed("Couldn't read NTFS MFT scan output.".into()))?;
            let tree: Tree = serde_json::from_slice(&bytes)
                .map_err(|_| JobError::Failed("Couldn't decode NTFS MFT scan output.".into()))?;
            drop(_cleanup);

            ctx.check()?;
            let elapsed_ms = started.elapsed().as_millis();
            let files = tree.nodes[0].files;
            let v = view_of(&tree, None);
            *DISK.lock().unwrap() = Some(tree);
            config::log(&format!(
                "Disk map: scanned {drive}:\\ via NTFS $MFT ({files} files, {elapsed_ms} ms)"
            ));
            push(ctx.app(), "disk", v);
            Ok(Vec::new())
        },
    );
    json!(id)
}

/// Recycle Bin only (it can be restored), only for something from the current scan, never system folders.
fn trash(path: &str) -> Value {
    let size = {
        let disk = DISK.lock().unwrap();
        let Some(tree) = disk.as_ref().filter(|t| in_scan(t, path)) else { return json!({ "error": "Scan again first." }) };
        match tree.find(path) {
            Some(i) => tree.nodes[i].size,
            None => std::fs::symlink_metadata(path).ok().filter(|m| m.is_file()).map(|m| m.len()).unwrap_or(0),
        }
    };
    if let Err(e) = to_recycle_bin(Path::new(path)) {
        return json!({ "error": e });
    }
    config::log(&format!("Disk map: moved to the Recycle Bin: {path}"));
    let parent = Path::new(path).parent().map(|p| p.to_string_lossy().to_string());
    let mut disk = DISK.lock().unwrap();
    let Some(tree) = disk.as_mut() else { return Value::Null };
    tree.remove(path, size);
    view_of(tree, parent.as_deref())
}

pub fn dispatch(app: &AppHandle, method: &str, args: &[Value]) -> Option<Value> {
    let s = arg(args, 0).as_str().unwrap_or("");
    Some(match method {
        "disk_drives" => drives(),
        "disk_pick" => {
            let home = dirs::home_dir().unwrap_or_default();
            super::ask_folder(app, &home).map(|p| json!(p.to_string_lossy())).unwrap_or(Value::Null)
        }
        "disk_scan" => scan_job(app, s),
        "disk_scan_mft" => scan_mft_job(app, s),
        "disk_view" => disk_view(Some(s).filter(|s| !s.is_empty())),
        "disk_reveal" => {
            let ok = DISK.lock().unwrap().as_ref().is_some_and(|t| in_scan(t, s)) && Path::new(s).symlink_metadata().is_ok();
            if ok {
                winapi::reveal(Path::new(s));
            }
            Value::Null
        }
        "disk_trash" => trash(s),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::ntfs_mft::*;
    use super::*;

    fn fresh() -> PathBuf {
        let mut b = [0u8; 6];
        getrandom::getrandom(&mut b).unwrap();
        let dir = std::env::temp_dir().join(format!("swl-disk-test-{}", hex::encode(b)));
        std::fs::create_dir(&dir).unwrap();
        dir
    }

    #[test]
    fn scans_and_views() {
        let dir = fresh();
        std::fs::create_dir_all(dir.join("a/b")).unwrap();
        std::fs::create_dir(dir.join("empty")).unwrap();
        std::fs::write(dir.join("small.txt"), vec![1u8; 100]).unwrap();
        std::fs::write(dir.join("a/big.bin"), vec![0u8; 2 << 20]).unwrap();
        std::fs::write(dir.join("a/b/bigger.bin"), vec![0u8; 3 << 20]).unwrap();
        std::fs::write(dir.join("a/b/tiny.txt"), vec![1u8; 10]).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(dir.join("a"), dir.join("loop")).unwrap(); // never followed or counted

        let tree = scan(&dir, |_, _| {}, || false).ok().unwrap();
        let root = view_of(&tree, None);
        assert_eq!(root["size"], 100 + (2 << 20) + (3 << 20) + 10);
        assert_eq!(root["files"], 4);
        assert_eq!(root["items"][0]["name"], "a");
        assert_eq!(root["items"][0]["kind"], "dir");
        assert_eq!(root["items"][1]["kind"], "small");
        assert_eq!(root["top"][0]["name"], "bigger.bin");
        assert_eq!(root["top"].as_array().unwrap().len(), 2);
        assert_eq!(root["crumbs"].as_array().unwrap().len(), 1);

        let a = dir.join("a").to_string_lossy().to_string();
        let va = view_of(&tree, Some(&a));
        assert_eq!(va["path"], a.as_str());
        assert_eq!(va["crumbs"][1]["path"], a.as_str());
        assert_eq!(va["top"], json!([]));
        assert!(tree.find(&dir.join("a/../a").to_string_lossy()).is_none());
        assert!(tree.find("/somewhere/else").is_none());
        assert!(in_scan(&tree, &dir.join("a/big.bin").to_string_lossy()));
        assert!(!in_scan(&tree, &dir.join("nope/x").to_string_lossy()));

        let mut tree = tree;
        tree.remove(&dir.join("a/b").to_string_lossy(), 3 << 20 | 10);
        let root = view_of(&tree, None);
        assert_eq!(root["size"], 100 + (2 << 20));
        assert_eq!(root["files"], 2);
        assert_eq!(root["top"].as_array().unwrap().len(), 1);
        assert!(scan(&dir, |_, _| {}, || true).is_err(), "cancel stops the scan");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Checks the rules only; nothing on disk is created or removed.
    #[test]
    fn never_offers_system_folders() {
        assert!(protected(Path::new("/")));
        if let Some(home) = dirs::home_dir() {
            assert!(protected(&home));
            assert!(protected(&home.join("Documents")));
            assert!(protected(home.parent().unwrap()));
            assert!(!protected(&home.join("Documents").join("old stuff")));
        }
        assert!(protected(Path::new("/x/$RECYCLE.BIN")));
        assert!(protected(Path::new("/x/pagefile.sys")));
        assert_eq!(grouped(1234567), "1,234,567");
        assert_eq!(grouped(12), "12");
        assert_eq!(tail("abcdef", 3), "def");
    }

    #[test]
    fn parallel_scan_nested_dirs_and_cancellation() {
        let dir = fresh();
        for branch in ["alpha", "beta", "gamma", "delta"] {
            let sub = dir.join(branch).join("deep");
            std::fs::create_dir_all(&sub).unwrap();
            std::fs::write(sub.join("tiny.txt"), vec![7u8; 250]).unwrap();
        }
        std::fs::write(dir.join("alpha/one.dat"), vec![0u8; (1 << 20) + 128]).unwrap();
        std::fs::write(dir.join("gamma/deep/two.dat"), vec![0u8; 2 << 20]).unwrap();

        let tree = scan(&dir, |_, _| {}, || false).ok().unwrap();
        assert_eq!(tree.nodes[0].files, 6);
        assert_eq!(tree.nodes[0].size, 4 * 250 + (1 << 20) + 128 + (2 << 20));
        assert_eq!(tree.top.len(), 2);
        assert!(tree.top[0].1.ends_with("two.dat"));
        assert!(tree.top[1].1.ends_with("one.dat"));

        // Cancellation stops cleanly.
        assert!(scan(&dir, |_, _| {}, || true).is_err());

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn decodes_ntfs_data_runs_signed_and_sparse() {
        let cluster = 4096u64;
        // Run 1: len=1 byte (0x04 = 4 clusters), off=2 bytes (+0x0100 = +256 -> LCN 256)
        // Run 2 (sparse): len=1 byte (0x02), off=0 bytes -> skipped
        // Run 3 (negative signed offset): len=1 byte (0x03 = 3 clusters), off=1 byte (0xF6 = -10 -> LCN 246)
        // Terminator: 0x00
        let runs: &[u8] = &[
            0x21, 0x04, 0x00, 0x01,
            0x01, 0x02,
            0x11, 0x03, 0xF6,
            0x00,
            0xFF, // ignored after terminator
        ];
        let decoded = decode_data_runs(runs, cluster).unwrap();
        assert_eq!(
            decoded,
            vec![
                (256 * cluster, 4 * cluster),
                (246 * cluster, 3 * cluster),
            ]
        );

        // Verify NTFS boot sector parser (512B sector, 8 sectors/cluster = 4KiB, record_size = 2^10 = 1024).
        let mut boot = [0u8; 512];
        boot[3..11].copy_from_slice(b"NTFS    ");
        boot[11..13].copy_from_slice(&512u16.to_le_bytes());
        boot[13] = 8;
        boot[48..56].copy_from_slice(&786_432u64.to_le_bytes());
        boot[64] = (-10i8) as u8;
        let parsed = parse_ntfs_boot_sector(&boot, 'C').unwrap();
        assert_eq!(parsed.bytes_per_sector, 512);
        assert_eq!(parsed.cluster_size, 4096);
        assert_eq!(parsed.record_size, 1024);
        assert_eq!(parsed.mft_byte_offset, 786_432 * 4096);

        // Reject non-NTFS boot sector.
        boot[3..11].copy_from_slice(b"EXFAT   ");
        assert!(parse_ntfs_boot_sector(&boot, 'D').is_err());
    }

    /// Helper to craft a valid 1024-byte synthetic NTFS MFT FILE record with USA fixup.
    fn make_synthetic_mft_record(
        is_dir: bool,
        file_names: &[(u64, u8, &str)],
        resident_data: Option<u32>,
        non_resident_data: Option<(u64, u64)>, // (lowest_vcn, real_size)
    ) -> Vec<u8> {
        let mut rec = vec![0u8; 1024];
        rec[0..4].copy_from_slice(b"FILE");
        let usa_off: u16 = 48;
        let usa_count: u16 = 3; // 1 USN + 2 sectors (1024 / 512)
        rec[4..6].copy_from_slice(&usa_off.to_le_bytes());
        rec[6..8].copy_from_slice(&usa_count.to_le_bytes());
        let first_attr: u16 = 56;
        rec[20..22].copy_from_slice(&first_attr.to_le_bytes());
        let flags: u16 = if is_dir { 0x0003 } else { 0x0001 };
        rec[22..24].copy_from_slice(&flags.to_le_bytes());

        let mut off = first_attr as usize;
        for &(parent_frn, ns, name) in file_names {
            let utf16: Vec<u16> = name.encode_utf16().collect();
            let val_len = 66 + utf16.len() * 2;
            let val_off = 24usize;
            let attr_len = (val_off + val_len + 7) & !7;
            rec[off..off + 4].copy_from_slice(&0x30u32.to_le_bytes());
            rec[off + 4..off + 8].copy_from_slice(&(attr_len as u32).to_le_bytes());
            rec[off + 8] = 0; // resident
            rec[off + 16..off + 20].copy_from_slice(&(val_len as u32).to_le_bytes());
            rec[off + 20..off + 22].copy_from_slice(&(val_off as u16).to_le_bytes());
            let v = &mut rec[off + val_off..off + val_off + val_len];
            v[0..8].copy_from_slice(&parent_frn.to_le_bytes());
            v[64] = utf16.len() as u8;
            v[65] = ns;
            for (i, &u) in utf16.iter().enumerate() {
                v[66 + i * 2..66 + i * 2 + 2].copy_from_slice(&u.to_le_bytes());
            }
            off += attr_len;
        }
        if let Some(sz) = resident_data {
            let attr_len = 24usize;
            rec[off..off + 4].copy_from_slice(&0x80u32.to_le_bytes());
            rec[off + 4..off + 8].copy_from_slice(&(attr_len as u32).to_le_bytes());
            rec[off + 8] = 0; // resident
            rec[off + 9] = 0; // unnamed
            rec[off + 16..off + 20].copy_from_slice(&sz.to_le_bytes());
            off += attr_len;
        }
        if let Some((lowest_vcn, real_size)) = non_resident_data {
            let attr_len = 64usize;
            rec[off..off + 4].copy_from_slice(&0x80u32.to_le_bytes());
            rec[off + 4..off + 8].copy_from_slice(&(attr_len as u32).to_le_bytes());
            rec[off + 8] = 1; // non-resident
            rec[off + 9] = 0; // unnamed
            rec[off + 16..off + 24].copy_from_slice(&lowest_vcn.to_le_bytes());
            rec[off + 48..off + 56].copy_from_slice(&real_size.to_le_bytes());
            off += attr_len;
        }
        rec[off..off + 4].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
        let used = (off + 4) as u32;
        rec[24..28].copy_from_slice(&used.to_le_bytes());

        // Populate USA fixup: save original sector-end bytes, stamp USN [0xAB, 0xCD].
        let usn = [0xABu8, 0xCDu8];
        rec[48] = usn[0];
        rec[49] = usn[1];
        rec[50] = rec[510];
        rec[51] = rec[511];
        rec[52] = rec[1022];
        rec[53] = rec[1023];
        rec[510] = usn[0];
        rec[511] = usn[1];
        rec[1022] = usn[0];
        rec[1023] = usn[1];
        rec
    }

    #[test]
    #[cfg(windows)] // builds Windows-style paths
    fn parses_mft_records_usa_fixup_and_builds_tree_with_cycle_guard() {
        // DOS (ns=2) listed before Win32 (ns=1): Win32 name must win! Non-resident $DATA size.
        let mut r_big = make_synthetic_mft_record(
            false,
            &[(100, 2, "LONGFI~1.ISO"), (100, 1, "ubuntu-desktop.iso")],
            None,
            Some((0, 50 << 20)),
        );
        let ParsedRecord::Base(e_big) = parse_mft_record(200, &mut r_big) else {
            panic!("expected parsed base record");
        };
        assert_eq!(e_big.name, "ubuntu-desktop.iso");
        assert_eq!(e_big.size, 50 << 20);
        assert_eq!(e_big.parent_frn, 100);
        assert!(!e_big.is_dir);

        // Resident $DATA size.
        let mut r_small = make_synthetic_mft_record(false, &[(100, 3, "note.txt")], Some(420), None);
        let ParsedRecord::Base(e_small) = parse_mft_record(201, &mut r_small) else {
            panic!("expected parsed small record");
        };
        assert_eq!(e_small.size, 420);

        // Broken USA fixup must be rejected.
        let mut r_bad = make_synthetic_mft_record(false, &[(100, 1, "bad.bin")], Some(10), None);
        r_bad[510] ^= 0xFF;
        assert!(matches!(parse_mft_record(202, &mut r_bad), ParsedRecord::Skip));

        // Build tree including a cyclic pair of orphan dirs (300 <-> 301): must terminate & ignore cycle.
        let entries = vec![
            MftEntry { frn: 100, parent_frn: NTFS_ROOT_FRN, name: "Downloads".into(), is_dir: true, size: 0 },
            MftEntry { frn: 300, parent_frn: 301, name: "loopA".into(), is_dir: true, size: 0 },
            MftEntry { frn: 301, parent_frn: 300, name: "loopB".into(), is_dir: true, size: 0 },
            MftEntry { frn: 302, parent_frn: 300, name: "orphan.bin".into(), is_dir: false, size: 9 << 20 },
            e_big,
            e_small,
        ];
        let tree = build_tree_from_mft('C', &entries);
        assert_eq!(tree.root_name(), r"C:\");
        assert_eq!(tree.nodes[0].files, 2);
        assert_eq!(tree.nodes[0].size, (50 << 20) + 420);
        assert_eq!(tree.top.len(), 1);
        assert_eq!(tree.top[0].1, r"C:\Downloads\ubuntu-desktop.iso");
    }

    #[test]
    fn validates_dmft_cli_args_strictly() {
        assert_eq!(validate_dmft_cli_drive("c"), Some('C'));
        assert_eq!(validate_dmft_cli_drive("Z"), Some('Z'));
        assert_eq!(validate_dmft_cli_drive("C:"), None);
        assert_eq!(validate_dmft_cli_drive("C:\\"), None);
        assert_eq!(validate_dmft_cli_drive(""), None);
        assert_eq!(validate_dmft_cli_drive("1"), None);

        let valid = std::env::temp_dir().join("swl-dmft-abcdef012345.swl-dmft");
        assert!(validate_dmft_out_path(&valid.to_string_lossy()).is_some());

        // Reject traversal, wrong prefix/suffix, wrong directory, bad chars.
        let trav = std::env::temp_dir().join("..").join("swl-dmft-abc.swl-dmft");
        assert!(validate_dmft_out_path(&trav.to_string_lossy()).is_none());
        let bad_prefix = std::env::temp_dir().join("evil-abc.swl-dmft");
        assert!(validate_dmft_out_path(&bad_prefix.to_string_lossy()).is_none());
        let bad_suffix = std::env::temp_dir().join("swl-dmft-abc.txt");
        assert!(validate_dmft_out_path(&bad_suffix.to_string_lossy()).is_none());
        let bad_chars = std::env::temp_dir().join("swl-dmft-abc$1.swl-dmft");
        assert!(validate_dmft_out_path(&bad_chars.to_string_lossy()).is_none());
        assert!(validate_dmft_out_path(r"C:\Windows\swl-dmft-abc.swl-dmft").is_none());
        assert!(validate_dmft_out_path("").is_none());
    }

    #[test]
    fn cleanup_advisor_classifies_and_respects_protected_guard() {
        // 1. Unprotected workspace inside temp_dir(): safe & consider classifications + dedup.
        let dir = fresh();
        let mut nodes = vec![Node::new(dir.to_string_lossy().to_string(), NONE)];

        let dl_idx = nodes.len();
        nodes.push(Node::new("Downloads".into(), 0));
        nodes[0].dirs.push(dl_idx);
        nodes[dl_idx].big.push(("ubuntu.iso".into(), 50 << 20));
        nodes[dl_idx].size = 50 << 20;
        nodes[dl_idx].files = 1;

        let work_idx = nodes.len();
        nodes.push(Node::new("work".into(), 0));
        nodes[0].dirs.push(work_idx);
        nodes[work_idx].big.push(("crash.tmp".into(), 5 << 20));
        nodes[work_idx].size = 5 << 20;
        nodes[work_idx].files = 1;

        let nm_idx = nodes.len();
        nodes.push(Node::new("node_modules".into(), work_idx));
        nodes[work_idx].dirs.push(nm_idx);
        nodes[nm_idx].size = 150 << 20;
        nodes[nm_idx].files = 500;

        // Nested node_modules inside node_modules must be deduped under its ancestor in `consider`.
        let nested_nm_idx = nodes.len();
        nodes.push(Node::new("node_modules".into(), nm_idx));
        nodes[nm_idx].dirs.push(nested_nm_idx);
        nodes[nested_nm_idx].size = 110 << 20;
        nodes[nested_nm_idx].files = 200;

        let cache_idx = nodes.len();
        nodes.push(Node::new(".cache".into(), nm_idx));
        nodes[nm_idx].dirs.push(cache_idx);
        nodes[cache_idx].size = 12 << 20;
        nodes[cache_idx].files = 40;

        nodes[0].size = 220 << 20;
        nodes[0].files = 742;

        let tree = Tree { nodes, top: Vec::new() };
        let adv = advice_for(&tree);
        let safe = adv["safe"].as_array().unwrap();
        let consider = adv["consider"].as_array().unwrap();

        assert!(safe.iter().any(|x| x["name"] == "crash.tmp"));
        assert!(safe.iter().any(|x| x["name"] == ".cache"));
        assert!(consider.iter().any(|x| x["name"] == "ubuntu.iso"));
        assert_eq!(
            consider.iter().filter(|x| x["name"] == "node_modules").count(),
            1,
            "nested node_modules must be deduplicated under ancestor"
        );
        for item in safe.iter().chain(consider.iter()) {
            let p = Path::new(item["path"].as_str().unwrap());
            assert!(!protected(p), "protected path leaked into removable tier: {p:?}");
            assert_eq!(item["protected"], false);
        }

        // 2. System tree: Windows and pagefile.sys go into `keep`, and files inside Windows
        //    never leak into `safe` or `consider`.
        let sys_entries = vec![
            MftEntry { frn: 20, parent_frn: NTFS_ROOT_FRN, name: "Windows".into(), is_dir: true, size: 0 },
            MftEntry { frn: 21, parent_frn: 20, name: "install.tmp".into(), is_dir: false, size: 15 << 20 },
            MftEntry { frn: 22, parent_frn: 20, name: "big.log".into(), is_dir: false, size: 500 << 20 },
            MftEntry { frn: 23, parent_frn: NTFS_ROOT_FRN, name: "pagefile.sys".into(), is_dir: false, size: 4u64 << 30 },
        ];
        let sys_tree = build_tree_from_mft('C', &sys_entries);
        let sys_adv = advice_for(&sys_tree);
        let sys_keep = sys_adv["keep"].as_array().unwrap();
        assert!(sys_keep.iter().any(|x| x["name"] == "Windows"));
        assert!(sys_keep.iter().any(|x| x["name"] == "pagefile.sys"));
        #[cfg(windows)]
        {
            assert!(sys_adv["safe"].as_array().unwrap().is_empty());
            assert!(sys_adv["consider"].as_array().unwrap().is_empty());
        }

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
