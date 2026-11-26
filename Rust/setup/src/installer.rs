//! Swiftlane Setup's transactional install engine, with rollback.
//!
//! Every step registers how to undo itself. If any step fails, everything already done is
//! undone in reverse order, so the PC ends up exactly as before.
//! Windows-specific actions (firewall, registry, network, shortcuts, processes) live in
//! `WindowsSystem` behind `SystemOps` so the engine can be tested with a fake implementation.

use chrono::Local;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::rc::Rc;
use std::thread;
use std::time::Duration;

#[cfg(windows)]
use std::os::windows::process::CommandExt;

pub const APP: &str = "Swiftlane";
pub const OLD_APP: &str = "PocketDrop";
pub const PORT: u16 = 8765;
pub const UNINSTALL_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall\Swiftlane";
pub const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
pub const TRAILER_MAGIC: &[u8; 8] = b"SWLSETUP";
pub const TRAILER_LEN: usize = 40;

const OUR_RULES: &str = "'Swiftlane','PocketDrop'";
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Path to Setup's log file (`%TEMP%\Swiftlane Setup.log` or `SWIFTLANE_SETUP_LOG`).
pub fn log_file() -> PathBuf {
    if let Ok(p) = std::env::var("SWIFTLANE_SETUP_LOG") {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    std::env::temp_dir().join("Swiftlane Setup.log")
}

/// Appends a timestamped line to Setup's log file (best-effort, never fails the install).
pub fn write_log(text: &str) {
    let path = log_file();
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(path) {
        let ts = Local::now().format("%Y-%m-%d %H:%M:%S");
        let _ = writeln!(f, "[{ts}] {text}");
    }
}

// ---------------------------------------------------------------- Payload & 40-byte trailer

/// Zero-copy `Read + Seek` view over `[start, start + len)` of an underlying reader.
pub struct SectionReader<R> {
    inner: R,
    start: u64,
    len: u64,
    pos: u64,
}

impl<R: Read + Seek> SectionReader<R> {
    pub fn new(mut inner: R, start: u64, len: u64) -> io::Result<Self> {
        inner.seek(SeekFrom::Start(start))?;
        Ok(Self {
            inner,
            start,
            len,
            pos: 0,
        })
    }
}

impl<R: Read + Seek> Read for SectionReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let remaining = self.len.saturating_sub(self.pos);
        if remaining == 0 {
            return Ok(0);
        }
        let max = (remaining as usize).min(buf.len());
        let n = self.inner.read(&mut buf[..max])?;
        self.pos += n as u64;
        Ok(n)
    }
}

impl<R: Read + Seek> Seek for SectionReader<R> {
    fn seek(&mut self, style: SeekFrom) -> io::Result<u64> {
        let target: i128 = match style {
            SeekFrom::Start(n) => n as i128,
            SeekFrom::End(off) => self.len as i128 + off as i128,
            SeekFrom::Current(off) => self.pos as i128 + off as i128,
        };
        if target < 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid seek to a negative position",
            ));
        }
        let target = target as u64;
        self.inner.seek(SeekFrom::Start(self.start + target))?;
        self.pos = target;
        Ok(self.pos)
    }
}

/// Source of the app's `payload.zip`: either appended to `Swiftlane Setup.exe` with the 40-byte
/// `SWLSETUP` trailer, or an external file (via `SWIFTLANE_PAYLOAD` or sibling `payload.zip`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PayloadSource {
    File {
        path: PathBuf,
        version: String,
    },
    Embedded {
        exe_path: PathBuf,
        offset: u64,
        len: u64,
        version: String,
    },
}

impl PayloadSource {
    /// Parses the 40-byte trailer at EOF of `path`:
    /// `[magic: b"SWLSETUP" (8), payload_offset: u64 LE (8), payload_len: u64 LE (8), version: [u8; 16] (16)]`.
    pub fn parse_trailer(path: &Path) -> io::Result<Option<Self>> {
        let mut f = File::open(path)?;
        let file_len = f.metadata()?.len();
        if file_len < TRAILER_LEN as u64 {
            return Ok(None);
        }
        f.seek(SeekFrom::End(-(TRAILER_LEN as i64)))?;
        let mut buf = [0u8; TRAILER_LEN];
        f.read_exact(&mut buf)?;
        if &buf[0..8] != TRAILER_MAGIC {
            return Ok(None);
        }
        let offset = u64::from_le_bytes(buf[8..16].try_into().unwrap());
        let len = u64::from_le_bytes(buf[16..24].try_into().unwrap());
        let trailer_start = file_len - TRAILER_LEN as u64;
        if len == 0 || offset.checked_add(len).map_or(true, |end| end > trailer_start) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Invalid embedded payload trailer offsets",
            ));
        }
        let ver_bytes = &buf[24..40];
        let end = ver_bytes.iter().position(|&b| b == 0).unwrap_or(ver_bytes.len());
        let version = std::str::from_utf8(&ver_bytes[..end])
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "Invalid UTF-8 in trailer version"))?
            .trim()
            .to_string();
        let version = if version.is_empty() {
            env!("CARGO_PKG_VERSION").to_string()
        } else {
            version
        };
        Ok(Some(Self::Embedded {
            exe_path: path.to_path_buf(),
            offset,
            len,
            version,
        }))
    }

    /// Resolves the payload for this Setup process:
    /// 1. `SWIFTLANE_PAYLOAD` environment variable (external zip file)
    /// 2. 40-byte `SWLSETUP` trailer at EOF of the current executable
    /// 3. `payload.zip` next to the current executable
    pub fn resolve() -> Result<Self, String> {
        if let Ok(p) = std::env::var("SWIFTLANE_PAYLOAD") {
            if !p.is_empty() {
                return Ok(Self::File {
                    path: PathBuf::from(p),
                    version: env!("CARGO_PKG_VERSION").to_string(),
                });
            }
        }
        if let Ok(exe) = std::env::current_exe() {
            if let Ok(Some(embedded)) = Self::parse_trailer(&exe) {
                return Ok(embedded);
            }
            if let Some(dir) = exe.parent() {
                let sibling = dir.join("payload.zip");
                if sibling.is_file() {
                    return Ok(Self::File {
                        path: sibling,
                        version: env!("CARGO_PKG_VERSION").to_string(),
                    });
                }
            }
        }
        Err("Installer payload not found.".to_string())
    }

    pub fn version(&self) -> &str {
        match self {
            Self::File { version, .. } | Self::Embedded { version, .. } => version,
        }
    }

    pub fn open_zip(&self) -> Result<zip::ZipArchive<SectionReader<File>>, String> {
        let (path, offset, len) = match self {
            Self::File { path, .. } => {
                let meta = fs::metadata(path)
                    .map_err(|e| format!("Couldn't open installer payload ({e})."))?;
                (path.as_path(), 0u64, meta.len())
            }
            Self::Embedded {
                exe_path,
                offset,
                len,
                ..
            } => (exe_path.as_path(), *offset, *len),
        };
        let file = File::open(path)
            .map_err(|e| format!("Couldn't open installer payload ({e})."))?;
        let section = SectionReader::new(file, offset, len)
            .map_err(|e| format!("Couldn't seek installer payload ({e})."))?;
        zip::ZipArchive::new(section)
            .map_err(|e| format!("The installer payload is corrupted ({e})."))
    }

    /// Total uncompressed size of all entries in the payload zip (0 on error).
    pub fn uncompressed_size(&self) -> u64 {
        let Ok(mut archive) = self.open_zip() else {
            return 0;
        };
        let mut total = 0u64;
        for i in 0..archive.len() {
            if let Ok(entry) = archive.by_index_raw(i) {
                total = total.saturating_add(entry.size());
            }
        }
        total
    }
}

// ---------------------------------------------------------------- Windows helpers & FFI

fn powershell_exe() -> PathBuf {
    let root = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
    PathBuf::from(root)
        .join("System32")
        .join("WindowsPowerShell")
        .join("v1.0")
        .join("powershell.exe")
}

fn taskkill_exe() -> PathBuf {
    let root = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
    PathBuf::from(root).join("System32").join("taskkill.exe")
}

/// Run PowerShell hidden; values arrive only through environment variables, never interpolated.
fn run_ps(script: &str, envs: &[(&str, &str)]) -> Result<String, String> {
    let mut cmd = Command::new(powershell_exe());
    cmd.args([
        "-NoProfile",
        "-NonInteractive",
        "-ExecutionPolicy",
        "Bypass",
        "-Command",
        script,
    ]);
    for (k, v) in envs {
        cmd.env(k, v);
    }
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);

    let out = cmd
        .output()
        .map_err(|e| format!("Couldn't start PowerShell: {e}"))?;
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    if !out.status.success() {
        let combined = if !stderr.trim().is_empty() {
            stderr.trim()
        } else {
            stdout.trim()
        };
        let msg = combined
            .lines()
            .last()
            .filter(|l| !l.trim().is_empty())
            .unwrap_or("PowerShell failed");
        return Err(msg.to_string());
    }
    Ok(stdout)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsingProcess {
    pub name: String,
    pub pid: u32,
}

/// Names of programs that currently hold any of `paths` open (Windows Restart Manager).
#[cfg(windows)]
pub fn programs_using(paths: &[PathBuf]) -> Vec<UsingProcess> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::ERROR_MORE_DATA;
    use windows_sys::Win32::System::RestartManager::{
        RmEndSession, RmGetList, RmRegisterResources, RmStartSession, RM_PROCESS_INFO,
    };

    let existing: Vec<Vec<u16>> = paths
        .iter()
        .filter(|p| p.exists())
        .map(|p| p.as_os_str().encode_wide().chain(std::iter::once(0)).collect())
        .collect();
    if existing.is_empty() {
        return Vec::new();
    }

    let mut session: u32 = 0;
    let mut session_key = [0u16; 64];
    let rc = unsafe { RmStartSession(&mut session, 0, session_key.as_mut_ptr()) };
    if rc != 0 {
        return Vec::new();
    }

    struct SessionGuard(u32);
    impl Drop for SessionGuard {
        fn drop(&mut self) {
            unsafe {
                RmEndSession(self.0);
            }
        }
    }
    let _guard = SessionGuard(session);

    let ptrs: Vec<*const u16> = existing.iter().map(|w| w.as_ptr()).collect();
    let rc = unsafe {
        RmRegisterResources(
            session,
            ptrs.len() as u32,
            ptrs.as_ptr(),
            0,
            std::ptr::null(),
            0,
            std::ptr::null(),
        )
    };
    if rc != 0 {
        return Vec::new();
    }

    let mut needed: u32 = 0;
    let mut count: u32 = 16;
    let mut reasons: u32 = 0;
    let mut infos: Vec<RM_PROCESS_INFO> = vec![unsafe { std::mem::zeroed() }; count as usize];
    let mut rc = unsafe {
        RmGetList(
            session,
            &mut needed,
            &mut count,
            infos.as_mut_ptr(),
            &mut reasons,
        )
    };
    if rc == ERROR_MORE_DATA && needed > 0 {
        count = needed;
        infos.resize(count as usize, unsafe { std::mem::zeroed() });
        rc = unsafe {
            RmGetList(
                session,
                &mut needed,
                &mut count,
                infos.as_mut_ptr(),
                &mut reasons,
            )
        };
    }
    if rc != 0 {
        return Vec::new();
    }

    let mut found: BTreeMap<u32, String> = BTreeMap::new();
    for info in infos.iter().take(count as usize) {
        let pid = info.Process.dwProcessId;
        let end = info
            .strAppName
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(info.strAppName.len());
        let name = String::from_utf16_lossy(&info.strAppName[..end]);
        found.insert(pid, name);
    }
    found
        .into_iter()
        .map(|(pid, name)| UsingProcess { name, pid })
        .collect()
}

#[cfg(not(windows))]
pub fn programs_using(_paths: &[PathBuf]) -> Vec<UsingProcess> {
    Vec::new()
}

/// `{pid: (parent_pid, exe_name)}` for every running process.
pub type ProcessTable = HashMap<u32, (u32, String)>;

#[cfg(windows)]
pub fn process_table() -> ProcessTable {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };

    let snap = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snap == INVALID_HANDLE_VALUE || snap.is_null() {
        return HashMap::new();
    }
    let mut table = HashMap::new();
    unsafe {
        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        if Process32FirstW(snap, &mut entry) != 0 {
            loop {
                let end = entry
                    .szExeFile
                    .iter()
                    .position(|&c| c == 0)
                    .unwrap_or(entry.szExeFile.len());
                let exe = String::from_utf16_lossy(&entry.szExeFile[..end]);
                table.insert(entry.th32ProcessID, (entry.th32ParentProcessID, exe));
                if Process32NextW(snap, &mut entry) == 0 {
                    break;
                }
            }
        }
        CloseHandle(snap);
    }
    table
}

#[cfg(not(windows))]
pub fn process_table() -> ProcessTable {
    HashMap::new()
}

fn descendants(children: &HashMap<u32, Vec<u32>>, root: u32) -> HashSet<u32> {
    let mut out = HashSet::new();
    let mut todo = vec![root];
    while let Some(p) = todo.pop() {
        if out.insert(p) {
            if let Some(kids) = children.get(&p) {
                todo.extend(kids.iter().copied());
            }
        }
    }
    out
}

/// PIDs to close: every process named in `names`, plus everything it started (WebView2 can hold
/// the folder), but never Setup itself or anything Setup started. When updating from inside
/// Swiftlane, Swiftlane above Setup in the ancestor chain is still stopped.
pub fn processes_to_stop(table: &ProcessTable, me: u32, names: &[&str]) -> Vec<u32> {
    let names_lc: HashSet<String> = names.iter().map(|n| n.to_lowercase()).collect();
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    for (&pid, &(parent, _)) in table {
        if pid != parent {
            children.entry(parent).or_default().push(pid);
        }
    }

    let my_tree = descendants(&children, me);
    let mut keep = HashSet::new();
    let mut p = me;
    let mut seen = HashSet::new();
    while let Some(&(parent, _)) = table.get(&p) {
        if !seen.insert(p) {
            break;
        }
        if p == me {
            keep.extend(&my_tree);
        } else {
            keep.insert(p);
        }
        p = parent;
    }

    let mut stop = HashSet::new();
    for (&pid, (_, exe)) in table {
        if names_lc.contains(&exe.to_lowercase()) {
            stop.extend(descendants(&children, pid));
        }
    }

    let mut out: Vec<u32> = stop
        .into_iter()
        .filter(|&pid| {
            !my_tree.contains(&pid)
                && pid != me
                && (!keep.contains(&pid)
                    || table
                        .get(&pid)
                        .is_some_and(|(_, exe)| names_lc.contains(&exe.to_lowercase())))
        })
        .collect();
    out.sort_unstable();
    out
}

/// Other copies of Setup still running (e.g. a leftover from a failed update that can keep
/// Swiftlane's folder busy). Never this Setup, the chain that started it, or what it started.
pub fn stale_setups(table: &ProcessTable, me: u32, name: &str) -> Vec<u32> {
    let mut mine = HashSet::new();
    let mut p = me;
    let mut seen = HashSet::new();
    while let Some(&(parent, _)) = table.get(&p) {
        if !seen.insert(p) {
            break;
        }
        mine.insert(p);
        p = parent;
    }
    let mut todo = vec![me];
    while let Some(q) = todo.pop() {
        for (&pid, &(parent, _)) in table {
            if parent == q && pid != parent && mine.insert(pid) {
                todo.push(pid);
            }
        }
    }
    let mut out: Vec<u32> = table
        .iter()
        .filter_map(|(&pid, (_, exe))| {
            (exe.eq_ignore_ascii_case(name) && !mine.contains(&pid)).then_some(pid)
        })
        .collect();
    out.sort_unstable();
    out
}

#[cfg(windows)]
pub fn known_folder(csidl: i32) -> PathBuf {
    use windows_sys::Win32::UI::Shell::SHGetFolderPathW;
    let mut buf = [0u16; 260];
    let hr = unsafe {
        SHGetFolderPathW(
            std::ptr::null_mut(),
            csidl,
            std::ptr::null_mut(),
            0,
            buf.as_mut_ptr(),
        )
    };
    if hr == 0 {
        let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        if end > 0 {
            return PathBuf::from(String::from_utf16_lossy(&buf[..end]));
        }
    }
    PathBuf::new()
}

#[cfg(not(windows))]
pub fn known_folder(_csidl: i32) -> PathBuf {
    PathBuf::new()
}

pub fn documents_dir() -> PathBuf {
    let p = known_folder(5); // CSIDL_PERSONAL
    if !p.as_os_str().is_empty() {
        return p;
    }
    dirs::document_dir().unwrap_or_else(|| PathBuf::from("."))
}

pub fn start_menu_dir() -> PathBuf {
    let p = known_folder(0x17); // CSIDL_COMMON_PROGRAMS (ProgramData\...\Start Menu\Programs)
    if !p.as_os_str().is_empty() {
        return p;
    }
    let pd = std::env::var_os("ProgramData").unwrap_or_else(|| "C:\\ProgramData".into());
    PathBuf::from(pd)
        .join("Microsoft")
        .join("Windows")
        .join("Start Menu")
        .join("Programs")
}

pub fn desktop_dir() -> PathBuf {
    let p = known_folder(0x10); // CSIDL_DESKTOPDIRECTORY
    if !p.as_os_str().is_empty() {
        return p;
    }
    dirs::desktop_dir().unwrap_or_else(|| PathBuf::from("."))
}

pub fn program_files_dir() -> PathBuf {
    std::env::var_os("ProgramFiles")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Program Files"))
}

// ---------------------------------------------------------------- SystemOps abstraction

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RegHive {
    Hklm,
    Hkcu,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegVal {
    Sz(String),
    Dword(u32),
    ExpandSz(String),
    Raw { kind: u32, bytes: Vec<u8> },
}

impl RegVal {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::Sz(s) | Self::ExpandSz(s) => Some(s.as_str()),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkProfile {
    pub alias: String,
    pub name: String,
    pub category: String,
}

pub trait SystemOps {
    fn stop_app(&self) -> Result<(), String>;
    fn firewall_rules(&self) -> Result<Vec<(String, String)>, String>;
    fn remove_firewall_rules(&self) -> Result<(), String>;
    fn add_firewall_rule(&self, name: &str, program: &str) -> Result<(), String>;
    fn networks(&self) -> Result<Vec<NetworkProfile>, String>;
    fn set_network_category(&self, alias: &str, category: &str) -> Result<(), String>;
    fn read_key(&self, hive: RegHive, path: &str) -> Option<BTreeMap<String, RegVal>>;
    fn write_key(&self, hive: RegHive, path: &str, values: &BTreeMap<String, RegVal>) -> Result<(), String>;
    fn delete_key(&self, hive: RegHive, path: &str) -> Result<(), String>;
    fn read_value(&self, hive: RegHive, path: &str, name: &str) -> Option<RegVal>;
    fn write_value(&self, hive: RegHive, path: &str, name: &str, value: Option<&RegVal>) -> Result<(), String>;
    fn make_shortcut(&self, lnk: &Path, target: &Path, description: &str) -> Result<(), String>;

    fn start_menu_dir(&self) -> PathBuf {
        start_menu_dir()
    }
    fn desktop_dir(&self) -> PathBuf {
        desktop_dir()
    }
    fn program_files_dir(&self) -> PathBuf {
        program_files_dir()
    }
}

pub struct WindowsSystem;

impl WindowsSystem {
    fn kill_pid(pid: u32) {
        let mut cmd = Command::new(taskkill_exe());
        cmd.args(["/F", "/PID", &pid.to_string()]);
        #[cfg(windows)]
        cmd.creation_flags(CREATE_NO_WINDOW);
        let _ = cmd.output();
    }
}

#[cfg(windows)]
mod win_reg_helpers {
    use super::{RegHive, RegVal};
    use winreg::enums::{
        HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, KEY_SET_VALUE, REG_DWORD, REG_EXPAND_SZ,
        REG_SZ,
    };
    use winreg::{RegKey, RegValue, HKEY};

    pub fn predef(hive: RegHive) -> HKEY {
        match hive {
            RegHive::Hklm => HKEY_LOCAL_MACHINE,
            RegHive::Hkcu => HKEY_CURRENT_USER,
        }
    }

    pub fn from_reg_value(v: RegValue) -> RegVal {
        match v.vtype {
            REG_SZ => RegVal::Sz(decode_wide_sz(&v.bytes)),
            REG_EXPAND_SZ => RegVal::ExpandSz(decode_wide_sz(&v.bytes)),
            REG_DWORD if v.bytes.len() >= 4 => {
                RegVal::Dword(u32::from_le_bytes(v.bytes[..4].try_into().unwrap()))
            }
            other => RegVal::Raw {
                kind: other as u32,
                bytes: v.bytes,
            },
        }
    }

    fn decode_wide_sz(bytes: &[u8]) -> String {
        let u16s: Vec<u16> = bytes
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        let end = u16s.iter().position(|&c| c == 0).unwrap_or(u16s.len());
        String::from_utf16_lossy(&u16s[..end])
    }

    pub fn set_reg_value(key: &RegKey, name: &str, val: &RegVal) -> Result<(), String> {
        let res = match val {
            RegVal::Sz(s) => key.set_value(name, s),
            RegVal::Dword(d) => key.set_value(name, d),
            RegVal::ExpandSz(s) => {
                let mut bytes = Vec::with_capacity((s.len() + 1) * 2);
                for u in s.encode_utf16().chain(std::iter::once(0)) {
                    bytes.extend_from_slice(&u.to_le_bytes());
                }
                key.set_raw_value(
                    name,
                    &RegValue {
                        bytes,
                        vtype: REG_EXPAND_SZ,
                    },
                )
            }
            RegVal::Raw { kind, bytes } => {
                let vtype = match *kind {
                    0 => winreg::enums::REG_NONE,
                    1 => REG_SZ,
                    2 => REG_EXPAND_SZ,
                    3 => winreg::enums::REG_BINARY,
                    4 => REG_DWORD,
                    5 => winreg::enums::REG_DWORD_BIG_ENDIAN,
                    6 => winreg::enums::REG_LINK,
                    7 => winreg::enums::REG_MULTI_SZ,
                    11 => winreg::enums::REG_QWORD,
                    _ => winreg::enums::REG_BINARY,
                };
                key.set_raw_value(
                    name,
                    &RegValue {
                        bytes: bytes.clone(),
                        vtype,
                    },
                )
            }
        };
        res.map_err(|e| format!("Registry write failed ({name}): {e}"))
    }

    pub fn open_read(hive: RegHive, path: &str) -> io::Result<RegKey> {
        RegKey::predef(predef(hive)).open_subkey_with_flags(path, KEY_READ)
    }

    pub fn create_write(hive: RegHive, path: &str) -> Result<RegKey, String> {
        RegKey::predef(predef(hive))
            .create_subkey_with_flags(path, KEY_SET_VALUE)
            .map(|(k, _)| k)
            .map_err(|e| format!("Couldn't open registry key {path}: {e}"))
    }

    use std::io;
}

impl SystemOps for WindowsSystem {
    fn stop_app(&self) -> Result<(), String> {
        let me = std::process::id();
        let names = [format!("{APP}.exe"), format!("{OLD_APP}.exe")];
        let name_refs: Vec<&str> = names.iter().map(String::as_str).collect();

        let stale = stale_setups(&process_table(), me, "Swiftlane Setup.exe");
        for &pid in &stale {
            Self::kill_pid(pid);
        }
        if !stale.is_empty() {
            write_log(&format!("Closed {} leftover Setup process(es)", stale.len()));
        }

        // Swiftlane is usually closing by itself already (after starting the update); give it up to 3s.
        for _ in 0..10 {
            if processes_to_stop(&process_table(), me, &name_refs).is_empty() {
                return Ok(());
            }
            thread::sleep(Duration::from_millis(300));
        }

        let pids = processes_to_stop(&process_table(), me, &name_refs);
        for &pid in &pids {
            // One by one, never /T: Setup itself may count as part of Swiftlane's tree.
            Self::kill_pid(pid);
        }
        write_log(&format!("Closed {} Swiftlane process(es)", pids.len()));
        thread::sleep(Duration::from_secs(1));
        Ok(())
    }

    fn firewall_rules(&self) -> Result<Vec<(String, String)>, String> {
        let script = format!(
            "Get-NetFirewallRule | Where-Object {{ $_.DisplayName -in {OUR_RULES} }} | \
             ForEach-Object {{ $_.DisplayName + '|' + ($_ | Get-NetFirewallApplicationFilter).Program }}"
        );
        let out = run_ps(&script, &[])?;
        let rules = out
            .lines()
            .filter_map(|line| {
                let (name, prog) = line.split_once('|')?;
                Some((name.trim().to_string(), prog.trim().to_string()))
            })
            .collect();
        Ok(rules)
    }

    fn remove_firewall_rules(&self) -> Result<(), String> {
        let script = format!(
            "Get-NetFirewallRule | Where-Object {{ $_.DisplayName -in {OUR_RULES} }} | Remove-NetFirewallRule"
        );
        run_ps(&script, &[])?;
        Ok(())
    }

    fn add_firewall_rule(&self, name: &str, program: &str) -> Result<(), String> {
        let port = PORT.to_string();
        run_ps(
            "New-NetFirewallRule -DisplayName $env:SL_NAME -Direction Inbound -Action Allow -Profile Private \
             -Program $env:SL_PROG -Protocol TCP -LocalPort $env:SL_PORT | Out-Null",
            &[("SL_NAME", name), ("SL_PROG", program), ("SL_PORT", &port)],
        )?;
        Ok(())
    }

    fn networks(&self) -> Result<Vec<NetworkProfile>, String> {
        let out = run_ps(
            "Get-NetConnectionProfile | ForEach-Object { $_.InterfaceAlias + '|' + $_.Name + '|' + $_.NetworkCategory }",
            &[],
        )?;
        let mut nets = Vec::new();
        for line in out.lines() {
            let mut parts = line.splitn(3, '|');
            if let (Some(alias), Some(name), Some(category)) = (parts.next(), parts.next(), parts.next()) {
                nets.push(NetworkProfile {
                    alias: alias.trim().to_string(),
                    name: name.trim().to_string(),
                    category: category.trim().to_string(),
                });
            }
        }
        Ok(nets)
    }

    fn set_network_category(&self, alias: &str, category: &str) -> Result<(), String> {
        run_ps(
            "Set-NetConnectionProfile -InterfaceAlias $env:SL_ALIAS -NetworkCategory $env:SL_CAT",
            &[("SL_ALIAS", alias), ("SL_CAT", category)],
        )?;
        Ok(())
    }

    #[cfg(windows)]
    fn read_key(&self, hive: RegHive, path: &str) -> Option<BTreeMap<String, RegVal>> {
        let key = win_reg_helpers::open_read(hive, path).ok()?;
        let mut values = BTreeMap::new();
        for item in key.enum_values().flatten() {
            values.insert(item.0, win_reg_helpers::from_reg_value(item.1));
        }
        Some(values)
    }

    #[cfg(not(windows))]
    fn read_key(&self, _hive: RegHive, _path: &str) -> Option<BTreeMap<String, RegVal>> {
        None
    }

    #[cfg(windows)]
    fn write_key(&self, hive: RegHive, path: &str, values: &BTreeMap<String, RegVal>) -> Result<(), String> {
        let key = win_reg_helpers::create_write(hive, path)?;
        for (name, val) in values {
            win_reg_helpers::set_reg_value(&key, name, val)?;
        }
        Ok(())
    }

    #[cfg(not(windows))]
    fn write_key(&self, _hive: RegHive, _path: &str, _values: &BTreeMap<String, RegVal>) -> Result<(), String> {
        Ok(())
    }

    #[cfg(windows)]
    fn delete_key(&self, hive: RegHive, path: &str) -> Result<(), String> {
        match winreg::RegKey::predef(win_reg_helpers::predef(hive)).delete_subkey(path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(format!("Couldn't delete registry key {path}: {e}")),
        }
    }

    #[cfg(not(windows))]
    fn delete_key(&self, _hive: RegHive, _path: &str) -> Result<(), String> {
        Ok(())
    }

    #[cfg(windows)]
    fn read_value(&self, hive: RegHive, path: &str, name: &str) -> Option<RegVal> {
        let key = win_reg_helpers::open_read(hive, path).ok()?;
        key.get_raw_value(name)
            .ok()
            .map(win_reg_helpers::from_reg_value)
    }

    #[cfg(not(windows))]
    fn read_value(&self, _hive: RegHive, _path: &str, _name: &str) -> Option<RegVal> {
        None
    }

    #[cfg(windows)]
    fn write_value(&self, hive: RegHive, path: &str, name: &str, value: Option<&RegVal>) -> Result<(), String> {
        let key = win_reg_helpers::create_write(hive, path)?;
        match value {
            None => match key.delete_value(name) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(format!("Couldn't delete registry value {name}: {e}")),
            },
            Some(v) => win_reg_helpers::set_reg_value(&key, name, v),
        }
    }

    #[cfg(not(windows))]
    fn write_value(&self, _hive: RegHive, _path: &str, _name: &str, _value: Option<&RegVal>) -> Result<(), String> {
        Ok(())
    }

    fn make_shortcut(&self, lnk: &Path, target: &Path, description: &str) -> Result<(), String> {
        let lnk_s = lnk.to_string_lossy();
        let exe_s = target.to_string_lossy();
        run_ps(
            "$s = (New-Object -ComObject WScript.Shell).CreateShortcut($env:SL_LNK); \
             $s.TargetPath = $env:SL_EXE; \
             $s.WorkingDirectory = (Split-Path $env:SL_EXE); \
             $s.Description = $env:SL_DESC; \
             $s.Save()",
            &[("SL_LNK", &lnk_s), ("SL_EXE", &exe_s), ("SL_DESC", description)],
        )?;
        Ok(())
    }
}

// ---------------------------------------------------------------- Transaction & install engine

struct Transaction<'a> {
    undos: Vec<(String, Box<dyn FnOnce() -> Result<(), String> + 'a>)>,
}

impl<'a> Transaction<'a> {
    fn new() -> Self {
        Self { undos: Vec::new() }
    }

    fn push_undo<F>(&mut self, title: &str, undo: F)
    where
        F: FnOnce() -> Result<(), String> + 'a,
    {
        self.undos.push((title.to_string(), Box::new(undo)));
    }

    fn rollback(&mut self) -> Vec<String> {
        let mut errors = Vec::new();
        for (title, undo) in self.undos.drain(..).rev() {
            write_log(&format!("Undoing: {title}"));
            if let Err(e) = undo() {
                errors.push(format!("{title}: {e}"));
            }
        }
        errors
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstallResult {
    pub exe: String,
    pub stamp: String,
}

fn generate_stamp() -> String {
    let mut rnd = [0u8; 3];
    let _ = getrandom::getrandom(&mut rnd);
    format!(
        "{}-{}",
        Local::now().format("%Y%m%d-%H%M%S"),
        hex::encode(rnd)
    )
}

/// Validates a zip entry path against Zip-Slip (never allows `..`, root, drive prefixes, or empty).
fn safe_zip_relpath(raw_name: &str, enclosed: Option<&Path>) -> Result<PathBuf, String> {
    if raw_name.starts_with('/') || raw_name.starts_with('\\') || raw_name.contains('\0') {
        return Err(format!("Unsafe path in installer package: {raw_name}"));
    }
    let rel = enclosed
        .ok_or_else(|| format!("Unsafe path in installer package: {raw_name}"))?;
    let mut clean = PathBuf::new();
    for comp in rel.components() {
        match comp {
            Component::Normal(part) => clean.push(part),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(format!("Unsafe path in installer package: {raw_name}"));
            }
        }
    }
    if clean.as_os_str().is_empty() {
        return Err(format!("Unsafe empty path in installer package: {raw_name}"));
    }
    Ok(clean)
}

fn collect_dlls(dir: &Path, limit: usize) -> Vec<PathBuf> {
    let Ok(rd) = fs::read_dir(dir) else {
        return Vec::new();
    };
    rd.flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e.eq_ignore_ascii_case("dll"))
        })
        .take(limit)
        .collect()
}

fn dir_size_bytes(dir: &Path) -> u64 {
    let mut total = 0u64;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(cur) = stack.pop() {
        let Ok(rd) = fs::read_dir(&cur) else {
            continue;
        };
        for entry in rd.flatten() {
            let p = entry.path();
            if let Ok(meta) = entry.metadata() {
                if meta.is_dir() {
                    stack.push(p);
                } else if meta.is_file() {
                    total = total.saturating_add(meta.len());
                }
            }
        }
    }
    total
}

fn is_retryable_lock_error(e: &io::Error) -> bool {
    e.kind() == io::ErrorKind::PermissionDenied
        || matches!(e.raw_os_error(), Some(5 | 32 | 33))
}

/// Install or update Swiftlane transactionally. On any failure, rolls back every step in reverse
/// order and returns `Err(message)`.
pub fn install<S, P>(
    payload: &PayloadSource,
    choices: &Value,
    system: &S,
    mut progress: P,
    fail_at: Option<&str>,
    hive: RegHive,
) -> Result<InstallResult, String>
where
    S: SystemOps,
    P: FnMut(f64, &str),
{
    let steps_total: f64 = 9.0;
    let mut done: usize = 0;

    let dest_str = choices
        .get("install_dir")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| "Install folder is missing.".to_string())?;
    let dest = PathBuf::from(dest_str);
    let exe = dest.join(format!("{APP}.exe"));
    let stamp = generate_stamp();
    let backup = {
        let base = dest
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| APP.to_string());
        dest.with_file_name(format!("{base}.backup-{stamp}"))
    };

    let mut tx = Transaction::new();

    let run_result: Result<(), String> = (|| {
        macro_rules! begin_step {
            ($title:expr) => {{
                let title: &str = $title;
                write_log(title);
                progress(done as f64 / steps_total, title);
                done += 1;
                if let Some(target) = fail_at {
                    if !target.is_empty() && title.contains(target) {
                        return Err(format!("Simulated failure at: {title}"));
                    }
                }
            }};
        }

        // Step 1: Close running Swiftlane / stale setups
        begin_step!("Closing Swiftlane");
        system.stop_app()?;

        // Step 2: Move old install aside (undo = restore old or remove fresh dest)
        begin_step!("Making room for the new version");
        let had_old = dest.exists();
        {
            let dest_u = dest.clone();
            let backup_u = backup.clone();
            tx.push_undo("Making room for the new version", move || {
                if had_old {
                    if !backup_u.exists() {
                        return Ok(()); // move never happened; old install untouched
                    }
                    let _ = fs::remove_dir_all(&dest_u);
                    fs::rename(&backup_u, &dest_u)
                        .map_err(|e| format!("Couldn't restore old install: {e}"))?;
                } else {
                    let _ = fs::remove_dir_all(&dest_u);
                }
                Ok(())
            });
        }
        if had_old {
            let mut last_err: Option<io::Error> = None;
            let mut moved = false;
            for _ in 0..20 {
                match fs::rename(&dest, &backup) {
                    Ok(()) => {
                        moved = true;
                        break;
                    }
                    Err(e) if is_retryable_lock_error(&e) => {
                        last_err = Some(e);
                        thread::sleep(Duration::from_millis(750));
                    }
                    Err(e) => {
                        return Err(format!("Couldn't move the old Swiftlane folder aside: {e}"));
                    }
                }
            }
            if !moved {
                let mut check_paths = vec![
                    dest.join(format!("{APP}.exe")),
                    dest.join("ffmpeg.exe"),
                    dest.join("yt-dlp.exe"),
                ];
                check_paths.extend(collect_dlls(&dest, 20));
                check_paths.extend(collect_dlls(&dest.join("_internal"), 20));
                let holders = programs_using(&check_paths);
                let by = if holders.is_empty() {
                    " by another program.".to_string()
                } else {
                    let names: Vec<&str> = holders.iter().map(|h| h.name.as_str()).collect();
                    format!(" by: {}.", names.join(", "))
                };
                let detail = last_err.map(|e| e.to_string()).unwrap_or_default();
                return Err(format!(
                    "The old Swiftlane folder is in use{by} Close it and run Setup again. ({detail})"
                ));
            }
        }

        // Step 3: Extract payload.zip safely into dest
        begin_step!("Copying files");
        {
            fs::create_dir_all(&dest)
                .map_err(|e| format!("Couldn't create install folder: {e}"))?;
            let mut archive = payload.open_zip()?;
            let total = archive.len();
            for i in 0..total {
                let mut entry = archive
                    .by_index(i)
                    .map_err(|e| format!("Failed reading installer package entry: {e}"))?;
                let raw_name = entry
                    .name()
                    .map_err(|e| format!("Invalid filename in installer package: {e}"))?
                    .into_owned();
                let is_dir = entry.is_dir() || raw_name.ends_with('/') || raw_name.ends_with('\\');
                let rel = safe_zip_relpath(&raw_name, entry.enclosed_name().as_deref())?;
                let out_path = dest.join(rel);
                if is_dir {
                    fs::create_dir_all(&out_path)
                        .map_err(|e| format!("Couldn't create directory {}: {e}", out_path.display()))?;
                } else {
                    if let Some(parent) = out_path.parent() {
                        fs::create_dir_all(parent).map_err(|e| {
                            format!("Couldn't create directory {}: {e}", parent.display())
                        })?;
                    }
                    let mut out_file = File::create(&out_path)
                        .map_err(|e| format!("Couldn't write {}: {e}", out_path.display()))?;
                    io::copy(&mut entry, &mut out_file)
                        .map_err(|e| format!("Couldn't extract {}: {e}", out_path.display()))?;
                }
                if i % 50 == 0 && total > 0 {
                    let frac = ((done - 1) as f64 + i as f64 / total as f64) / steps_total;
                    progress(frac, "Copying files");
                }
            }
            if !exe.is_file() {
                return Err("The installer package is incomplete (Swiftlane.exe missing).".into());
            }
        }

        // Step 4: Write setup-choices.json (never touching pairing code in AppData) & create save_dir
        begin_step!("Saving your choices");
        {
            const KEEP_KEYS: &[&str] = &[
                "save_dir",
                "sort_files",
                "download_dir",
                "calc_angle",
                "language",
                "notify",
                "features",
                "theme",
            ];
            let mut data = Map::new();
            if let Some(obj) = choices.as_object() {
                for &k in KEEP_KEYS {
                    if let Some(v) = obj.get(k) {
                        data.insert(k.to_string(), v.clone());
                    }
                }
                if let Some(op) = obj.get("open_page").and_then(Value::as_str) {
                    if !op.is_empty() {
                        data.insert("open_page".into(), Value::String(op.to_string()));
                    }
                }
            }
            data.insert("stamp".into(), Value::String(stamp.clone()));
            let json_text = serde_json::to_string_pretty(&Value::Object(data))
                .map_err(|e| format!("Couldn't format setup choices: {e}"))?;
            fs::write(dest.join("setup-choices.json"), json_text)
                .map_err(|e| format!("Couldn't write setup-choices.json: {e}"))?;

            if let Some(save_dir) = choices.get("save_dir").and_then(Value::as_str) {
                if !save_dir.trim().is_empty() {
                    fs::create_dir_all(save_dir)
                        .map_err(|e| format!("Couldn't create received-files folder: {e}"))?;
                }
            }
        }

        // Step 5: Firewall rules
        let old_rules = system.firewall_rules()?;
        begin_step!("Setting up the firewall");
        {
            let old_rules_u = old_rules.clone();
            tx.push_undo("Setting up the firewall", move || {
                system.remove_firewall_rules()?;
                for (name, prog) in &old_rules_u {
                    system.add_firewall_rule(name, prog)?;
                }
                Ok(())
            });
        }
        system.remove_firewall_rules()?;
        let want_fw = choices
            .get("firewall")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let want_transfer = choices
            .get("features")
            .and_then(|f| f.get("transfer"))
            .and_then(Value::as_bool)
            .unwrap_or(true);
        if want_fw && want_transfer {
            let exe_s = exe.to_string_lossy();
            system.add_firewall_rule(APP, &exe_s)?;
        }

        // Step 6: Network profile categories
        let changed: Rc<RefCell<Vec<(String, String)>>> = Rc::new(RefCell::new(Vec::new()));
        begin_step!("Setting up your network");
        {
            let changed_u = Rc::clone(&changed);
            tx.push_undo("Setting up your network", move || {
                for (alias, before) in changed_u.borrow().iter() {
                    system.set_network_category(alias, before)?;
                }
                Ok(())
            });
        }
        if let Some(aliases) = choices.get("private_networks").and_then(Value::as_array) {
            for alias_v in aliases {
                let Some(alias) = alias_v.as_str() else {
                    continue;
                };
                let nets = system.networks()?;
                if let Some(net) = nets.into_iter().find(|n| n.alias == alias) {
                    if net.category != "Private" {
                        system.set_network_category(alias, "Private")?;
                        changed.borrow_mut().push((alias.to_string(), net.category));
                    }
                }
            }
        }

        // Step 7: Shortcuts (remember existing files byte-for-byte)
        let start_lnk = system.start_menu_dir().join(format!("{APP}.lnk"));
        let desk_lnk = system.desktop_dir().join(format!("{APP}.lnk"));
        let want_start = choices
            .get("start_menu")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let want_desk = choices
            .get("desktop")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let links = [(start_lnk, want_start), (desk_lnk, want_desk)];
        let saved_links: Vec<(PathBuf, Option<Vec<u8>>)> = links
            .iter()
            .map(|(p, _)| (p.clone(), fs::read(p).ok()))
            .collect();

        begin_step!("Creating shortcuts");
        {
            tx.push_undo("Creating shortcuts", move || {
                for (lnk, data) in saved_links {
                    match data {
                        None => {
                            let _ = fs::remove_file(&lnk);
                        }
                        Some(bytes) => {
                            if let Some(parent) = lnk.parent() {
                                let _ = fs::create_dir_all(parent);
                            }
                            fs::write(&lnk, bytes)
                                .map_err(|e| format!("Couldn't restore {}: {e}", lnk.display()))?;
                        }
                    }
                }
                Ok(())
            });
        }
        for (lnk, wanted) in &links {
            if *wanted {
                if let Some(parent) = lnk.parent() {
                    fs::create_dir_all(parent)
                        .map_err(|e| format!("Couldn't create shortcut folder: {e}"))?;
                }
                system.make_shortcut(
                    lnk,
                    &exe,
                    "Send files between your phone and this PC, plus everyday tools",
                )?;
            } else if lnk.exists() {
                fs::remove_file(lnk)
                    .map_err(|e| format!("Couldn't remove shortcut {}: {e}", lnk.display()))?;
            }
        }

        // Step 8: Start with Windows (HKCU Run)
        let old_run = system.read_value(RegHive::Hkcu, RUN_KEY, APP);
        begin_step!("Start with Windows");
        {
            let old_run_u = old_run.clone();
            tx.push_undo("Start with Windows", move || {
                system.write_value(RegHive::Hkcu, RUN_KEY, APP, old_run_u.as_ref())
            });
        }
        if choices
            .get("autostart")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            let val = RegVal::Sz(format!("\"{}\" --minimized", exe.display()));
            system.write_value(RegHive::Hkcu, RUN_KEY, APP, Some(&val))?;
        } else {
            system.write_value(RegHive::Hkcu, RUN_KEY, APP, None)?;
        }

        // Step 9: Register in Settings > Apps (HKLM Uninstall)
        let old_key = system.read_key(hive, UNINSTALL_KEY);
        begin_step!("Registering in Settings > Apps");
        {
            let old_key_u = old_key.clone();
            tx.push_undo("Registering in Settings > Apps", move || {
                system.delete_key(hive, UNINSTALL_KEY)?;
                if let Some(prev) = &old_key_u {
                    system.write_key(hive, UNINSTALL_KEY, prev)?;
                }
                Ok(())
            });
        }
        let version = choices
            .get("version")
            .and_then(Value::as_str)
            .unwrap_or(payload.version());
        let size_kb = (dir_size_bytes(&dest) / 1024).min(u32::MAX as u64) as u32;
        system.delete_key(hive, UNINSTALL_KEY)?;
        let mut reg_entries = BTreeMap::new();
        reg_entries.insert("DisplayName".into(), RegVal::Sz(APP.into()));
        reg_entries.insert("DisplayVersion".into(), RegVal::Sz(version.into()));
        reg_entries.insert("Publisher".into(), RegVal::Sz(APP.into()));
        reg_entries.insert("DisplayIcon".into(), RegVal::Sz(exe.to_string_lossy().into_owned()));
        reg_entries.insert(
            "InstallLocation".into(),
            RegVal::Sz(dest.to_string_lossy().into_owned()),
        );
        reg_entries.insert(
            "UninstallString".into(),
            RegVal::Sz(format!("\"{}\" --uninstall", exe.display())),
        );
        reg_entries.insert("EstimatedSize".into(), RegVal::Dword(size_kb));
        reg_entries.insert("NoModify".into(), RegVal::Dword(1));
        reg_entries.insert("NoRepair".into(), RegVal::Dword(1));
        system.write_key(hive, UNINSTALL_KEY, &reg_entries)?;

        Ok(())
    })();

    if let Err(err) = run_result {
        write_log(&format!("FAILED: {err}"));
        let rollback_errors = tx.rollback();
        if rollback_errors.is_empty() {
            write_log("Rollback finished, everything restored");
            return Err(err);
        }
        let joined = rollback_errors.join("; ");
        write_log(&format!("Rollback finished with problems: {joined}"));
        return Err(format!("{err}  (Some things couldn't be undone: {joined})"));
    }

    // Commit phase (none of this can break the new install).
    progress(0.95, "Cleaning up");
    let _ = fs::remove_dir_all(&backup);
    cleanup_old_name(system, hive);
    progress(1.0, "Done");

    Ok(InstallResult {
        exe: exe.to_string_lossy().into_owned(),
        stamp,
    })
}

fn cleanup_old_name<S: SystemOps>(system: &S, hive: RegHive) {
    let old = system.program_files_dir().join(OLD_APP);
    let _ = fs::remove_dir_all(old);
    let _ = fs::remove_file(system.start_menu_dir().join(format!("{OLD_APP}.lnk")));
    let _ = system.delete_key(
        hive,
        &format!(r"Software\Microsoft\Windows\CurrentVersion\Uninstall\{OLD_APP}"),
    );
    let _ = system.write_value(RegHive::Hkcu, RUN_KEY, OLD_APP, None);
}

/// `(version, install_dir)` of an existing Swiftlane install, or `None`.
pub fn installed_version<S: SystemOps>(system: &S) -> Option<(String, String)> {
    let values = system.read_key(RegHive::Hklm, UNINSTALL_KEY)?;
    let ver = values
        .get("DisplayVersion")
        .and_then(RegVal::as_str)
        .unwrap_or("?")
        .to_string();
    let dir = values
        .get("InstallLocation")
        .and_then(RegVal::as_str)
        .unwrap_or("")
        .to_string();
    Some((ver, dir))
}

// ---------------------------------------------------------------- Unit tests

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::cell::RefCell;

    fn fresh_temp_dir(prefix: &str) -> PathBuf {
        let mut b = [0u8; 6];
        getrandom::getrandom(&mut b).unwrap();
        let d = std::env::temp_dir().join(format!("swl-setup-{prefix}-{}", hex::encode(b)));
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn make_test_zip(path: &Path, files: &[(&str, &[u8])]) {
        let f = File::create(path).unwrap();
        let mut zw = zip::ZipWriter::new(f);
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        for (name, data) in files {
            zw.start_file(*name, opts).unwrap();
            zw.write_all(data).unwrap();
        }
        zw.finish().unwrap();
    }

    struct FakeSystem {
        firewall: RefCell<Vec<(String, String)>>,
        nets: RefCell<Vec<NetworkProfile>>,
        registry: RefCell<HashMap<(RegHive, String), BTreeMap<String, RegVal>>>,
        start_menu: PathBuf,
        desktop: PathBuf,
        program_files: PathBuf,
        stopped: RefCell<usize>,
    }

    impl FakeSystem {
        fn new(root: &Path) -> Self {
            let start_menu = root.join("start_menu");
            let desktop = root.join("desktop");
            let program_files = root.join("program_files");
            fs::create_dir_all(&start_menu).unwrap();
            fs::create_dir_all(&desktop).unwrap();
            fs::create_dir_all(&program_files).unwrap();
            Self {
                firewall: RefCell::new(Vec::new()),
                nets: RefCell::new(vec![NetworkProfile {
                    alias: "Wi-Fi".into(),
                    name: "HomeNet".into(),
                    category: "Public".into(),
                }]),
                registry: RefCell::new(HashMap::new()),
                start_menu,
                desktop,
                program_files,
                stopped: RefCell::new(0),
            }
        }
    }

    impl SystemOps for FakeSystem {
        fn stop_app(&self) -> Result<(), String> {
            *self.stopped.borrow_mut() += 1;
            Ok(())
        }
        fn firewall_rules(&self) -> Result<Vec<(String, String)>, String> {
            Ok(self.firewall.borrow().clone())
        }
        fn remove_firewall_rules(&self) -> Result<(), String> {
            self.firewall.borrow_mut().clear();
            Ok(())
        }
        fn add_firewall_rule(&self, name: &str, program: &str) -> Result<(), String> {
            self.firewall
                .borrow_mut()
                .push((name.to_string(), program.to_string()));
            Ok(())
        }
        fn networks(&self) -> Result<Vec<NetworkProfile>, String> {
            Ok(self.nets.borrow().clone())
        }
        fn set_network_category(&self, alias: &str, category: &str) -> Result<(), String> {
            for n in self.nets.borrow_mut().iter_mut() {
                if n.alias == alias {
                    n.category = category.to_string();
                }
            }
            Ok(())
        }
        fn read_key(&self, hive: RegHive, path: &str) -> Option<BTreeMap<String, RegVal>> {
            self.registry
                .borrow()
                .get(&(hive, path.to_string()))
                .cloned()
        }
        fn write_key(
            &self,
            hive: RegHive,
            path: &str,
            values: &BTreeMap<String, RegVal>,
        ) -> Result<(), String> {
            self.registry
                .borrow_mut()
                .insert((hive, path.to_string()), values.clone());
            Ok(())
        }
        fn delete_key(&self, hive: RegHive, path: &str) -> Result<(), String> {
            self.registry.borrow_mut().remove(&(hive, path.to_string()));
            Ok(())
        }
        fn read_value(&self, hive: RegHive, path: &str, name: &str) -> Option<RegVal> {
            self.registry
                .borrow()
                .get(&(hive, path.to_string()))
                .and_then(|m| m.get(name))
                .cloned()
        }
        fn write_value(
            &self,
            hive: RegHive,
            path: &str,
            name: &str,
            value: Option<&RegVal>,
        ) -> Result<(), String> {
            let mut reg = self.registry.borrow_mut();
            let entry = reg.entry((hive, path.to_string())).or_default();
            match value {
                Some(v) => {
                    entry.insert(name.to_string(), v.clone());
                }
                None => {
                    entry.remove(name);
                }
            }
            Ok(())
        }
        fn make_shortcut(&self, lnk: &Path, target: &Path, description: &str) -> Result<(), String> {
            fs::write(lnk, format!("LNK:{}|{}", target.display(), description))
                .map_err(|e| e.to_string())
        }
        fn start_menu_dir(&self) -> PathBuf {
            self.start_menu.clone()
        }
        fn desktop_dir(&self) -> PathBuf {
            self.desktop.clone()
        }
        fn program_files_dir(&self) -> PathBuf {
            self.program_files.clone()
        }
    }

    #[test]
    fn processes_to_stop_never_kills_self_or_children_but_stops_parent_and_siblings() {
        let mut table: ProcessTable = HashMap::new();
        table.insert(1, (0, "explorer.exe".into()));
        table.insert(10, (1, "Swiftlane.exe".into()));
        table.insert(11, (10, "msedgewebview2.exe".into())); // sibling of Setup under Swiftlane
        table.insert(12, (11, "msedgewebview2.exe".into())); // grandchild of Swiftlane
        table.insert(20, (10, "Swiftlane Setup.exe".into())); // me (launched by Swiftlane --update)
        table.insert(21, (20, "msedgewebview2.exe".into())); // Setup's own WebView2 child
        table.insert(30, (1, "PocketDrop.exe".into()));
        table.insert(31, (30, "helper.exe".into()));
        table.insert(99, (1, "notepad.exe".into()));

        let to_stop = processes_to_stop(&table, 20, &["Swiftlane.exe", "PocketDrop.exe"]);
        assert_eq!(to_stop, vec![10, 11, 12, 30, 31]);
        assert!(!to_stop.contains(&20), "must never stop Setup itself");
        assert!(!to_stop.contains(&21), "must never stop Setup's children");
        assert!(!to_stop.contains(&1), "must never stop explorer");
        assert!(!to_stop.contains(&99), "must never stop unrelated processes");
    }

    #[test]
    fn stale_setups_kills_unrelated_copies_never_self_or_chain() {
        let mut table: ProcessTable = HashMap::new();
        table.insert(1, (0, "explorer.exe".into()));
        table.insert(5, (1, "Swiftlane Setup.exe".into())); // parent Setup that restarted us
        table.insert(10, (5, "Swiftlane Setup.exe".into())); // me
        table.insert(11, (10, "Swiftlane Setup.exe".into())); // child helper
        table.insert(40, (1, "Swiftlane Setup.exe".into())); // stale unrelated copy
        table.insert(41, (40, "Swiftlane Setup.exe".into())); // stale child
        table.insert(50, (1, "notepad.exe".into()));

        let stale = stale_setups(&table, 10, "Swiftlane Setup.exe");
        assert_eq!(stale, vec![40, 41]);
    }

    #[test]
    fn trailer_parse_and_payload_read() {
        let dir = fresh_temp_dir("trailer");
        let zip_path = dir.join("inner.zip");
        make_test_zip(
            &zip_path,
            &[
                ("Swiftlane.exe", b"MZ-fake-swiftlane-binary"),
                ("uninstall.ps1", b"# uninstall"),
            ],
        );
        let zip_bytes = fs::read(&zip_path).unwrap();

        let fake_exe = dir.join("Swiftlane Setup.exe");
        let stub = b"MZ-fake-setup-stub-header-1234567890";
        let offset = stub.len() as u64;
        let len = zip_bytes.len() as u64;
        let mut ver_buf = [0u8; 16];
        let ver_str = b"2.3.2";
        ver_buf[..ver_str.len()].copy_from_slice(ver_str);

        let mut f = File::create(&fake_exe).unwrap();
        f.write_all(stub).unwrap();
        f.write_all(&zip_bytes).unwrap();
        f.write_all(TRAILER_MAGIC).unwrap();
        f.write_all(&offset.to_le_bytes()).unwrap();
        f.write_all(&len.to_le_bytes()).unwrap();
        f.write_all(&ver_buf).unwrap();
        drop(f);

        let parsed = PayloadSource::parse_trailer(&fake_exe).unwrap().unwrap();
        assert_eq!(parsed.version(), "2.3.2");
        assert_eq!(
            parsed,
            PayloadSource::Embedded {
                exe_path: fake_exe.clone(),
                offset,
                len,
                version: "2.3.2".into(),
            }
        );

        let mut archive = parsed.open_zip().unwrap();
        assert_eq!(archive.len(), 2);
        let mut content = Vec::new();
        archive
            .by_name("Swiftlane.exe")
            .unwrap()
            .read_to_end(&mut content)
            .unwrap();
        assert_eq!(content, b"MZ-fake-swiftlane-binary");
        assert_eq!(
            parsed.uncompressed_size(),
            (b"MZ-fake-swiftlane-binary".len() + b"# uninstall".len()) as u64
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn install_and_rollback_on_simulated_failure() {
        let dir = fresh_temp_dir("tx");
        let zip_path = dir.join("payload.zip");
        make_test_zip(
            &zip_path,
            &[
                ("Swiftlane.exe", b"NEW-EXE-V232"),
                ("licenses/SOURCES.txt", b"MIT"),
            ],
        );
        let payload = PayloadSource::File {
            path: zip_path,
            version: "2.3.2".into(),
        };

        let sys = FakeSystem::new(&dir);
        let install_dir = sys.program_files.join("Swiftlane");
        let save_dir = dir.join("Received");

        // Pre-populate an existing old install, firewall rule, shortcut, autostart, and Uninstall key
        fs::create_dir_all(&install_dir).unwrap();
        fs::write(install_dir.join("Swiftlane.exe"), b"OLD-EXE-V231").unwrap();
        sys.firewall
            .borrow_mut()
            .push(("Swiftlane".into(), r"C:\Old\Swiftlane.exe".into()));
        let old_start_lnk = sys.start_menu.join("Swiftlane.lnk");
        fs::write(&old_start_lnk, b"OLD-START-LINK").unwrap();
        sys.write_value(
            RegHive::Hkcu,
            RUN_KEY,
            APP,
            Some(&RegVal::Sz("OLD-RUN".into())),
        )
        .unwrap();
        let mut old_uninst = BTreeMap::new();
        old_uninst.insert("DisplayVersion".into(), RegVal::Sz("2.3.1".into()));
        old_uninst.insert(
            "InstallLocation".into(),
            RegVal::Sz(install_dir.to_string_lossy().into_owned()),
        );
        sys.write_key(RegHive::Hklm, UNINSTALL_KEY, &old_uninst)
            .unwrap();

        let choices = json!({
            "install_dir": install_dir.to_string_lossy(),
            "save_dir": save_dir.to_string_lossy(),
            "sort_files": true,
            "download_dir": "",
            "calc_angle": "deg",
            "language": "en",
            "notify": true,
            "features": { "transfer": true, "biosusb": true, "remote": false },
            "theme": { "accent": "cyan", "custom": "#14b8e0", "mode": "dark" },
            "firewall": true,
            "private_networks": ["Wi-Fi"],
            "start_menu": true,
            "desktop": true,
            "autostart": true,
            "version": "2.3.2"
        });

        // 1) Simulated failure on final step must roll EVERYTHING back cleanly
        let err = install(
            &payload,
            &choices,
            &sys,
            |_, _| {},
            Some("Registering in Settings > Apps"),
            RegHive::Hklm,
        )
        .unwrap_err();
        assert!(err.contains("Simulated failure"), "unexpected error: {err}");
        assert_eq!(
            fs::read(install_dir.join("Swiftlane.exe")).unwrap(),
            b"OLD-EXE-V231",
            "old install must be restored"
        );
        assert_eq!(
            *sys.firewall.borrow(),
            vec![("Swiftlane".into(), r"C:\Old\Swiftlane.exe".into())],
            "old firewall rules must be restored"
        );
        assert_eq!(
            sys.nets.borrow()[0].category,
            "Public",
            "network category must be restored to Public"
        );
        assert_eq!(
            fs::read(&old_start_lnk).unwrap(),
            b"OLD-START-LINK",
            "old start menu shortcut must be restored byte-for-byte"
        );
        assert!(
            !sys.desktop.join("Swiftlane.lnk").exists(),
            "newly created desktop shortcut must be removed on rollback"
        );
        assert_eq!(
            sys.read_value(RegHive::Hkcu, RUN_KEY, APP),
            Some(RegVal::Sz("OLD-RUN".into())),
            "old autostart entry must be restored"
        );
        assert_eq!(
            installed_version(&sys),
            Some(("2.3.1".into(), install_dir.to_string_lossy().into_owned())),
            "old Uninstall registry key must be intact"
        );

        // 2) Full install succeeds and writes all expected artifacts & registry entries
        let mut last_progress = 0.0f64;
        let res = install(
            &payload,
            &choices,
            &sys,
            |frac, _| {
                last_progress = frac;
            },
            None,
            RegHive::Hklm,
        )
        .unwrap();
        assert_eq!(last_progress, 1.0);
        assert_eq!(
            fs::read(install_dir.join("Swiftlane.exe")).unwrap(),
            b"NEW-EXE-V232"
        );
        assert!(install_dir.join("licenses").join("SOURCES.txt").is_file());
        assert!(save_dir.is_dir());

        let saved_choices: Value =
            serde_json::from_str(&fs::read_to_string(install_dir.join("setup-choices.json")).unwrap())
                .unwrap();
        assert_eq!(saved_choices["stamp"], json!(res.stamp));
        assert_eq!(saved_choices["features"]["biosusb"], json!(true));
        assert!(saved_choices.get("code").is_none(), "pairing code must not be overwritten");

        assert_eq!(
            *sys.firewall.borrow(),
            vec![("Swiftlane".into(), res.exe.clone())]
        );
        assert_eq!(sys.nets.borrow()[0].category, "Private");
        assert!(sys.desktop.join("Swiftlane.lnk").is_file());
        assert!(sys.start_menu.join("Swiftlane.lnk").is_file());
        assert_eq!(
            sys.read_value(RegHive::Hkcu, RUN_KEY, APP),
            Some(RegVal::Sz(format!("\"{}\" --minimized", res.exe)))
        );
        assert_eq!(
            installed_version(&sys),
            Some(("2.3.2".into(), install_dir.to_string_lossy().into_owned()))
        );

        let _ = fs::remove_dir_all(&dir);
    }
}
