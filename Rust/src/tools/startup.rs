//! Startup & Background Check: read-only inspection and reversible toggling of everything
//! that runs automatically when Windows starts.
//!
//! Sources scanned:
//! - Registry Run keys (HKCU Run, HKLM Run, HKLM WOW6432Node Run) + Explorer\StartupApproved
//! - User & Common Startup folders + Explorer\StartupApproved\StartupFolder
//! - Scheduled Tasks via `schtasks /Query /FO CSV /V`
//!
//! Security rules:
//! - Strictly read-only inspection: never executes any startup target or `.lnk`.
//! - Reversible toggles only: never deletes registry values, `.lnk` files, or scheduled tasks.
//! - Strictly validates `id` against the cached scan (`ITEMS`) before toggling or revealing.
//! - Never interpolates untrusted strings into a shell command.

use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
#[cfg(any(windows, test))]
use std::time::{SystemTime, UNIX_EPOCH};
use tauri::AppHandle;

use super::{arg, command};
use crate::{config, winapi};

#[cfg(windows)]
const RUN_SUBKEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
#[cfg(windows)]
const RUN32_SUBKEY: &str = r"Software\WOW6432Node\Microsoft\Windows\CurrentVersion\Run";
#[cfg(windows)]
const APPROVED_RUN: &str = r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run";
#[cfg(windows)]
const APPROVED_RUN32: &str = r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run32";
#[cfg(windows)]
const APPROVED_FOLDER: &str = r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\StartupFolder";

#[derive(Clone, Debug)]
struct StartupItem {
    id: String,
    name: String,
    source: String,
    scope: String,
    system: bool,
    builtin: bool,
    enabled: bool,
    command: String,
    target_path: String,
    reveal_path: String,
    can_reveal: bool,
    publisher: String,
    description: String,
    missing: bool,
    reg_value_name: String,
    trigger: String,
}

impl StartupItem {
    fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "name": self.name,
            "source": self.source,
            "scope": self.scope,
            "system": self.system,
            "builtin": self.builtin,
            "enabled": self.enabled,
            "command": self.command,
            "target_path": self.target_path,
            "can_reveal": self.can_reveal,
            "publisher": self.publisher,
            "description": self.description,
            "missing": self.missing,
            "trigger": self.trigger,
        })
    }
}

static ITEMS: Mutex<Vec<StartupItem>> = Mutex::new(Vec::new());

// ------------------------------------------------------------------ StartupApproved binary codec

/// Decodes Windows' `Explorer\StartupApproved` REG_BINARY payload.
/// First byte `0x02` or `0x06` (or missing/empty/short payload) => enabled;
/// odd first byte (`0x03`, `0x07`, etc.) => disabled.
#[cfg(any(windows, test))]
fn decode_startup_approved(bytes: &[u8]) -> bool {
    if bytes.len() < 12 {
        return true;
    }
    let first = bytes[0];
    if first == 0x02 || first == 0x06 {
        true
    } else {
        (first & 1) == 0
    }
}

/// Encodes a 12-byte `Explorer\StartupApproved` REG_BINARY payload matching Task Manager.
#[cfg(any(windows, test))]
fn encode_startup_approved(enabled: bool, filetime: u64) -> [u8; 12] {
    let mut out = [0u8; 12];
    if enabled {
        out[0] = 0x02;
    } else {
        out[0] = 0x03;
        out[4..12].copy_from_slice(&filetime.to_le_bytes());
    }
    out
}

/// Current UTC time as a Windows FILETIME (100-ns intervals since 1601-01-01).
#[cfg(any(windows, test))]
fn current_windows_filetime() -> u64 {
    const EPOCH_DIFF_100NS: u64 = 116_444_736_000_000_000;
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(d) => {
            let ticks = d
                .as_secs()
                .saturating_mul(10_000_000)
                .saturating_add((d.subsec_nanos() / 100) as u64);
            EPOCH_DIFF_100NS.saturating_add(ticks)
        }
        Err(_) => EPOCH_DIFF_100NS,
    }
}

// ------------------------------------------------------------------ command & path resolution

fn lookup_env(name: &str) -> Option<String> {
    if let Ok(v) = std::env::var(name) {
        return Some(v);
    }
    // Case-insensitive fallback for non-Windows test environments.
    for (k, v) in std::env::vars() {
        if k.eq_ignore_ascii_case(name) {
            return Some(v);
        }
    }
    None
}

/// Expands `%VAR%` environment variables (e.g. `%LOCALAPPDATA%`, `%ProgramFiles(x86)%`, `%SystemRoot%`).
fn expand_env_vars(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = String::with_capacity(input.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if let Some(rel_end) = input[i + 1..].find('%') {
                let var_name = &input[i + 1..i + 1 + rel_end];
                let valid_name = !var_name.is_empty()
                    && var_name
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'(' | b')' | b'-' | b' '));
                if valid_name {
                    if let Some(val) = lookup_env(var_name) {
                        out.push_str(&val);
                        i += rel_end + 2;
                        continue;
                    }
                }
            }
        }
        let ch = input[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

const EXEC_EXTS: &[&str] = &[
    ".exe", ".cmd", ".bat", ".vbs", ".msc", ".com", ".ps1", ".msi", ".scr", ".lnk",
];

/// Extracts the executable path from a Windows startup command line without executing anything.
fn extract_executable(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty()
        || trimmed.eq_ignore_ascii_case("n/a")
        || trimmed.to_lowercase().starts_with("com handler")
    {
        return String::new();
    }

    // Quoted path: `"C:\Program Files\App\app.exe" --arg`
    if let Some(rest) = trimmed.strip_prefix('"') {
        if let Some(end_quote) = rest.find('"') {
            let inner = rest[..end_quote].trim();
            return expand_env_vars(inner);
        }
        return expand_env_vars(rest.trim());
    }

    // Unquoted path: expand env vars first so `%ProgramFiles%\App\a.exe --arg` resolves properly.
    let expanded = expand_env_vars(trimmed);
    let lower = expanded.to_ascii_lowercase();

    // Find the earliest known executable extension bounded by EOF, whitespace, or comma.
    let mut best_end: Option<usize> = None;
    for ext in EXEC_EXTS {
        let mut search_from = 0;
        while let Some(pos) = lower[search_from..].find(ext) {
            let end = search_from + pos + ext.len();
            let boundary = match expanded.as_bytes().get(end) {
                None => true,
                Some(&b) => b == b' ' || b == b'\t' || b == b',' || b == b'"',
            };
            if boundary {
                if best_end.map_or(true, |cur| end < cur) {
                    best_end = Some(end);
                }
                break;
            }
            search_from = end;
        }
    }
    if let Some(end) = best_end {
        return expanded[..end].trim().to_string();
    }

    // Fallback: first whitespace-delimited token.
    expanded
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_string()
}

/// Resolves a bare executable name (e.g. `rundll32.exe`) against System32 / PATH if needed.
fn resolve_target_path(cmd: &str) -> String {
    let extracted = extract_executable(cmd);
    if extracted.is_empty() {
        return String::new();
    }
    if extracted.contains('\\') || extracted.contains('/') || extracted.contains(':') {
        return extracted;
    }
    if let Some(sysroot) = std::env::var_os("SystemRoot").or_else(|| std::env::var_os("WINDIR")) {
        let base = PathBuf::from(sysroot);
        for candidate in [base.join("System32").join(&extracted), base.join(&extracted)] {
            if candidate.is_file() {
                return candidate.to_string_lossy().to_string();
            }
        }
    }
    if let Some(path_var) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path_var) {
            let candidate = dir.join(&extracted);
            if candidate.is_file() {
                return candidate.to_string_lossy().to_string();
            }
        }
    }
    extracted
}

// ------------------------------------------------------------------ Win32 version info (publisher)

#[cfg(windows)]
mod version_info {
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;

    #[link(name = "version")]
    extern "system" {
        fn GetFileVersionInfoSizeW(lptstrfilename: *const u16, lpdwhandle: *mut u32) -> u32;
        fn GetFileVersionInfoW(
            lptstrfilename: *const u16,
            dwhandle: u32,
            dwlen: u32,
            lpdata: *mut c_void,
        ) -> i32;
        fn VerQueryValueW(
            pblock: *const c_void,
            lpsubblock: *const u16,
            lplpbuffer: *mut *mut c_void,
            pulen: *mut u32,
        ) -> i32;
    }

    fn wide_str(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn query_string(buf: &[u8], subblock: &str) -> Option<String> {
        let sub = wide_str(subblock);
        let mut ptr: *mut c_void = std::ptr::null_mut();
        let mut len: u32 = 0;
        let ok = unsafe {
            VerQueryValueW(
                buf.as_ptr() as *const c_void,
                sub.as_ptr(),
                &mut ptr,
                &mut len,
            )
        };
        if ok == 0 || ptr.is_null() || len == 0 {
            return None;
        }
        let byte_count = (len as usize).saturating_mul(2);
        let raw_bytes = unsafe { std::slice::from_raw_parts(ptr as *const u8, byte_count) };
        let mut u16s = Vec::with_capacity(len as usize);
        for chunk in raw_bytes.chunks_exact(2) {
            let code = u16::from_le_bytes([chunk[0], chunk[1]]);
            if code == 0 {
                break;
            }
            u16s.push(code);
        }
        let text = String::from_utf16_lossy(&u16s).trim().to_string();
        if text.is_empty() {
            None
        } else {
            Some(text)
        }
    }

    /// Reads `(CompanyName, FileDescription)` from an executable's Win32 version info.
    pub fn read(path: &Path) -> (String, String) {
        if !path.is_file() {
            return (String::new(), String::new());
        }
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if !matches!(ext.as_str(), "exe" | "dll" | "scr" | "ocx" | "cpl" | "sys" | "com") {
            return (String::new(), String::new());
        }

        let wide_path: Vec<u16> = path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let mut handle: u32 = 0;
        let size = unsafe { GetFileVersionInfoSizeW(wide_path.as_ptr(), &mut handle) };
        if size == 0 || size > 4 * 1024 * 1024 {
            return (String::new(), String::new());
        }
        let mut buf = vec![0u8; size as usize];
        let ok = unsafe {
            GetFileVersionInfoW(
                wide_path.as_ptr(),
                0,
                size,
                buf.as_mut_ptr() as *mut c_void,
            )
        };
        if ok == 0 {
            return (String::new(), String::new());
        }

        let mut lang_cp_list: Vec<(u16, u16)> = Vec::new();
        let trans_key = wide_str(r"\VarFileInfo\Translation");
        let mut ptr: *mut c_void = std::ptr::null_mut();
        let mut byte_len: u32 = 0;
        let has_trans = unsafe {
            VerQueryValueW(
                buf.as_ptr() as *const c_void,
                trans_key.as_ptr(),
                &mut ptr,
                &mut byte_len,
            )
        };
        if has_trans != 0 && !ptr.is_null() && byte_len >= 4 {
            let count = (byte_len as usize) / 4;
            let bytes = unsafe { std::slice::from_raw_parts(ptr as *const u8, count * 4) };
            for chunk in bytes.chunks_exact(4) {
                let lang = u16::from_le_bytes([chunk[0], chunk[1]]);
                let cp = u16::from_le_bytes([chunk[2], chunk[3]]);
                lang_cp_list.push((lang, cp));
            }
        }
        for fallback in [(0x0409, 0x04B0), (0x0000, 0x04B0), (0x0409, 0x04E4)] {
            if !lang_cp_list.contains(&fallback) {
                lang_cp_list.push(fallback);
            }
        }

        let mut company = String::new();
        let mut desc = String::new();
        for (lang, cp) in lang_cp_list {
            if company.is_empty() {
                let key = format!(r"\StringFileInfo\{lang:04X}{cp:04X}\CompanyName");
                if let Some(v) = query_string(&buf, &key) {
                    company = v;
                }
            }
            if desc.is_empty() {
                let key = format!(r"\StringFileInfo\{lang:04X}{cp:04X}\FileDescription");
                if let Some(v) = query_string(&buf, &key) {
                    desc = v;
                }
            }
            if !company.is_empty() && !desc.is_empty() {
                break;
            }
        }
        (company, desc)
    }
}

#[cfg(not(windows))]
mod version_info {
    use std::path::Path;
    pub fn read(_path: &Path) -> (String, String) {
        (String::new(), String::new())
    }
}

fn cached_version_info(
    target_path: &str,
    cache: &mut HashMap<String, (String, String)>,
) -> (String, String) {
    if target_path.is_empty() {
        return (String::new(), String::new());
    }
    let key = target_path.to_lowercase();
    if let Some(hit) = cache.get(&key) {
        return hit.clone();
    }
    let info = version_info::read(Path::new(target_path));
    cache.insert(key, info.clone());
    info
}

// ------------------------------------------------------------------ Registry Run keys

#[cfg(windows)]
fn read_approved_map(root: winreg::HKEY, subkey: &str) -> HashMap<String, bool> {
    use winreg::enums::KEY_READ;
    use winreg::RegKey;

    let mut map = HashMap::new();
    let Ok(key) = RegKey::predef(root).open_subkey_with_flags(subkey, KEY_READ) else {
        return map;
    };
    for (name, val) in key.enum_values().flatten() {
        if !name.is_empty() {
            map.insert(name.to_lowercase(), decode_startup_approved(&val.bytes));
        }
    }
    map
}

#[cfg(windows)]
fn scan_run_key(
    root: winreg::HKEY,
    run_subkey: &str,
    approved_subkey: &str,
    source: &str,
    scope: &str,
    system: bool,
    ver_cache: &mut HashMap<String, (String, String)>,
    out: &mut Vec<StartupItem>,
) {
    use winreg::enums::KEY_READ;
    use winreg::RegKey;

    let Ok(key) = RegKey::predef(root).open_subkey_with_flags(run_subkey, KEY_READ) else {
        return;
    };
    let approved = read_approved_map(root, approved_subkey);

    for (name, raw) in key.enum_values().flatten() {
        if name.is_empty() {
            continue;
        }
        let command = match key.get_value::<String, _>(&name) {
            Ok(s) => s,
            Err(_) => String::from_utf8_lossy(&raw.bytes)
                .trim_end_matches('\0')
                .to_string(),
        };
        let enabled = approved.get(&name.to_lowercase()).copied().unwrap_or(true);
        let target_path = resolve_target_path(&command);
        let exists = !target_path.is_empty() && Path::new(&target_path).exists();
        let missing = !target_path.is_empty() && !exists;
        let (publisher, description) = if exists {
            cached_version_info(&target_path, ver_cache)
        } else {
            (String::new(), String::new())
        };
        let reveal_path = if exists {
            target_path.clone()
        } else {
            String::new()
        };

        out.push(StartupItem {
            id: format!("{source}:{name}"),
            name: name.clone(),
            source: source.to_string(),
            scope: scope.to_string(),
            system,
            builtin: false,
            enabled,
            command,
            target_path,
            reveal_path,
            can_reveal: exists,
            publisher,
            description,
            missing,
            reg_value_name: name,
            trigger: String::new(),
        });
    }
}

#[cfg(windows)]
fn scan_all_run_keys(
    ver_cache: &mut HashMap<String, (String, String)>,
    out: &mut Vec<StartupItem>,
) {
    use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};

    scan_run_key(
        HKEY_CURRENT_USER,
        RUN_SUBKEY,
        APPROVED_RUN,
        "user_run",
        "user",
        false,
        ver_cache,
        out,
    );
    scan_run_key(
        HKEY_LOCAL_MACHINE,
        RUN_SUBKEY,
        APPROVED_RUN,
        "machine_run",
        "system",
        true,
        ver_cache,
        out,
    );
    scan_run_key(
        HKEY_LOCAL_MACHINE,
        RUN32_SUBKEY,
        APPROVED_RUN32,
        "machine_run32",
        "system",
        true,
        ver_cache,
        out,
    );
}

#[cfg(not(windows))]
fn scan_all_run_keys(
    _ver_cache: &mut HashMap<String, (String, String)>,
    _out: &mut Vec<StartupItem>,
) {
}

// ------------------------------------------------------------------ Startup folders

fn scan_folder_dir(
    dir: &Path,
    approved: &HashMap<String, bool>,
    source: &str,
    scope: &str,
    system: bool,
    ver_cache: &mut HashMap<String, (String, String)>,
    out: &mut Vec<StartupItem>,
) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(file_name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if file_name.eq_ignore_ascii_case("desktop.ini") {
            continue;
        }
        let display_name = if file_name.len() > 4 && file_name[file_name.len() - 4..].eq_ignore_ascii_case(".lnk") {
            file_name[..file_name.len() - 4].to_string()
        } else {
            file_name.to_string()
        };
        let enabled = approved
            .get(&file_name.to_lowercase())
            .copied()
            .unwrap_or(true);
        let full_path = path.to_string_lossy().to_string();
        let exists = path.exists();
        let (publisher, description) = if exists {
            cached_version_info(&full_path, ver_cache)
        } else {
            (String::new(), String::new())
        };

        out.push(StartupItem {
            id: format!("{source}:{file_name}"),
            name: display_name,
            source: source.to_string(),
            scope: scope.to_string(),
            system,
            builtin: false,
            enabled,
            command: full_path.clone(),
            target_path: full_path.clone(),
            reveal_path: full_path,
            can_reveal: exists,
            publisher,
            description,
            missing: !exists,
            reg_value_name: file_name.to_string(),
            trigger: String::new(),
        });
    }
}

fn scan_all_folders(
    ver_cache: &mut HashMap<String, (String, String)>,
    out: &mut Vec<StartupItem>,
) {
    let user_dir = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .or_else(dirs::config_dir)
        .map(|p| p.join(r"Microsoft\Windows\Start Menu\Programs\Startup"));

    if let Some(dir) = user_dir {
        #[cfg(windows)]
        let approved = read_approved_map(winreg::enums::HKEY_CURRENT_USER, APPROVED_FOLDER);
        #[cfg(not(windows))]
        let approved = HashMap::new();
        scan_folder_dir(
            &dir,
            &approved,
            "user_folder",
            "user",
            false,
            ver_cache,
            out,
        );
    }

    let common_dir = std::env::var_os("ProgramData")
        .or_else(|| std::env::var_os("PROGRAMDATA"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"))
        .join(r"Microsoft\Windows\Start Menu\Programs\Startup");

    #[cfg(windows)]
    let approved = read_approved_map(winreg::enums::HKEY_LOCAL_MACHINE, APPROVED_FOLDER);
    #[cfg(not(windows))]
    let approved = HashMap::new();
    scan_folder_dir(
        &common_dir,
        &approved,
        "machine_folder",
        "system",
        true,
        ver_cache,
        out,
    );
}

// ------------------------------------------------------------------ Scheduled Tasks CSV parser

/// RFC-4180 CSV parser supporting quoted fields, embedded commas, escaped `""`, and CRLF/LF.
fn parse_csv(input: &str) -> Vec<Vec<String>> {
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut row: Vec<String> = Vec::new();
    let mut field = String::new();
    let mut in_quotes = false;
    let mut chars = input.chars().peekable();

    while let Some(c) = chars.next() {
        if in_quotes {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    chars.next();
                    field.push('"');
                } else {
                    in_quotes = false;
                }
            } else {
                field.push(c);
            }
        } else {
            match c {
                '"' if field.is_empty() => in_quotes = true,
                ',' => row.push(std::mem::take(&mut field)),
                '\r' => {
                    if chars.peek() == Some(&'\n') {
                        chars.next();
                    }
                    row.push(std::mem::take(&mut field));
                    if !row.iter().all(|s| s.is_empty()) {
                        rows.push(std::mem::take(&mut row));
                    } else {
                        row.clear();
                    }
                }
                '\n' => {
                    row.push(std::mem::take(&mut field));
                    if !row.iter().all(|s| s.is_empty()) {
                        rows.push(std::mem::take(&mut row));
                    } else {
                        row.clear();
                    }
                }
                _ => field.push(c),
            }
        }
    }
    if !field.is_empty() || !row.is_empty() {
        row.push(field);
        if !row.iter().all(|s| s.is_empty()) {
            rows.push(row);
        }
    }
    rows
}

fn is_startup_trigger(schedule_type: &str) -> bool {
    let s = schedule_type.trim().to_lowercase();
    if s.is_empty() {
        return false;
    }
    const KEYWORDS: &[&str] = &[
        "at logon",
        "at startup",
        "on logon",
        "logon",
        "boot",
        "startup",
        "bei anmeldung",
        "beim start",
        "przy logowaniu",
        "przy uruchamianiu",
        "all'accesso",
        "all'avvio",
    ];
    KEYWORDS.iter().any(|k| s.contains(k))
}

fn is_builtin_task(task_name: &str) -> bool {
    let trimmed = task_name.trim().trim_start_matches('\\').to_lowercase();
    trimmed.starts_with(r"microsoft\windows\")
        || trimmed.starts_with(r"microsoft\office\")
        || trimmed.starts_with(r"microsoft\edge")
}

fn is_disabled_status(status: &str, state: &str) -> bool {
    for raw in [status, state] {
        let s = raw.trim().to_lowercase();
        if matches!(
            s.as_str(),
            "disabled" | "deaktiviert" | "wyłączono" | "wylaczono" | "disabilitato"
        ) {
            return true;
        }
    }
    false
}

struct ParsedTask {
    task_name: String,
    display_name: String,
    command: String,
    author: String,
    trigger: String,
    enabled: bool,
    builtin: bool,
    startup_trigger: bool,
}

fn parse_schtasks_csv(csv_text: &str) -> Vec<ParsedTask> {
    let rows = parse_csv(csv_text);
    let Some(header) = rows.first() else {
        return Vec::new();
    };

    let find_col = |candidates: &[&str], fallback: Option<usize>| -> Option<usize> {
        for (idx, col) in header.iter().enumerate() {
            let norm = col.trim().trim_start_matches('\u{feff}').to_lowercase();
            if candidates.iter().any(|c| norm == *c) {
                return Some(idx);
            }
        }
        fallback.filter(|&i| i < header.len())
    };

    let wide_table = header.len() >= 19;
    let col_task = find_col(
        &["taskname", "task name", "aufgabenname", "nazwa zadania", "nome attività"],
        if wide_table { Some(1) } else { None },
    );
    let col_status = find_col(
        &["status", "stan", "stato"],
        if wide_table { Some(3) } else { None },
    );
    let col_author = find_col(
        &["author", "autor", "autore"],
        if wide_table { Some(7) } else { None },
    );
    let col_run = find_col(
        &[
            "task to run",
            "auszuführende aufgabe",
            "zadanie do uruchomienia",
            "attività da eseguire",
        ],
        if wide_table { Some(8) } else { None },
    );
    let col_state = find_col(
        &[
            "scheduled task state",
            "status der geplanten aufgabe",
            "stan zaplanowanego zadania",
            "stato attività pianificata",
        ],
        if wide_table { Some(11) } else { None },
    );
    let col_sched = find_col(
        &[
            "schedule type",
            "trigger",
            "zeitplantyp",
            "typ harmonogramu",
            "tipo di pianificazione",
        ],
        if wide_table { Some(18) } else { None },
    );

    let Some(i_task) = col_task else {
        return Vec::new();
    };
    let header_task_label = header[i_task].trim().to_string();

    let mut order: Vec<String> = Vec::new();
    let mut by_key: HashMap<String, ParsedTask> = HashMap::new();
    let mut seen_order: HashSet<String> = HashSet::new();

    for row in rows.iter().skip(1) {
        let get = |opt: Option<usize>| -> &str {
            opt.and_then(|i| row.get(i)).map(String::as_str).unwrap_or("").trim()
        };
        let task_name = get(Some(i_task));
        if task_name.is_empty()
            || task_name == header_task_label
            || task_name.eq_ignore_ascii_case("taskname")
        {
            continue;
        }
        let status = get(col_status);
        let state = get(col_state);
        let author = get(col_author);
        let command = get(col_run);
        let sched = get(col_sched);

        let startup_trig = is_startup_trigger(sched);
        let builtin = is_builtin_task(task_name);
        let enabled = !is_disabled_status(status, state);
        let key = task_name.to_lowercase();

        if let Some(existing) = by_key.get_mut(&key) {
            if startup_trig && !existing.startup_trigger {
                existing.startup_trigger = true;
                existing.trigger = sched.to_string();
            }
            if existing.command.is_empty() && !command.is_empty() {
                existing.command = command.to_string();
            }
            if !enabled {
                existing.enabled = false;
            }
            continue;
        }

        let display_name = task_name
            .rsplit('\\')
            .find(|s| !s.is_empty())
            .unwrap_or(task_name)
            .to_string();

        if seen_order.insert(key.clone()) {
            order.push(key.clone());
        }
        by_key.insert(
            key,
            ParsedTask {
                task_name: task_name.to_string(),
                display_name,
                command: command.to_string(),
                author: if author.eq_ignore_ascii_case("n/a") {
                    String::new()
                } else {
                    author.to_string()
                },
                trigger: if startup_trig {
                    sched.to_string()
                } else {
                    String::new()
                },
                enabled,
                builtin,
                startup_trigger: startup_trig,
            },
        );
    }

    let mut result = Vec::new();
    for key in order {
        let Some(task) = by_key.remove(&key) else {
            continue;
        };
        let has_exec = !extract_executable(&task.command).is_empty();
        // Include if startup/logon trigger OR non-Microsoft task with an executable action.
        let is_microsoft = task.builtin
            || task
                .task_name
                .trim_start_matches('\\')
                .to_lowercase()
                .starts_with(r"microsoft\");
        if task.startup_trigger || (!is_microsoft && has_exec) {
            result.push(task);
        }
    }
    result
}

fn scan_scheduled_tasks(
    ver_cache: &mut HashMap<String, (String, String)>,
    out: &mut Vec<StartupItem>,
) {
    let Ok(output) = command(Path::new("schtasks"))
        .args(["/Query", "/FO", "CSV", "/V"])
        .output()
    else {
        return;
    };
    if !output.status.success() {
        return;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    for task in parse_schtasks_csv(&text) {
        let target_path = resolve_target_path(&task.command);
        let exists = !target_path.is_empty() && Path::new(&target_path).exists();
        let missing = !target_path.is_empty() && !exists;
        let (mut publisher, description) = if exists {
            cached_version_info(&target_path, ver_cache)
        } else {
            (String::new(), String::new())
        };
        if publisher.is_empty() && !task.author.is_empty() && !task.author.starts_with("$(@") {
            publisher = task.author.clone();
        }
        let reveal_path = if exists {
            target_path.clone()
        } else {
            String::new()
        };

        out.push(StartupItem {
            id: format!("task:{}", task.task_name),
            name: task.display_name,
            source: "task".to_string(),
            scope: "system".to_string(),
            system: true,
            builtin: task.builtin,
            enabled: task.enabled,
            command: task.command,
            target_path,
            reveal_path,
            can_reveal: exists,
            publisher,
            description,
            missing,
            reg_value_name: task.task_name,
            trigger: task.trigger,
        });
    }
}

// ------------------------------------------------------------------ Scan, Toggle, Reveal

fn build_summary(items: &[StartupItem]) -> Value {
    let total = items.len();
    let enabled = items.iter().filter(|i| i.enabled).count();
    let disabled = total.saturating_sub(enabled);
    let user = items.iter().filter(|i| i.scope == "user").count();
    let system = items.iter().filter(|i| i.scope == "system").count();
    json!({
        "total": total,
        "enabled": enabled,
        "disabled": disabled,
        "user": user,
        "system": system,
    })
}

fn scan() -> Value {
    let mut ver_cache: HashMap<String, (String, String)> = HashMap::new();
    let mut items: Vec<StartupItem> = Vec::new();

    scan_all_run_keys(&mut ver_cache, &mut items);
    scan_all_folders(&mut ver_cache, &mut items);
    scan_scheduled_tasks(&mut ver_cache, &mut items);

    items.sort_by(|a, b| {
        a.builtin
            .cmp(&b.builtin)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            .then_with(|| a.source.cmp(&b.source))
    });

    let items_json: Vec<Value> = items.iter().map(StartupItem::to_json).collect();
    let summary = build_summary(&items);

    if let Ok(mut guard) = ITEMS.lock() {
        *guard = items;
    }

    json!({
        "items": items_json,
        "summary": summary,
    })
}

#[cfg(windows)]
fn write_startup_approved(
    root: winreg::HKEY,
    subkey: &str,
    value_name: &str,
    enabled: bool,
) -> std::io::Result<()> {
    use winreg::enums::{RegType, KEY_SET_VALUE};
    use winreg::{RegKey, RegValue};

    let predef = RegKey::predef(root);
    let key = match predef.open_subkey_with_flags(subkey, KEY_SET_VALUE) {
        Ok(k) => k,
        Err(_) => predef.create_subkey(subkey)?.0,
    };
    let bytes = encode_startup_approved(enabled, current_windows_filetime());
    let reg_val = RegValue {
        bytes: bytes.to_vec(),
        vtype: RegType::REG_BINARY,
    };
    key.set_raw_value(value_name, &reg_val)
}

fn toggle_backend(item: &StartupItem, enabled: bool) -> Result<(), String> {
    match item.source.as_str() {
        "user_run" | "user_folder" => {
            #[cfg(windows)]
            {
                let subkey = if item.source == "user_run" {
                    APPROVED_RUN
                } else {
                    APPROVED_FOLDER
                };
                write_startup_approved(
                    winreg::enums::HKEY_CURRENT_USER,
                    subkey,
                    &item.reg_value_name,
                    enabled,
                )
                .map_err(|e| format!("Could not update startup entry: {e}"))
            }
            #[cfg(not(windows))]
            {
                let _ = (item, enabled);
                Err("Startup management is only supported on Windows.".to_string())
            }
        }
        "machine_run" | "machine_run32" | "machine_folder" => {
            #[cfg(windows)]
            {
                let subkey = match item.source.as_str() {
                    "machine_run" => APPROVED_RUN,
                    "machine_run32" => APPROVED_RUN32,
                    _ => APPROVED_FOLDER,
                };
                write_startup_approved(
                    winreg::enums::HKEY_LOCAL_MACHINE,
                    subkey,
                    &item.reg_value_name,
                    enabled,
                )
                .map_err(|_| {
                    "This is a system-wide startup entry and requires administrator permission to change."
                        .to_string()
                })
            }
            #[cfg(not(windows))]
            {
                let _ = (item, enabled);
                Err("This is a system-wide startup entry and requires administrator permission to change.".to_string())
            }
        }
        "task" => {
            let flag = if enabled { "/ENABLE" } else { "/DISABLE" };
            let output = command(Path::new("schtasks"))
                .args(["/Change", "/TN", &item.reg_value_name, flag])
                .output()
                .map_err(|_| {
                    "Could not change this scheduled task (administrator permission may be required)."
                        .to_string()
                })?;
            if !output.status.success() {
                return Err(
                    "Could not change this scheduled task (administrator permission may be required)."
                        .to_string(),
                );
            }
            Ok(())
        }
        _ => Err("Unsupported startup entry type.".to_string()),
    }
}

fn toggle(id: &str, enabled: bool) -> Value {
    let item = {
        let guard = ITEMS.lock().unwrap();
        guard.iter().find(|i| i.id == id).cloned()
    };
    let Some(item) = item else {
        return json!({ "error": "Item not found. Refresh the list first." });
    };

    if let Err(err) = toggle_backend(&item, enabled) {
        return json!({ "error": err });
    }

    let mut guard = ITEMS.lock().unwrap();
    let updated_json = if let Some(slot) = guard.iter_mut().find(|i| i.id == id) {
        slot.enabled = enabled;
        slot.to_json()
    } else {
        let mut copy = item.clone();
        copy.enabled = enabled;
        copy.to_json()
    };
    let summary = build_summary(&guard);
    drop(guard);

    config::log(&format!(
        "Startup {}: {} ({})",
        if enabled { "enabled" } else { "disabled" },
        item.name,
        item.source
    ));

    json!({
        "ok": true,
        "item": updated_json,
        "summary": summary,
    })
}

fn reveal(id: &str) -> Value {
    let reveal_path = {
        let guard = ITEMS.lock().unwrap();
        guard
            .iter()
            .find(|i| i.id == id)
            .map(|i| i.reveal_path.clone())
    };
    let Some(path_str) = reveal_path else {
        return json!({ "error": "Item not found. Refresh the list first." });
    };
    if path_str.is_empty() || !Path::new(&path_str).exists() {
        return json!({ "error": "Target file does not exist on disk." });
    }
    if winapi::reveal(Path::new(&path_str)) {
        json!({ "ok": true })
    } else {
        json!({ "error": "Could not open File Explorer for this item." })
    }
}

pub fn dispatch(_app: &AppHandle, method: &str, args: &[Value]) -> Option<Value> {
    match method {
        "startup_scan" => Some(scan()),
        "startup_toggle" => {
            let id = arg(args, 0).as_str().unwrap_or("");
            let enabled = arg(args, 1).as_bool().unwrap_or(true);
            Some(toggle(id, enabled))
        }
        "startup_reveal" => {
            let id = arg(args, 0).as_str().unwrap_or("");
            Some(reveal(id))
        }
        _ => None,
    }
}

// ------------------------------------------------------------------ Unit tests

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_executable_handles_quotes_args_and_env() {
        assert_eq!(
            extract_executable(r#""C:\Program Files\Discord\Update.exe" --processStart Discord.exe"#),
            r"C:\Program Files\Discord\Update.exe"
        );
        assert_eq!(
            extract_executable(r"C:\Program Files\My App\helper.exe --tray --silent"),
            r"C:\Program Files\My App\helper.exe"
        );
        assert_eq!(
            extract_executable(r"C:\Tools\startup.cmd /quiet"),
            r"C:\Tools\startup.cmd"
        );
        assert_eq!(
            extract_executable(r"C:\Scripts\check.vbs //Nologo"),
            r"C:\Scripts\check.vbs"
        );
        assert_eq!(extract_executable("COM handler"), "");
        assert_eq!(extract_executable("N/A"), "");
        assert_eq!(extract_executable("   "), "");

        if let Some(sysroot) = lookup_env("SystemRoot") {
            assert_eq!(
                extract_executable(r"%SystemRoot%\System32\SecurityHealthSystray.exe /minimized"),
                format!(r"{sysroot}\System32\SecurityHealthSystray.exe")
            );
            assert_eq!(
                extract_executable(r#""%SystemRoot%\System32\cmd.exe" /c echo hi"#),
                format!(r"{sysroot}\System32\cmd.exe")
            );
        }
    }

    #[test]
    fn startup_approved_decode_and_encode_roundtrip() {
        assert!(decode_startup_approved(&[]));
        assert!(decode_startup_approved(&[0x03]));
        assert!(decode_startup_approved(&[0x03, 0, 0, 0]));

        assert!(decode_startup_approved(&[0x02, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]));
        assert!(decode_startup_approved(&[0x06, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]));
        assert!(!decode_startup_approved(&[0x03, 0, 0, 0, 1, 2, 3, 4, 5, 6, 7, 8]));
        assert!(!decode_startup_approved(&[0x07, 0, 0, 0, 1, 2, 3, 4, 5, 6, 7, 8]));

        let enc_on = encode_startup_approved(true, 123_456_789);
        assert_eq!(enc_on, [0x02, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        assert!(decode_startup_approved(&enc_on));

        let ft: u64 = 133_500_000_000_000_000;
        let enc_off = encode_startup_approved(false, ft);
        assert_eq!(enc_off[0], 0x03);
        assert_eq!(&enc_off[1..4], &[0, 0, 0]);
        assert_eq!(&enc_off[4..12], &ft.to_le_bytes());
        assert!(!decode_startup_approved(&enc_off));
        assert!(current_windows_filetime() > 116_444_736_000_000_000);
    }

    #[test]
    fn schtasks_csv_parser_quotes_triggers_builtins_and_dedup() {
        let sample = concat!(
            "\"TaskName\",\"Status\",\"Author\",\"Task To Run\",\"Scheduled Task State\",\"Schedule Type\"\r\n",
            "\"\\Acme\\Updater\",\"Ready\",\"Acme, Inc.\",\"\"\"C:\\Program Files\\Acme, Inc\\updater.exe\"\" --silent\",\"Enabled\",\"At logon\"\r\n",
            "\"\\Acme\\Updater\",\"Ready\",\"Acme, Inc.\",\"\"\"C:\\Program Files\\Acme, Inc\\updater.exe\"\" --silent\",\"Enabled\",\"Daily\"\r\n",
            "\"\\Microsoft\\Windows\\Wininet\\CacheTask\",\"Ready\",\"Microsoft\",\"COM handler\",\"Enabled\",\"At logon\"\r\n",
            "\"\\Microsoft\\Windows\\Maintenance\\Weekly\",\"Ready\",\"Microsoft\",\"C:\\Windows\\System32\\maint.exe\",\"Enabled\",\"Weekly\"\r\n",
            "\"\\Microsoft\\Office\\ClickToRun\",\"Disabled\",\"Microsoft\",\"C:\\Office\\c2r.exe\",\"Disabled\",\"At startup\"\r\n",
            "\"\\Microsoft\\EdgeUpdateTaskMachineCore\",\"Ready\",\"Microsoft\",\"C:\\Edge\\update.exe\",\"Enabled\",\"Boot\"\r\n",
            "\"\\Custom\\NoStartupExe\",\"Disabled\",\"Me\",\"C:\\Tools\\sync.exe /bg\",\"Disabled\",\"Daily\"\r\n",
        );

        let tasks = parse_schtasks_csv(sample);
        assert_eq!(tasks.len(), 5);

        let acme = tasks.iter().find(|t| t.task_name == r"\Acme\Updater").unwrap();
        assert_eq!(acme.display_name, "Updater");
        assert_eq!(acme.author, "Acme, Inc.");
        assert_eq!(
            acme.command,
            r#""C:\Program Files\Acme, Inc\updater.exe" --silent"#
        );
        assert_eq!(
            extract_executable(&acme.command),
            r"C:\Program Files\Acme, Inc\updater.exe"
        );
        assert!(acme.enabled);
        assert!(!acme.builtin);
        assert!(acme.startup_trigger);

        let win_task = tasks
            .iter()
            .find(|t| t.task_name == r"\Microsoft\Windows\Wininet\CacheTask")
            .unwrap();
        assert!(win_task.builtin);
        assert!(win_task.startup_trigger);

        // Weekly Microsoft\Windows task with no startup trigger is excluded.
        assert!(tasks
            .iter()
            .all(|t| t.task_name != r"\Microsoft\Windows\Maintenance\Weekly"));

        let office = tasks
            .iter()
            .find(|t| t.task_name == r"\Microsoft\Office\ClickToRun")
            .unwrap();
        assert!(office.builtin);
        assert!(!office.enabled);

        let edge = tasks
            .iter()
            .find(|t| t.task_name == r"\Microsoft\EdgeUpdateTaskMachineCore")
            .unwrap();
        assert!(edge.builtin);

        let custom = tasks
            .iter()
            .find(|t| t.task_name == r"\Custom\NoStartupExe")
            .unwrap();
        assert!(!custom.builtin);
        assert!(!custom.enabled);
    }

    #[test]
    fn unknown_id_is_rejected_for_toggle_and_reveal() {
        let r_toggle = toggle("user_run:__definitely_not_in_cache__", false);
        assert!(r_toggle.get("error").and_then(Value::as_str).is_some());

        let r_reveal = reveal("user_run:__definitely_not_in_cache__");
        assert!(r_reveal.get("error").and_then(Value::as_str).is_some());
    }

    #[test]
    fn folder_scan_skips_desktop_ini_and_respects_approved_map() {
        let mut rand_bytes = [0u8; 6];
        getrandom::getrandom(&mut rand_bytes).unwrap();
        let dir = std::env::temp_dir().join(format!("swl-startup-test-{}", hex::encode(rand_bytes)));
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(dir.join("desktop.ini"), b"[.ShellClassInfo]").unwrap();
        std::fs::write(dir.join("MyApp.lnk"), b"fake-lnk").unwrap();
        std::fs::write(dir.join("Helper.cmd"), b"@echo off").unwrap();

        let mut approved = HashMap::new();
        approved.insert("myapp.lnk".to_string(), false);
        let mut cache = HashMap::new();
        let mut items = Vec::new();
        scan_folder_dir(
            &dir,
            &approved,
            "user_folder",
            "user",
            false,
            &mut cache,
            &mut items,
        );

        let _ = std::fs::remove_dir_all(&dir);

        assert_eq!(items.len(), 2);
        assert!(items.iter().all(|i| !i.reg_value_name.eq_ignore_ascii_case("desktop.ini")));
        let myapp = items.iter().find(|i| i.reg_value_name == "MyApp.lnk").unwrap();
        assert_eq!(myapp.name, "MyApp");
        assert!(!myapp.enabled);
        let helper = items.iter().find(|i| i.reg_value_name == "Helper.cmd").unwrap();
        assert_eq!(helper.name, "Helper.cmd");
        assert!(helper.enabled);
    }
}
