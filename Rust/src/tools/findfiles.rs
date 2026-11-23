//! Instant File Search ("Find files"): blazing-fast in-memory search across either
//! a no-admin parallel folder index or an optional elevated NTFS MFT scan (UAC).
//!
//! Security & reliability rules:
//! - Swiftlane itself never runs elevated. Only the optional NTFS MFT scan spawns a
//!   short-lived elevated `Swiftlane.exe --mft-scan <DRIVE> "<temp_out_path>"` helper.
//! - The elevated helper strictly validates `<DRIVE>` (single ASCII letter A..=Z) and
//!   `<temp_out_path>` (single filename `swl-mft-*.swl-mft` directly inside `std::env::temp_dir()`),
//!   opens `\\.\<DRIVE>:` strictly read-only (`GENERIC_READ`), writes compact TSV output
//!   atomically, and exits immediately before Tauri ever starts.
//! - Temp files are always cleaned up immediately (even on cancel or error).
//! - `find_open` and `find_reveal` only act on paths present in the current index.

use serde_json::{json, Value};
#[cfg(any(windows, test))]
use std::collections::HashMap;
use std::io::{BufRead, BufReader};
#[cfg(any(windows, test))]
use std::io::{BufWriter, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Manager};

use super::diskmap::drives;
use super::{arg, ask_folder};
use crate::api::{push, AppState};
use crate::config;
use crate::jobs::JobError;
use crate::winapi;

const DEFAULT_LIMIT: usize = 150;
const MAX_LIMIT: usize = 500;
const MFT_TIMEOUT_SECS: u64 = 120;

#[cfg(windows)]
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;

struct Entry {
    path: String,
    name_lower: String,
    path_lower: String,
    ext_lower: String,
    is_dir: bool,
    size: Option<u64>,
    modified: Option<i64>,
}

struct FileIndex {
    entries: Vec<Entry>,
    mode: String, // "" | "folder" | "mft"
    roots: Vec<String>,
    indexed_at: i64,
    elapsed_ms: u64,
}

impl FileIndex {
    const fn empty() -> Self {
        FileIndex {
            entries: Vec::new(),
            mode: String::new(),
            roots: Vec::new(),
            indexed_at: 0,
            elapsed_ms: 0,
        }
    }
}

static INDEX: Mutex<FileIndex> = Mutex::new(FileIndex::empty());

// ------------------------------------------------------------------ path & formatting helpers

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn time_to_unix(t: Option<SystemTime>) -> Option<i64> {
    t?.duration_since(UNIX_EPOCH).ok().map(|d| d.as_secs() as i64)
}

fn grouped(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn split_name_dir(path: &str) -> (&str, &str) {
    let trimmed = path.trim_end_matches(['\\', '/']);
    if trimmed.is_empty() {
        return (path, "");
    }
    match trimmed.rfind(['\\', '/']) {
        Some(pos) => {
            let name = &trimmed[pos + 1..];
            let dir = if pos == 0 {
                &trimmed[..1]
            } else if pos == 2 && trimmed.as_bytes().get(1) == Some(&b':') {
                // Keep trailing backslash on Windows drive roots like "C:\"
                &path[..=pos]
            } else {
                &trimmed[..pos]
            };
            (name, dir)
        }
        None => (trimmed, ""),
    }
}

fn ext_of_lower(name_lower: &str) -> String {
    match name_lower.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() && !ext.is_empty() => ext.to_string(),
        _ => String::new(),
    }
}

fn make_entry(path: String, is_dir: bool, size: Option<u64>, modified: Option<i64>) -> Entry {
    let (name, _) = split_name_dir(&path);
    let name_lower = name.to_lowercase();
    let path_lower = path.to_lowercase();
    let ext_lower = if is_dir {
        String::new()
    } else {
        ext_of_lower(&name_lower)
    };
    Entry {
        path,
        name_lower,
        path_lower,
        ext_lower,
        is_dir,
        size,
        modified,
    }
}

fn should_skip_dir_name(name: &str) -> bool {
    name.eq_ignore_ascii_case("$Recycle.Bin")
        || name.eq_ignore_ascii_case("System Volume Information")
}

fn state_summary() -> Value {
    let idx = INDEX.lock().unwrap();
    json!({
        "indexed_count": idx.entries.len(),
        "mode": idx.mode,
        "roots": idx.roots,
        "indexed_at": idx.indexed_at,
        "elapsed_ms": idx.elapsed_ms,
        "drives": drives(),
    })
}

fn in_index(path: &str) -> bool {
    if path.is_empty() {
        return false;
    }
    INDEX.lock().unwrap().entries.iter().any(|e| e.path == path)
}

// ------------------------------------------------------------------ Mode 1: no-admin parallel walk

struct QueueState {
    dirs: Vec<PathBuf>,
    active: usize,
}

#[cfg(windows)]
fn is_reparse_point(entry: &std::fs::DirEntry) -> bool {
    use std::os::windows::fs::MetadataExt;
    entry
        .metadata()
        .map(|m| m.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0)
        .unwrap_or(false)
}

#[cfg(not(windows))]
fn is_reparse_point(entry: &std::fs::DirEntry) -> bool {
    entry.file_type().map(|t| t.is_symlink()).unwrap_or(false)
}

/// Walks `root` in parallel across up to 8 worker threads without requiring admin rights.
/// Skips symlinks, Windows reparse points (`FILE_ATTRIBUTE_REPARSE_POINT`), `$Recycle.Bin`,
/// and `System Volume Information` so directory traversal can never loop.
fn scan_folder_parallel<P, C>(
    root: &Path,
    progress: P,
    cancelled: C,
) -> Result<Vec<Entry>, JobError>
where
    P: Fn(u64) + Sync,
    C: Fn() -> bool + Sync,
{
    let workers = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .clamp(1, 8);
    let queue = Arc::new((
        Mutex::new(QueueState {
            dirs: vec![root.to_path_buf()],
            active: 0,
        }),
        Condvar::new(),
    ));
    let seen = AtomicU64::new(0);
    let buckets: Mutex<Vec<Vec<Entry>>> = Mutex::new(Vec::with_capacity(workers));

    std::thread::scope(|scope| {
        for _ in 0..workers {
            let queue = Arc::clone(&queue);
            let seen = &seen;
            let progress = &progress;
            let cancelled = &cancelled;
            let buckets = &buckets;
            scope.spawn(move || {
                let mut local: Vec<Entry> = Vec::with_capacity(1024);
                let mut new_dirs: Vec<PathBuf> = Vec::new();
                loop {
                    if cancelled() {
                        queue.1.notify_all();
                        break;
                    }
                    let dir = {
                        let (lock, cv) = &*queue;
                        let mut q = lock.lock().unwrap();
                        loop {
                            if cancelled() {
                                cv.notify_all();
                                break None;
                            }
                            if let Some(d) = q.dirs.pop() {
                                q.active += 1;
                                break Some(d);
                            }
                            if q.active == 0 {
                                cv.notify_all();
                                break None;
                            }
                            let (guard, _) = cv.wait_timeout(q, Duration::from_millis(40)).unwrap();
                            q = guard;
                        }
                    };
                    let Some(dir) = dir else { break };

                    new_dirs.clear();
                    let mut batch_count = 0u64;
                    if let Ok(rd) = std::fs::read_dir(&dir) {
                        for item in rd {
                            if batch_count & 0xFF == 0 && cancelled() {
                                break;
                            }
                            let Ok(de) = item else { continue };
                            let Ok(ft) = de.file_type() else { continue };
                            if ft.is_symlink() {
                                continue;
                            }
                            let is_dir = ft.is_dir();
                            if is_dir && is_reparse_point(&de) {
                                continue;
                            }
                            let name = de.file_name();
                            let name_str = name.to_string_lossy();
                            if is_dir && should_skip_dir_name(&name_str) {
                                continue;
                            }
                            let path = de.path();
                            if is_dir {
                                new_dirs.push(path.clone());
                            }
                            local.push(make_entry(
                                path.to_string_lossy().into_owned(),
                                is_dir,
                                None,
                                None,
                            ));
                            batch_count += 1;
                        }
                    }

                    if batch_count > 0 {
                        let prev = seen.fetch_add(batch_count, Ordering::Relaxed);
                        let now = prev + batch_count;
                        if now / 2000 != prev / 2000 {
                            progress(now);
                        }
                    }

                    let (lock, cv) = &*queue;
                    let mut q = lock.lock().unwrap();
                    q.active -= 1;
                    if !new_dirs.is_empty() && !cancelled() {
                        q.dirs.append(&mut new_dirs);
                        cv.notify_all();
                    } else if q.dirs.is_empty() && q.active == 0 {
                        cv.notify_all();
                    }
                }
                if !local.is_empty() {
                    buckets.lock().unwrap().push(local);
                }
            });
        }
    });

    if cancelled() {
        return Err(JobError::Cancelled);
    }

    let parts = buckets.into_inner().unwrap();
    let total: usize = parts.iter().map(Vec::len).sum();
    let mut entries = Vec::with_capacity(total);
    for mut part in parts {
        entries.append(&mut part);
    }
    Ok(entries)
}

fn start_folder_index(app: &AppHandle, raw_path: &str) -> Value {
    let root = if raw_path.trim().is_empty() {
        match dirs::home_dir() {
            Some(h) => h,
            None => return json!({ "error": "Couldn't find your home folder." }),
        }
    } else {
        PathBuf::from(raw_path.trim())
    };
    if !root.is_dir() {
        return json!({ "error": "That folder doesn't exist." });
    }
    let root_str = root.to_string_lossy().into_owned();
    let label = root
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| root_str.clone());

    let id = app.state::<AppState>().jobs.start(
        app,
        "findfiles",
        &format!("Index {label}"),
        false,
        move |ctx| {
            let started = Instant::now();
            let entries = scan_folder_parallel(
                &root,
                |count| {
                    ctx.report(
                        None,
                        Some(&format!("Scanning {} items…", grouped(count))),
                    );
                },
                || ctx.cancelled(),
            )?;
            ctx.check()?;
            let elapsed_ms = started.elapsed().as_millis() as u64;
            let count = entries.len();
            {
                let mut idx = INDEX.lock().unwrap();
                idx.entries = entries;
                idx.mode = "folder".into();
                idx.roots = vec![root_str.clone()];
                idx.indexed_at = unix_now();
                idx.elapsed_ms = elapsed_ms;
            }
            config::log(&format!(
                "Find files: indexed {count} items in {root_str} ({elapsed_ms} ms, folder)"
            ));
            push(ctx.app(), "findfiles", state_summary());
            Ok(Vec::new())
        },
    );
    json!(id)
}

// ------------------------------------------------------------------ Mode 2: optional UAC NTFS MFT scan

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

/// Strictly validates the `--mft-scan` CLI `<drive>` argument: exactly 1 ASCII letter A..=Z.
fn validate_mft_cli_drive(input: &str) -> Option<char> {
    let bytes = input.as_bytes();
    if bytes.len() == 1 && bytes[0].is_ascii_alphabetic() {
        Some((bytes[0] as char).to_ascii_uppercase())
    } else {
        None
    }
}

/// Strictly validates the `--mft-scan` CLI `<out_path>` argument:
/// must be a single `swl-mft-*.swl-mft` file directly inside `std::env::temp_dir()`.
fn validate_mft_out_path(raw: &str) -> Option<PathBuf> {
    if raw.is_empty() {
        return None;
    }
    let p = Path::new(raw);
    if p.components().any(|c| matches!(c, Component::ParentDir)) {
        return None;
    }
    let fname = p.file_name()?.to_str()?;
    if !fname.starts_with("swl-mft-") || !fname.ends_with(".swl-mft") {
        return None;
    }
    if fname.len() <= "swl-mft-.swl-mft".len() {
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

fn parse_mft_cli_args<I: IntoIterator<Item = String>>(args: I) -> Option<(String, String)> {
    let mut it = args.into_iter().skip(1);
    while let Some(a) = it.next() {
        if a == "--mft-scan" {
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
fn join_win_path(parent: &str, name: &str) -> String {
    if parent.ends_with('\\') {
        format!("{parent}{name}")
    } else {
        format!("{parent}\\{name}")
    }
}

/// Iterative directory path reconstruction with memoization and cycle guard.
#[cfg(any(windows, test))]
fn resolve_mft_dir(
    frn: u64,
    dirs: &HashMap<u64, (u64, String)>,
    cache: &mut HashMap<u64, Option<String>>,
) -> Option<String> {
    if let Some(hit) = cache.get(&frn) {
        return hit.clone();
    }
    let mut chain = Vec::new();
    let mut cur = frn;
    let base: Option<String> = loop {
        if let Some(hit) = cache.get(&cur) {
            break hit.clone();
        }
        if chain.len() >= 128 || chain.contains(&cur) {
            break None;
        }
        let Some((parent, _)) = dirs.get(&cur) else {
            break None;
        };
        chain.push(cur);
        cur = *parent;
    };
    let mut built = base;
    for id in chain.into_iter().rev() {
        built = match (built, dirs.get(&id)) {
            (Some(p), Some((_, name))) => Some(join_win_path(&p, name)),
            _ => None,
        };
        cache.insert(id, built.clone());
    }
    built
}

#[cfg(any(windows, test))]
fn write_mft_tsv(
    drive: char,
    dirs: &HashMap<u64, (u64, String)>,
    records: &[(u64, String, bool)],
    out_path: &Path,
) -> Result<(), String> {
    const NTFS_ROOT_FRN: u64 = 5;
    let mut cache: HashMap<u64, Option<String>> = HashMap::with_capacity(dirs.len() + 1);
    cache.insert(NTFS_ROOT_FRN, Some(format!("{drive}:\\")));

    let stage_path = temp_stage_path(out_path);
    let write_res = (|| -> std::io::Result<()> {
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&stage_path)?;
        let mut w = BufWriter::with_capacity(128 * 1024, file);
        for (parent_frn, name, is_dir) in records {
            let Some(parent_path) = resolve_mft_dir(*parent_frn, dirs, &mut cache) else {
                continue;
            };
            let full = join_win_path(&parent_path, name);
            let tag = if *is_dir { "D\t" } else { "F\t" };
            w.write_all(tag.as_bytes())?;
            w.write_all(full.as_bytes())?;
            w.write_all(b"\n")?;
        }
        w.flush()?;
        Ok(())
    })();

    if let Err(e) = write_res {
        let _ = std::fs::remove_file(&stage_path);
        return Err(format!("Couldn't write scan output: {e}"));
    }
    if let Err(e) = std::fs::rename(&stage_path, out_path) {
        let _ = std::fs::remove_file(&stage_path);
        return Err(format!("Couldn't finalize scan output: {e}"));
    }
    Ok(())
}

#[cfg(windows)]
mod mft_win {
    use super::*;
    use std::ffi::c_void;
    use windows_sys::Win32::Foundation::{
        CloseHandle, GetLastError, HANDLE, INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    };

    const GENERIC_READ: u32 = 0x8000_0000;
    const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x0000_0010;
    const FSCTL_QUERY_USN_JOURNAL: u32 = 0x0009_00f4;
    const FSCTL_ENUM_USN_DATA: u32 = 0x0009_00b3;
    const ERROR_INVALID_FUNCTION: u32 = 1;
    const ERROR_ACCESS_DENIED: u32 = 5;
    const ERROR_NO_MORE_FILES: u32 = 18;
    const ERROR_HANDLE_EOF: u32 = 38;
    const ERROR_NOT_SUPPORTED: u32 = 50;
    const ERROR_JOURNAL_DELETE_IN_PROGRESS: u32 = 1178;
    const ERROR_JOURNAL_NOT_ACTIVE: u32 = 1179;
    const NTFS_ROOT_FRN: u64 = 5;
    const FRN_MASK: u64 = 0x0000_FFFF_FFFF_FFFF;

    #[link(name = "kernel32")]
    extern "system" {
        fn DeviceIoControl(
            device: HANDLE,
            control_code: u32,
            in_buf: *const c_void,
            in_size: u32,
            out_buf: *mut c_void,
            out_size: u32,
            bytes_returned: *mut u32,
            overlapped: *mut c_void,
        ) -> i32;
    }

    #[repr(C)]
    #[derive(Default)]
    #[allow(dead_code)]
    struct UsnJournalDataV0 {
        usn_journal_id: u64,
        first_usn: i64,
        next_usn: i64,
        lowest_valid_usn: i64,
        max_usn: i64,
        maximum_size: u64,
        allocation_delta: u64,
    }

    #[repr(C)]
    #[allow(dead_code)]
    struct MftEnumDataV0 {
        start_file_reference_number: u64,
        low_usn: i64,
        high_usn: i64,
    }

    struct HandleGuard(HANDLE);

    impl Drop for HandleGuard {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }

    pub fn enumerate_and_write(drive: char, out_path: &Path) -> Result<(), String> {
        let vol_str = format!(r"\\.\{drive}:");
        let wide: Vec<u16> = vol_str.encode_utf16().chain(std::iter::once(0)).collect();
        let handle = unsafe {
            CreateFileW(
                wide.as_ptr(),
                GENERIC_READ,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null(),
                OPEN_EXISTING,
                0,
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

        // Step 1: Query USN journal to obtain high_usn (or fall back to i64::MAX if inactive).
        let mut journal = UsnJournalDataV0::default();
        let mut returned = 0u32;
        let q_ok = unsafe {
            DeviceIoControl(
                handle,
                FSCTL_QUERY_USN_JOURNAL,
                std::ptr::null(),
                0,
                (&mut journal as *mut UsnJournalDataV0).cast::<c_void>(),
                std::mem::size_of::<UsnJournalDataV0>() as u32,
                &mut returned,
                std::ptr::null_mut(),
            )
        };
        let high_usn = if q_ok != 0 {
            journal.next_usn
        } else {
            let err = unsafe { GetLastError() };
            match err {
                ERROR_JOURNAL_NOT_ACTIVE | ERROR_JOURNAL_DELETE_IN_PROGRESS => i64::MAX,
                ERROR_INVALID_FUNCTION | ERROR_NOT_SUPPORTED => {
                    return Err(format!("Drive {drive}: is not an NTFS volume."));
                }
                _ => {
                    return Err(format!(
                        "Couldn't query NTFS journal on {drive}: (Windows error {err})."
                    ));
                }
            }
        };

        // Step 2: Enumerate the MFT via FSCTL_ENUM_USN_DATA.
        let mut med = MftEnumDataV0 {
            start_file_reference_number: 0,
            low_usn: 0,
            high_usn,
        };
        let mut aligned_buf = vec![0u64; 8192]; // 64 KiB, 8-byte aligned
        let mut dirs: HashMap<u64, (u64, String)> = HashMap::with_capacity(32_768);
        let mut records: Vec<(u64, String, bool)> = Vec::with_capacity(131_072);

        loop {
            let mut bytes_ret = 0u32;
            let ok = unsafe {
                DeviceIoControl(
                    handle,
                    FSCTL_ENUM_USN_DATA,
                    (&med as *const MftEnumDataV0).cast::<c_void>(),
                    std::mem::size_of::<MftEnumDataV0>() as u32,
                    aligned_buf.as_mut_ptr().cast::<c_void>(),
                    (aligned_buf.len() * std::mem::size_of::<u64>()) as u32,
                    &mut bytes_ret,
                    std::ptr::null_mut(),
                )
            };
            if ok == 0 {
                let err = unsafe { GetLastError() };
                if err == ERROR_HANDLE_EOF || err == ERROR_NO_MORE_FILES {
                    break;
                }
                if err == ERROR_INVALID_FUNCTION || err == ERROR_NOT_SUPPORTED {
                    return Err(format!("Drive {drive}: is not an NTFS volume."));
                }
                return Err(format!(
                    "NTFS MFT enumeration failed on {drive}: (Windows error {err})."
                ));
            }

            let total = bytes_ret as usize;
            if total <= 8 {
                break;
            }
            let buf: &[u8] = unsafe {
                std::slice::from_raw_parts(aligned_buf.as_ptr().cast::<u8>(), total)
            };
            let next_frn = u64::from_le_bytes(buf[0..8].try_into().unwrap());

            let mut offset = 8usize;
            while offset + 60 <= total {
                let rec = &buf[offset..total];
                let rec_len = u32::from_le_bytes(rec[0..4].try_into().unwrap()) as usize;
                if rec_len < 60 || offset + rec_len > total {
                    break;
                }
                let major = u16::from_le_bytes(rec[4..6].try_into().unwrap());
                if major == 2 {
                    let frn = u64::from_le_bytes(rec[8..16].try_into().unwrap()) & FRN_MASK;
                    let parent_frn =
                        u64::from_le_bytes(rec[16..24].try_into().unwrap()) & FRN_MASK;
                    let attrs = u32::from_le_bytes(rec[52..56].try_into().unwrap());
                    let name_len = u16::from_le_bytes(rec[56..58].try_into().unwrap()) as usize;
                    let name_off = u16::from_le_bytes(rec[58..60].try_into().unwrap()) as usize;
                    if name_off >= 60
                        && name_off + name_len <= rec_len
                        && name_len > 0
                        && name_len % 2 == 0
                    {
                        let u16s: Vec<u16> = rec[name_off..name_off + name_len]
                            .chunks_exact(2)
                            .map(|c| u16::from_le_bytes([c[0], c[1]]))
                            .collect();
                        let name = String::from_utf16_lossy(&u16s);
                        let is_dir = (attrs & FILE_ATTRIBUTE_DIRECTORY) != 0;
                        let skip_root_meta = parent_frn == NTFS_ROOT_FRN
                            && (name.starts_with('$') || should_skip_dir_name(&name));
                        let bad_chars = name.contains(['\t', '\r', '\n']);
                        if !skip_root_meta && !bad_chars && !name.is_empty() {
                            if is_dir && frn != NTFS_ROOT_FRN {
                                dirs.insert(frn, (parent_frn, name.clone()));
                            }
                            records.push((parent_frn, name, is_dir));
                        }
                    }
                }
                offset += rec_len;
            }

            if next_frn == 0 || next_frn == med.start_file_reference_number {
                break;
            }
            med.start_file_reference_number = next_frn;
        }

        write_mft_tsv(drive, &dirs, &records, out_path)
    }
}

/// Entry point called at the very top of `main.rs` before Tauri starts.
/// Returns `true` if `--mft-scan` was requested (so `main()` exits immediately).
pub fn run_mft_cli_if_requested() -> bool {
    let Some((drive_raw, out_raw)) = parse_mft_cli_args(std::env::args()) else {
        return false;
    };

    // Refuse arbitrary file overwrites: only touch files inside std::env::temp_dir()
    // matching `swl-mft-*.swl-mft`. If invalid, exit silently without writing anything.
    let Some(out_path) = validate_mft_out_path(&out_raw) else {
        return true;
    };
    let done_path = done_sentinel_path(&out_path);

    let Some(drive) = validate_mft_cli_drive(&drive_raw) else {
        let _ = std::fs::write(&done_path, "ERR:Invalid drive letter.");
        return true;
    };

    #[cfg(windows)]
    {
        match mft_win::enumerate_and_write(drive, &out_path) {
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

fn start_mft_index(app: &AppHandle, raw_drive: &str) -> Value {
    let Some(drive) = parse_drive_letter(raw_drive) else {
        return json!({ "error": "Pick a valid drive letter (A–Z)." });
    };

    if !cfg!(windows) {
        return json!({ "error": "NTFS MFT scan is only available on Windows." });
    }

    let mut rand_bytes = [0u8; 16];
    if getrandom::getrandom(&mut rand_bytes).is_err() {
        return json!({ "error": "Couldn't create temporary scan file." });
    }
    let token = hex::encode(rand_bytes);
    let out_path = std::env::temp_dir().join(format!("swl-mft-{token}.swl-mft"));
    let done_path = done_sentinel_path(&out_path);
    let stage_path = temp_stage_path(&out_path);

    let Ok(exe) = std::env::current_exe() else {
        return json!({ "error": "Couldn't locate Swiftlane.exe." });
    };
    let dir = exe
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(std::env::temp_dir);
    let params = format!("--mft-scan {drive} \"{}\"", out_path.display());

    if !winapi::run_elevated(&exe, &params, &dir) {
        return json!({
            "error": "Administrator permission was cancelled. You can still use the normal folder scan without admin."
        });
    }

    let id = app.state::<AppState>().jobs.start(
        app,
        "findfiles",
        &format!("NTFS MFT scan ({drive}:\\)"),
        false,
        move |ctx| {
            let _cleanup = TempCleanup {
                paths: vec![out_path.clone(), done_path.clone(), stage_path],
            };
            let started = Instant::now();
            let deadline = started + Duration::from_secs(MFT_TIMEOUT_SECS);
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
                        Some(&format!("Reading NTFS MFT on {drive}:\\ ({secs}s)…")),
                    );
                    last_report = Instant::now();
                }
                std::thread::sleep(Duration::from_millis(100));
            }

            let status = std::fs::read_to_string(&done_path)
                .map_err(|_| JobError::Failed("Couldn't read MFT scan status.".into()))?;
            let status = status.trim();
            if let Some(err) = status.strip_prefix("ERR:") {
                return Err(JobError::Failed(err.trim().to_string()));
            }
            if status != "OK" {
                return Err(JobError::Failed("NTFS MFT scan didn't finish cleanly.".into()));
            }

            let file = std::fs::File::open(&out_path)
                .map_err(|_| JobError::Failed("Couldn't open MFT scan output.".into()))?;
            let reader = BufReader::with_capacity(256 * 1024, file);
            let mut entries: Vec<Entry> = Vec::with_capacity(65_536);

            for (i, line) in reader.lines().enumerate() {
                if i & 0x1FFF == 0 {
                    ctx.check()?;
                }
                let Ok(line) = line else { continue };
                if let Some(path) = line.strip_prefix("F\t") {
                    entries.push(make_entry(path.to_string(), false, None, None));
                } else if let Some(path) = line.strip_prefix("D\t") {
                    entries.push(make_entry(path.to_string(), true, None, None));
                }
                if entries.len() % 25_000 == 0 && !entries.is_empty() {
                    ctx.report(
                        None,
                        Some(&format!("Loading {} items…", grouped(entries.len() as u64))),
                    );
                }
            }
            // Drop temp files immediately before updating the index.
            drop(_cleanup);

            ctx.check()?;
            let elapsed_ms = started.elapsed().as_millis() as u64;
            let count = entries.len();
            let root_str = format!("{drive}:\\");
            {
                let mut idx = INDEX.lock().unwrap();
                idx.entries = entries;
                idx.mode = "mft".into();
                idx.roots = vec![root_str.clone()];
                idx.indexed_at = unix_now();
                idx.elapsed_ms = elapsed_ms;
            }
            config::log(&format!(
                "Find files: indexed {count} items on {root_str} ({elapsed_ms} ms, NTFS MFT)"
            ));
            push(ctx.app(), "findfiles", state_summary());
            Ok(Vec::new())
        },
    );
    json!(id)
}

// ------------------------------------------------------------------ Query engine

struct ParsedQuery {
    files_only: bool,
    dirs_only: bool,
    exts: Vec<String>,
    /// (lower_token, alt_slash_token_if_path, is_path)
    tokens: Vec<(String, Option<String>, bool)>,
    first_name_token: Option<String>,
}

fn parse_query(query: &str) -> ParsedQuery {
    let mut files_only = false;
    let mut dirs_only = false;
    let mut exts: Vec<String> = Vec::new();
    let mut tokens: Vec<(String, Option<String>, bool)> = Vec::new();
    let mut first_name_token: Option<String> = None;

    for raw in query.split_whitespace() {
        let lower = raw.to_lowercase();
        if lower == "file:" {
            files_only = true;
            continue;
        }
        if lower == "folder:" || lower == "dir:" {
            dirs_only = true;
            continue;
        }
        if let Some(rest) = lower.strip_prefix("ext:") {
            for ext in rest.split([';', ',']) {
                let clean = ext.trim().trim_start_matches('.');
                if !clean.is_empty() {
                    exts.push(clean.to_string());
                }
            }
            continue;
        }
        let is_path = lower.contains('\\') || lower.contains('/');
        let alt = if is_path {
            let swapped = if lower.contains('/') {
                lower.replace('/', "\\")
            } else {
                lower.replace('\\', "/")
            };
            (swapped != lower).then_some(swapped)
        } else {
            if first_name_token.is_none() {
                first_name_token = Some(lower.clone());
            }
            None
        };
        tokens.push((lower, alt, is_path));
    }

    ParsedQuery {
        files_only,
        dirs_only,
        exts,
        tokens,
        first_name_token,
    }
}

fn score_entry(entry: &Entry, first_name_token: Option<&str>) -> u8 {
    let Some(tok) = first_name_token else {
        return 3;
    };
    let name = entry.name_lower.as_str();
    if name == tok {
        return 0;
    }
    if let Some((stem, _)) = name.rsplit_once('.') {
        if stem == tok {
            return 0;
        }
    }
    if name.starts_with(tok) {
        return 1;
    }
    if let Some(pos) = name.find(tok) {
        if pos > 0 {
            let prev = name.as_bytes()[pos - 1];
            if matches!(prev, b' ' | b'_' | b'-' | b'.' | b'(' | b'[') {
                return 2;
            }
        }
    }
    3
}

struct HitCandidate {
    path: String,
    is_dir: bool,
    ext: String,
    size: Option<u64>,
    modified: Option<i64>,
    score: u8,
    name_len: usize,
    name_lower: String,
}

fn query_entries(idx: &FileIndex, query: &str, limit: usize) -> Value {
    let started = Instant::now();
    let limit = limit.clamp(1, MAX_LIMIT);
    let q = parse_query(query);

    if q.files_only && q.dirs_only {
        return json!({
            "total_matches": 0,
            "elapsed_ms": started.elapsed().as_millis() as u64,
            "indexed_count": idx.entries.len(),
            "mode": idx.mode,
            "items": [],
        });
    }

    let mut buckets: [Vec<usize>; 4] = [
        Vec::with_capacity(limit.min(64)),
        Vec::with_capacity(limit.min(128)),
        Vec::with_capacity(limit.min(128)),
        Vec::with_capacity(limit.min(150)),
    ];
    let mut total_matches: usize = 0;

    'entries: for (i, e) in idx.entries.iter().enumerate() {
        if q.files_only && e.is_dir {
            continue;
        }
        if q.dirs_only && !e.is_dir {
            continue;
        }
        if !q.exts.is_empty() {
            if e.is_dir || !q.exts.iter().any(|x| x == &e.ext_lower) {
                continue;
            }
        }
        for (tok, alt, is_path) in &q.tokens {
            let ok = if *is_path {
                e.path_lower.contains(tok.as_str())
                    || alt.as_deref().is_some_and(|a| e.path_lower.contains(a))
            } else {
                e.name_lower.contains(tok.as_str())
            };
            if !ok {
                continue 'entries;
            }
        }

        total_matches += 1;
        let sc = score_entry(e, q.first_name_token.as_deref()) as usize;
        // Skip collecting into lower-priority buckets once higher buckets already fill `limit`.
        let higher: usize = buckets[..sc].iter().map(Vec::len).sum();
        if higher < limit && buckets[sc].len() < limit {
            buckets[sc].push(i);
        }
    }

    let mut picked: Vec<HitCandidate> = Vec::with_capacity(limit);
    for (sc, b) in buckets.into_iter().enumerate() {
        for idx_i in b {
            if picked.len() >= limit {
                break;
            }
            let e = &idx.entries[idx_i];
            picked.push(HitCandidate {
                path: e.path.clone(),
                is_dir: e.is_dir,
                ext: e.ext_lower.clone(),
                size: e.size,
                modified: e.modified,
                score: sc as u8,
                name_len: e.name_lower.len(),
                name_lower: e.name_lower.clone(),
            });
        }
        if picked.len() >= limit {
            break;
        }
    }

    picked.sort_by(|a, b| {
        a.score
            .cmp(&b.score)
            .then_with(|| a.name_len.cmp(&b.name_len))
            .then_with(|| a.name_lower.cmp(&b.name_lower))
    });

    // Lazily stat ONLY the top returned slice for size & modified timestamp.
    let items: Vec<Value> = picked
        .into_iter()
        .map(|mut h| {
            if (!h.is_dir && h.size.is_none()) || h.modified.is_none() {
                if let Ok(meta) = std::fs::metadata(&h.path) {
                    if !h.is_dir && h.size.is_none() {
                        h.size = Some(meta.len());
                    }
                    if h.modified.is_none() {
                        h.modified = time_to_unix(meta.modified().ok());
                    }
                }
            }
            let (name, dir) = split_name_dir(&h.path);
            json!({
                "path": h.path,
                "name": name,
                "dir": dir,
                "is_dir": h.is_dir,
                "ext": h.ext,
                "size": h.size,
                "modified": h.modified,
            })
        })
        .collect();

    json!({
        "total_matches": total_matches,
        "elapsed_ms": started.elapsed().as_millis() as u64,
        "indexed_count": idx.entries.len(),
        "mode": idx.mode,
        "items": items,
    })
}

fn query_index(query: &str, limit: Option<usize>) -> Value {
    let idx = INDEX.lock().unwrap();
    query_entries(&idx, query, limit.unwrap_or(DEFAULT_LIMIT))
}

// ------------------------------------------------------------------ dispatch

pub fn dispatch(app: &AppHandle, method: &str, args: &[Value]) -> Option<Value> {
    let s = arg(args, 0).as_str().unwrap_or("");
    Some(match method {
        "find_state" => state_summary(),
        "find_pick" => {
            let home = dirs::home_dir().unwrap_or_default();
            ask_folder(app, &home)
                .map(|p| json!(p.to_string_lossy()))
                .unwrap_or(Value::Null)
        }
        "find_index" => start_folder_index(app, s),
        "find_index_mft" => start_mft_index(app, s),
        "find_query" => {
            let limit = arg(args, 1).as_u64().map(|n| n as usize);
            query_index(s, limit)
        }
        "find_open" => json!(in_index(s) && winapi::shell_open(Path::new(s))),
        "find_reveal" => json!(in_index(s) && winapi::reveal(Path::new(s))),
        _ => return None,
    })
}

// ------------------------------------------------------------------ tests

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh_dir() -> PathBuf {
        let mut b = [0u8; 6];
        getrandom::getrandom(&mut b).unwrap();
        let dir = std::env::temp_dir().join(format!("swl-find-test-{}", hex::encode(b)));
        std::fs::create_dir(&dir).unwrap();
        dir
    }

    #[test]
    fn parallel_folder_index_and_queries() {
        let dir = fresh_dir();
        std::fs::create_dir_all(dir.join("docs/archive")).unwrap();
        std::fs::create_dir_all(dir.join("media")).unwrap();
        std::fs::create_dir_all(dir.join("$Recycle.Bin")).unwrap();
        std::fs::create_dir_all(dir.join("System Volume Information")).unwrap();

        std::fs::write(dir.join("docs/report.pdf"), vec![0u8; 42]).unwrap();
        std::fs::write(dir.join("docs/report_2026.pdf"), vec![0u8; 100]).unwrap();
        std::fs::write(dir.join("docs/archive/my_report.txt"), b"hello").unwrap();
        std::fs::write(dir.join("media/holiday_clip.mkv"), vec![1u8; 256]).unwrap();
        std::fs::write(dir.join("media/trailer.mp4"), vec![2u8; 128]).unwrap();
        std::fs::write(dir.join("$Recycle.Bin/deleted.pdf"), b"skip").unwrap();
        std::fs::write(dir.join("System Volume Information/sys.dat"), b"skip").unwrap();

        let entries = scan_folder_parallel(&dir, |_| {}, || false).ok().unwrap();
        // Skipped folders and their contents must never appear.
        assert!(!entries.iter().any(|e| e.path_lower.contains("$recycle.bin")));
        assert!(!entries.iter().any(|e| e.path_lower.contains("system volume information")));

        let idx = FileIndex {
            entries,
            mode: "folder".into(),
            roots: vec![dir.to_string_lossy().into_owned()],
            indexed_at: unix_now(),
            elapsed_ms: 1,
        };

        // Prefix/exact scoring + lazy metadata fill
        let r = query_entries(&idx, "report", 50);
        assert_eq!(r["total_matches"], 3);
        let items = r["items"].as_array().unwrap();
        assert_eq!(items[0]["name"], "report.pdf");
        assert_eq!(items[0]["size"], 42);
        assert!(items[0]["modified"].as_i64().is_some());
        assert_eq!(items[1]["name"], "report_2026.pdf");
        assert_eq!(items[2]["name"], "my_report.txt");

        // Multi-token + ext filter
        let r_pdf = query_entries(&idx, "report ext:pdf", 50);
        assert_eq!(r_pdf["total_matches"], 2);
        assert!(r_pdf["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|i| i["ext"] == "pdf"));

        // Multi-extension ext:mp4;mkv;mov
        let r_vid = query_entries(&idx, "ext:mp4;mkv;mov", 50);
        assert_eq!(r_vid["total_matches"], 2);

        // file: vs folder:/dir:
        let r_dirs = query_entries(&idx, "folder:", 50);
        assert!(r_dirs["total_matches"].as_u64().unwrap() >= 3);
        assert!(r_dirs["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|i| i["is_dir"] == true));

        let r_dir_alias = query_entries(&idx, "dir: archive", 50);
        assert_eq!(r_dir_alias["total_matches"], 1);
        assert_eq!(r_dir_alias["items"][0]["name"], "archive");

        let r_files = query_entries(&idx, "file: docs", 50);
        assert_eq!(r_files["total_matches"], 0);

        // Path token with slash
        let r_path = query_entries(&idx, "archive/my_report", 50);
        assert_eq!(r_path["total_matches"], 1);
        assert_eq!(r_path["items"][0]["name"], "my_report.txt");

        // Cancellation stops cleanly
        assert!(scan_folder_parallel(&dir, |_| {}, || true).is_err());

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn validates_mft_cli_drive_and_temp_path_strictly() {
        // Drive letter checks
        assert_eq!(validate_mft_cli_drive("C"), Some('C'));
        assert_eq!(validate_mft_cli_drive("d"), Some('D'));
        assert_eq!(validate_mft_cli_drive("C:"), None);
        assert_eq!(validate_mft_cli_drive("C:\\"), None);
        assert_eq!(validate_mft_cli_drive(""), None);
        assert_eq!(validate_mft_cli_drive("1"), None);
        assert_eq!(validate_mft_cli_drive("CD"), None);

        // UI drive parser accepts C, C:, C:\
        assert_eq!(parse_drive_letter("c"), Some('C'));
        assert_eq!(parse_drive_letter("D:\\"), Some('D'));
        assert_eq!(parse_drive_letter("E:/"), Some('E'));
        assert_eq!(parse_drive_letter("\\\\server\\share"), None);

        // Temp path security checks
        let valid = std::env::temp_dir().join("swl-mft-0123456789abcdef.swl-mft");
        assert!(validate_mft_out_path(&valid.to_string_lossy()).is_some());

        // Must reject wrong extension, wrong prefix, traversal, nested dirs, or outside temp_dir
        let wrong_ext = std::env::temp_dir().join("swl-mft-1234.txt");
        assert!(validate_mft_out_path(&wrong_ext.to_string_lossy()).is_none());

        let wrong_prefix = std::env::temp_dir().join("other-1234.swl-mft");
        assert!(validate_mft_out_path(&wrong_prefix.to_string_lossy()).is_none());

        let traversal = format!(
            "{}\\..\\swl-mft-1234.swl-mft",
            std::env::temp_dir().display()
        );
        assert!(validate_mft_out_path(&traversal).is_none());

        let nested = std::env::temp_dir()
            .join("subdir")
            .join("swl-mft-1234.swl-mft");
        assert!(validate_mft_out_path(&nested.to_string_lossy()).is_none());

        assert!(validate_mft_out_path("C:\\Windows\\swl-mft-1234.swl-mft").is_none());
        assert!(validate_mft_out_path("").is_none());
    }

    #[test]
    fn reconstructs_mft_paths_with_cycle_guard() {
        let mut dirs = HashMap::new();
        dirs.insert(10, (5, "Users".to_string()));
        dirs.insert(20, (10, "amato".to_string()));
        // Cycle: 30 -> 31 -> 30
        dirs.insert(30, (31, "loopA".to_string()));
        dirs.insert(31, (30, "loopB".to_string()));
        // Orphan: parent 999 does not exist
        dirs.insert(40, (999, "orphan".to_string()));

        let mut cache = HashMap::new();
        cache.insert(5, Some("C:\\".to_string()));

        assert_eq!(
            resolve_mft_dir(20, &dirs, &mut cache).as_deref(),
            Some("C:\\Users\\amato")
        );
        assert_eq!(resolve_mft_dir(30, &dirs, &mut cache), None);
        assert_eq!(resolve_mft_dir(40, &dirs, &mut cache), None);

        let mut b = [0u8; 6];
        getrandom::getrandom(&mut b).unwrap();
        let out = std::env::temp_dir().join(format!("swl-mft-test-{}.swl-mft", hex::encode(b)));
        let records = vec![
            (5u64, "Users".to_string(), true),
            (20u64, "notes.txt".to_string(), false),
            (30u64, "bad.txt".to_string(), false),
        ];
        write_mft_tsv('C', &dirs, &records, &out).unwrap();
        let text = std::fs::read_to_string(&out).unwrap();
        std::fs::remove_file(&out).unwrap();
        assert_eq!(text, "D\tC:\\Users\nF\tC:\\Users\\amato\\notes.txt\n");
    }
}
