"""Disk space map: what takes up the room on a drive or in a folder.

Walks the folder tree with os.scandir (sizes come with the directory listing, so it's quick), never
following junctions or symbolic links: nothing is counted twice and the walk can't loop. Files under
1 MB are only counted per folder to keep memory small; bigger ones are kept by name.
"""
import ctypes, heapq, os, shutil, stat, string, sys
from pathlib import Path

from .common import Cancelled

BIG = 1 << 20          # files at least this size are listed by name
TOP_FILES = 60


class Folder:
    __slots__ = ("name", "parent", "size", "files", "dirs", "big", "small_size", "small_count", "denied")

    def __init__(self, name, parent=None):
        self.name, self.parent = name, parent
        self.size = self.files = self.small_size = self.small_count = 0
        self.dirs, self.big = [], []
        self.denied = False

    def path(self):
        parts, node = [], self
        while node:
            parts.append(node.name)
            node = node.parent
        return str(Path(*reversed(parts)))


def _is_link(entry):
    try:
        st = entry.stat(follow_symlinks=False)
    except OSError:
        return True
    if entry.is_symlink():
        return True
    # Junctions and mount points lead elsewhere; other reparse points (OneDrive, deduplication) are real content.
    return getattr(st, "st_reparse_tag", 0) in (0xA0000003, 0xA000000C)


def scan(root, progress=None, cancelled=None):
    """Returns (root Folder, top files [(size, path)], stats)."""
    root = Folder(str(Path(root)))
    top, seen, stack = [], 0, [root]
    order = []
    while stack:
        if cancelled and cancelled():
            raise Cancelled()
        node = stack.pop()
        order.append(node)
        try:
            with os.scandir(node.path()) as it:
                entries = list(it)
        except OSError:
            node.denied = True
            continue
        for e in entries:
            try:
                if _is_link(e):
                    continue
                if e.is_dir(follow_symlinks=False):
                    child = Folder(e.name, node)
                    node.dirs.append(child)
                    stack.append(child)
                    continue
                size = e.stat(follow_symlinks=False).st_size
            except OSError:
                continue
            node.files += 1
            node.size += size
            if size >= BIG:
                node.big.append((e.name, size))
                item = (size, e.path)
                if len(top) < TOP_FILES:
                    heapq.heappush(top, item)
                elif size > top[0][0]:
                    heapq.heapreplace(top, item)
            else:
                node.small_size += size
                node.small_count += 1
        seen += len(entries)
        if progress and seen // 2000 != (seen - len(entries)) // 2000:
            progress(seen, node.path())
    for node in reversed(order):  # children before parents: add sizes upwards
        for d in node.dirs:
            node.size += d.size
            node.files += d.files
    return root, sorted(top, reverse=True), {"entries": seen, "folders": len(order)}


def find(root, path):
    """The Folder for a path inside the scanned tree, or None."""
    rel = os.path.relpath(path, root.name)
    node = root
    if rel == ".":
        return node
    for part in Path(rel).parts:
        if part == "..":
            return None
        node = next((d for d in node.dirs if d.name == part), None)
        if node is None:
            return None
    return node


def view(node, limit=150):
    """Children of a folder, biggest first, as plain dicts for the window."""
    base = node.path()
    items = [{"name": d.name, "path": os.path.join(base, d.name), "size": d.size, "kind": "dir", "count": d.files, "denied": d.denied}
             for d in node.dirs]
    items += [{"name": n, "path": os.path.join(base, n), "size": s, "kind": "file"} for n, s in node.big]
    if node.small_count:
        items.append({"name": "Smaller files", "path": None, "size": node.small_size, "kind": "small", "count": node.small_count})
    items.sort(key=lambda x: -x["size"])
    if len(items) > limit:
        rest = items[limit:]
        items = items[:limit] + [{"name": f"{len(rest)} more", "path": None, "size": sum(x["size"] for x in rest), "kind": "small", "count": len(rest)}]
    crumbs, n = [], node
    while n:
        crumbs.append({"name": n.name, "path": n.path()})
        n = n.parent
    return {"path": base, "size": node.size, "files": node.files, "denied": node.denied, "items": items, "crumbs": crumbs[::-1]}


def remove_from_tree(root, path, size):
    """After something went to the Recycle Bin: take it out of the tree and the totals."""
    parent = find(root, str(Path(path).parent))
    if parent is None:
        return
    name = Path(path).name
    d = next((x for x in parent.dirs if x.name == name), None)
    files = 0
    if d:
        parent.dirs.remove(d)
        files = d.files
    else:
        parent.big = [b for b in parent.big if b[0] != name]
        files = 1
    n = parent
    while n:
        n.size -= size
        n.files -= files
        n = n.parent


def drives():
    out = []
    if sys.platform != "win32":
        return [{"path": "/", "label": "/", **_usage("/")}]
    mask = ctypes.windll.kernel32.GetLogicalDrives()
    for i, letter in enumerate(string.ascii_uppercase):
        if not mask & (1 << i):
            continue
        root = f"{letter}:\\"
        kind = ctypes.windll.kernel32.GetDriveTypeW(root)
        if kind not in (2, 3):  # removable, fixed (skip network, CD, RAM disks)
            continue
        name = ctypes.create_unicode_buffer(261)
        ctypes.windll.kernel32.GetVolumeInformationW(root, name, 261, None, None, None, None, 0)
        try:
            out.append({"path": root, "label": name.value or ("USB drive" if kind == 2 else "Local disk"), "removable": kind == 2, **_usage(root)})
        except OSError:
            pass
    return out


def _usage(root):
    u = shutil.disk_usage(root)
    return {"total": u.total, "used": u.used, "free": u.free}


def protected(path):
    """Things the map never offers to delete: drive roots, Windows, programs, your profile's own folders."""
    p = Path(path).resolve()
    if p.parent == p:
        return True
    env = os.environ
    temp = Path(env.get("TEMP", "") or "\\nowhere").resolve()
    if temp in p.parents:  # leftovers inside the Temp folder are fair game
        return False
    guarded = [env.get("SystemRoot", r"C:\Windows"), env.get("ProgramFiles", r"C:\Program Files"),
               env.get("ProgramFiles(x86)", r"C:\Program Files (x86)"), env.get("ProgramData", r"C:\ProgramData"),
               env.get("APPDATA", ""), env.get("LOCALAPPDATA", "")]
    for g in filter(None, guarded):
        g = Path(g).resolve()
        if p == g or g in p.parents:
            return True
    home = Path.home().resolve()
    if p == home or p.parent == home or p == home.parent:  # C:\Users, your profile, Documents/Desktop/...
        return True
    if p.name.lower() in ("$recycle.bin", "system volume information", "pagefile.sys", "hiberfil.sys", "swapfile.sys"):
        return True
    return False


def to_recycle_bin(path):
    """Moves a file or folder to the Recycle Bin (can be restored from there)."""
    if protected(path):
        raise PermissionError("Swiftlane doesn't delete this: it belongs to Windows, a program, or your user folder.")
    if not os.path.lexists(path):
        raise FileNotFoundError(path)
    drive = os.path.splitdrive(str(Path(path).resolve()))[0] + "\\"
    if ctypes.windll.kernel32.GetDriveTypeW(drive) != 3:  # USB sticks and network drives have no Recycle Bin: it would be gone for good
        raise PermissionError("This drive has no Recycle Bin, so Swiftlane won't delete from it. Use File Explorer if you're sure.")
    from ctypes import wintypes

    class SHFILEOPSTRUCTW(ctypes.Structure):
        _fields_ = [("hwnd", wintypes.HWND), ("wFunc", wintypes.UINT), ("pFrom", wintypes.LPCWSTR), ("pTo", wintypes.LPCWSTR),
                    ("fFlags", ctypes.c_ushort), ("fAnyOperationsAborted", wintypes.BOOL), ("hNameMappings", ctypes.c_void_p),
                    ("lpszProgressTitle", wintypes.LPCWSTR)]
    FO_DELETE, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_SILENT, FOF_NOERRORUI = 3, 0x40, 0x10, 0x4, 0x400
    op = SHFILEOPSTRUCTW(None, FO_DELETE, str(Path(path)) + "\0\0", None, FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_SILENT | FOF_NOERRORUI)
    rc = ctypes.windll.shell32.SHFileOperationW(ctypes.byref(op))
    if rc or op.fAnyOperationsAborted or os.path.lexists(path):
        raise OSError(f"Windows couldn't move it to the Recycle Bin (code {rc}). It may be open in another program.")
