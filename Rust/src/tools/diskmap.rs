//! Disk space map: what takes up the room on a drive or in a folder. A port of
//! Source/toolkit/diskmap.py plus the disk_* methods of Source/swiftlane.py.
//!
//! Walks the folder tree (sizes come with the directory listing, so it's quick), never following
//! junctions or symbolic links: nothing is counted twice and the walk can't loop. Files under 1 MB
//! are only counted per folder to keep memory small; bigger ones are kept by name.
//! The only way to remove anything is the Recycle Bin, and never for Windows, programs, app data,
//! your user folders, or drives without a Recycle Bin.

use serde_json::{json, Value};
use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;
use tauri::{AppHandle, Manager};

use super::arg;
use crate::api::{push, AppState};
use crate::config;
use crate::jobs::JobError;

const BIG: u64 = 1 << 20; // files at least this size are listed by name
const TOP_FILES: usize = 60;
const NONE: usize = usize::MAX;

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
        Node { name, parent, size: 0, files: 0, dirs: Vec::new(), big: Vec::new(), small_size: 0, small_count: 0, denied: false }
    }
}

/// A scanned folder tree; node 0 is the folder that was scanned (its name is its full path).
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

/// Scans a folder. `progress(entries seen, current folder)` comes every 2000 entries or so.
pub fn scan(root: &Path, mut progress: impl FnMut(u64, &Path), cancelled: impl Fn() -> bool) -> Result<Tree, ScanError> {
    let root_name: PathBuf = root.components().collect();
    let mut nodes = vec![Node::new(root_name.to_string_lossy().to_string(), NONE)];
    let mut top: BinaryHeap<Reverse<(u64, String)>> = BinaryHeap::new();
    let mut stack = vec![0usize];
    let mut seen = 0u64;
    while let Some(i) = stack.pop() {
        if cancelled() {
            return Err(ScanError::Cancelled);
        }
        let dir = path_of(&nodes, i);
        let entries: Vec<std::fs::DirEntry> = match std::fs::read_dir(&dir) {
            Ok(it) => it.filter_map(Result::ok).collect(),
            Err(_) => {
                nodes[i].denied = true;
                continue;
            }
        };
        for e in &entries {
            // Junctions, mount points and symbolic links lead elsewhere: skipped. (Other reparse
            // points, like OneDrive's online-only files, are real content.)
            let Ok(kind) = e.file_type() else { continue };
            if kind.is_symlink() {
                continue;
            }
            let name = e.file_name().to_string_lossy().to_string();
            if kind.is_dir() {
                let child = nodes.len();
                nodes.push(Node::new(name, i));
                nodes[i].dirs.push(child);
                stack.push(child);
                continue;
            }
            let Ok(meta) = e.metadata() else { continue };
            let size = meta.len();
            let node = &mut nodes[i];
            node.files += 1;
            node.size += size;
            if size >= BIG {
                let item = (size, e.path().to_string_lossy().to_string());
                node.big.push((name, size));
                if top.len() < TOP_FILES {
                    top.push(Reverse(item));
                } else if top.peek().is_some_and(|m| size > m.0 .0) {
                    top.pop();
                    top.push(Reverse(item));
                }
            } else {
                node.small_size += size;
                node.small_count += 1;
            }
        }
        let before = seen;
        seen += entries.len() as u64;
        if seen / 2000 != before / 2000 {
            progress(seen, &dir);
        }
    }
    // Children always come after their parents, so going backwards adds sizes upwards.
    for i in (1..nodes.len()).rev() {
        let (size, files, parent) = (nodes[i].size, nodes[i].files, nodes[i].parent);
        nodes[parent].size += size;
        nodes[parent].files += files;
    }
    let mut top: Vec<(u64, String)> = top.into_iter().map(|r| r.0).collect();
    top.sort_by(|a, b| b.cmp(a));
    Ok(Tree { nodes, top })
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
        "disk_view" => disk_view(Some(s).filter(|s| !s.is_empty())),
        "disk_reveal" => {
            let ok = DISK.lock().unwrap().as_ref().is_some_and(|t| in_scan(t, s)) && Path::new(s).symlink_metadata().is_ok();
            if ok {
                crate::winapi::reveal(Path::new(s));
            }
            Value::Null
        }
        "disk_trash" => trash(s),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
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
}
