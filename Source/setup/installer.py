"""Swiftlane Setup's install engine, with rollback.

Every step registers how to undo itself. If any step fails, everything already done is
undone in reverse order, so the PC ends up exactly as before (like Inno Setup's rollback).
Windows-specific actions (firewall, registry, network, shortcuts, processes) live in
WindowsSystem so the engine can be tested with a fake one.
"""
import json
import os
import shutil
import subprocess
import time
import uuid
import zipfile
from pathlib import Path

APP = "Swiftlane"
OLD_APP = "PocketDrop"
PORT = 8765
UNINSTALL_KEY = r"Software\Microsoft\Windows\CurrentVersion\Uninstall\Swiftlane"
RUN_KEY = r"Software\Microsoft\Windows\CurrentVersion\Run"
NO_WINDOW = {"creationflags": subprocess.CREATE_NO_WINDOW} if os.name == "nt" else {}


class InstallError(Exception):
    pass


LOG_FILE = Path(os.environ.get("SWIFTLANE_SETUP_LOG") or Path(os.environ.get("TEMP", ".")) / "Swiftlane Setup.log")


def write_log(text):
    """Setup's log, so a failed install can be diagnosed afterwards."""
    try:
        with open(LOG_FILE, "a", encoding="utf-8") as f:
            f.write(f"[{time.strftime('%Y-%m-%d %H:%M:%S')}] {text}\n")
    except OSError:
        pass


# ---------------------------------------------------------------- Windows actions

def _ps(script, env=None):
    """Run PowerShell; values come in through environment variables, never pasted into the script."""
    r = subprocess.run(["powershell.exe", "-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", script],
                       capture_output=True, text=True, env={**os.environ, **(env or {})}, timeout=120, **NO_WINDOW)
    if r.returncode != 0:
        raise InstallError((r.stderr or r.stdout).strip().splitlines()[-1] if (r.stderr or r.stdout).strip() else "PowerShell failed")
    return r.stdout


def programs_using(paths):
    """Names of programs that have any of these files open (Windows Restart Manager)."""
    import ctypes
    from ctypes import wintypes
    paths = [str(p) for p in paths if Path(p).exists()]
    if not paths:
        return []

    class RM_UNIQUE_PROCESS(ctypes.Structure):
        _fields_ = [("dwProcessId", wintypes.DWORD), ("ProcessStartTime", wintypes.FILETIME)]

    class RM_PROCESS_INFO(ctypes.Structure):
        _fields_ = [("Process", RM_UNIQUE_PROCESS), ("strAppName", wintypes.WCHAR * 256),
                    ("strServiceShortName", wintypes.WCHAR * 64), ("ApplicationType", ctypes.c_int),
                    ("AppStatus", wintypes.ULONG), ("TSSessionId", wintypes.DWORD), ("bRestartable", wintypes.BOOL)]
    rm = ctypes.WinDLL("rstrtmgr")
    session, key = wintypes.DWORD(), ctypes.create_unicode_buffer(64)
    if rm.RmStartSession(ctypes.byref(session), 0, key):
        return []
    try:
        files = (wintypes.LPCWSTR * len(paths))(*paths)
        if rm.RmRegisterResources(session, len(paths), files, 0, None, 0, None):
            return []
        needed, count, reasons = wintypes.UINT(), wintypes.UINT(10), wintypes.DWORD()
        infos = (RM_PROCESS_INFO * 10)()
        rm.RmGetList(session, ctypes.byref(needed), ctypes.byref(count), infos, ctypes.byref(reasons))
        found = {infos[i].Process.dwProcessId: infos[i].strAppName for i in range(count.value)}
        return [{"name": name, "pid": pid} for pid, name in sorted(found.items())]
    finally:
        rm.RmEndSession(session)


def process_table():
    """{pid: (parent pid, exe name)} for every running process (Toolhelp snapshot)."""
    import ctypes
    from ctypes import wintypes

    class PROCESSENTRY32W(ctypes.Structure):
        _fields_ = [("dwSize", wintypes.DWORD), ("cntUsage", wintypes.DWORD), ("th32ProcessID", wintypes.DWORD),
                    ("th32DefaultHeapID", ctypes.c_size_t), ("th32ModuleID", wintypes.DWORD), ("cntThreads", wintypes.DWORD),
                    ("th32ParentProcessID", wintypes.DWORD), ("pcPriClassBase", ctypes.c_long), ("dwFlags", wintypes.DWORD),
                    ("szExeFile", wintypes.WCHAR * 260)]
    k32 = ctypes.windll.kernel32
    k32.CreateToolhelp32Snapshot.restype = wintypes.HANDLE
    snap = k32.CreateToolhelp32Snapshot(0x2, 0)  # TH32CS_SNAPPROCESS
    table = {}
    try:
        entry = PROCESSENTRY32W(dwSize=ctypes.sizeof(PROCESSENTRY32W))
        ok = k32.Process32FirstW(snap, ctypes.byref(entry))
        while ok:
            table[entry.th32ProcessID] = (entry.th32ParentProcessID, entry.szExeFile)
            ok = k32.Process32NextW(snap, ctypes.byref(entry))
    finally:
        k32.CloseHandle(snap)
    return table


def processes_to_stop(table, me, names):
    """PIDs to close: every process called one of `names`, plus everything it started (WebView2 can hold
    the folder), but never Setup itself. When you update from inside Swiftlane, Windows counts Setup
    as Swiftlane's child, so closing Swiftlane's whole tree would also close Setup."""
    names = {n.lower() for n in names}
    children = {}
    for pid, (parent, _) in table.items():
        if pid != parent:
            children.setdefault(parent, []).append(pid)

    def tree(pid):
        out, todo = set(), [pid]
        while todo:
            p = todo.pop()
            if p not in out:
                out.add(p)
                todo.extend(children.get(p, []))
        return out

    keep, p, seen = set(), me, set()
    while p in table and p not in seen:  # Setup, the chain that started it, and everything Setup started
        seen.add(p)
        keep |= tree(p) if p == me else {p}
        p = table[p][0]
    stop = set()
    for pid, (_, exe) in table.items():
        if exe.lower() in names:
            stop |= tree(pid)
    # Never stop Setup or anything Setup started; Swiftlane above it in the chain is still stopped.
    return sorted(pid for pid in stop if pid not in tree(me) and pid != me and (pid not in keep or table[pid][1].lower() in names))


def stale_setups(table, me, name="Swiftlane Setup.exe"):
    """Other copies of Setup still running (e.g. a hidden one left from a failed update; it can keep
    Swiftlane's folder busy). Never this Setup, the chain that started it, or what it started."""
    mine, p, seen = set(), me, set()
    while p in table and p not in seen:
        seen.add(p)
        mine.add(p)
        p = table[p][0]
    todo = [me]
    while todo:
        q = todo.pop()
        for pid, (parent, _) in table.items():
            if parent == q and pid not in mine and pid != parent:
                mine.add(pid)
                todo.append(pid)
    return sorted(pid for pid, (_, exe) in table.items() if exe.lower() == name.lower() and pid not in mine)


OUR_RULES = "'Swiftlane','PocketDrop'"  # firewall rules by this app's name and its old name


class WindowsSystem:
    def stop_app(self):
        names = (f"{APP}.exe", f"{OLD_APP}.exe")
        stale = stale_setups(process_table(), os.getpid())
        for pid in stale:
            subprocess.run(["taskkill", "/F", "/PID", str(pid)], capture_output=True, **NO_WINDOW)
        if stale:
            write_log(f"Closed {len(stale)} leftover Setup process(es)")
        # Swiftlane is usually closing by itself already (after starting the update); give it a moment.
        for _ in range(10):
            if not processes_to_stop(process_table(), os.getpid(), names):
                return
            time.sleep(0.3)
        pids = processes_to_stop(process_table(), os.getpid(), names)
        for pid in pids:  # one by one, never /T: Setup itself may count as part of Swiftlane's tree
            subprocess.run(["taskkill", "/F", "/PID", str(pid)], capture_output=True, **NO_WINDOW)
        write_log(f"Closed {len(pids)} Swiftlane process(es)")
        time.sleep(1.0)

    # firewall
    def firewall_rules(self):
        """[(name, program)] for our rules (new and old name)."""
        # Filtering with Where-Object (instead of -DisplayName) means a rule that doesn't exist
        # is simply not listed, rather than PowerShell ending with an error status.
        out = _ps(f"Get-NetFirewallRule | Where-Object {{ $_.DisplayName -in {OUR_RULES} }} | "
                  "ForEach-Object { $_.DisplayName + '|' + ($_ | Get-NetFirewallApplicationFilter).Program }")
        return [tuple(line.split("|", 1)) for line in out.splitlines() if "|" in line]

    def remove_firewall_rules(self):
        _ps(f"Get-NetFirewallRule | Where-Object {{ $_.DisplayName -in {OUR_RULES} }} | Remove-NetFirewallRule")

    def add_firewall_rule(self, name, program):
        _ps("New-NetFirewallRule -DisplayName $env:SL_NAME -Direction Inbound -Action Allow -Profile Private "
            "-Program $env:SL_PROG -Protocol TCP -LocalPort $env:SL_PORT | Out-Null",
            {"SL_NAME": name, "SL_PROG": program, "SL_PORT": str(PORT)})

    # networks
    def networks(self):
        """[{"alias", "name", "category"}] for connected networks."""
        out = _ps("Get-NetConnectionProfile | ForEach-Object { $_.InterfaceAlias + '|' + $_.Name + '|' + $_.NetworkCategory }")
        nets = []
        for line in out.splitlines():
            parts = line.split("|")
            if len(parts) == 3:
                nets.append({"alias": parts[0], "name": parts[1], "category": parts[2]})
        return nets

    def set_network_category(self, alias, category):
        _ps("Set-NetConnectionProfile -InterfaceAlias $env:SL_ALIAS -NetworkCategory $env:SL_CAT", {"SL_ALIAS": alias, "SL_CAT": category})

    # registry
    def read_key(self, hive, path):
        import winreg
        try:
            with winreg.OpenKey(hive, path) as k:
                values, i = {}, 0
                while True:
                    try:
                        name, value, kind = winreg.EnumValue(k, i)
                    except OSError:
                        return values
                    values[name] = (value, kind)
                    i += 1
        except FileNotFoundError:
            return None

    def write_key(self, hive, path, values):
        import winreg
        with winreg.CreateKeyEx(hive, path, 0, winreg.KEY_SET_VALUE) as k:
            for name, (value, kind) in values.items():
                winreg.SetValueEx(k, name, 0, kind, value)

    def delete_key(self, hive, path):
        import winreg
        try:
            winreg.DeleteKey(hive, path)
        except FileNotFoundError:
            pass

    def read_value(self, hive, path, name):
        import winreg
        try:
            with winreg.OpenKey(hive, path) as k:
                return winreg.QueryValueEx(k, name)
        except OSError:
            return None

    def write_value(self, hive, path, name, value):
        import winreg
        with winreg.CreateKeyEx(hive, path, 0, winreg.KEY_SET_VALUE) as k:
            if value is None:
                try:
                    winreg.DeleteValue(k, name)
                except FileNotFoundError:
                    pass
            else:
                winreg.SetValueEx(k, name, 0, value[1], value[0])

    # shortcuts
    def make_shortcut(self, lnk, target, description):
        _ps("$s = (New-Object -ComObject WScript.Shell).CreateShortcut($env:SL_LNK); $s.TargetPath = $env:SL_EXE; "
            "$s.WorkingDirectory = (Split-Path $env:SL_EXE); $s.Description = $env:SL_DESC; $s.Save()",
            {"SL_LNK": str(lnk), "SL_EXE": str(target), "SL_DESC": description})


def known_folder(csidl):
    import ctypes
    buf = ctypes.create_unicode_buffer(260)
    ctypes.windll.shell32.SHGetFolderPathW(None, csidl, None, 0, buf)
    return Path(buf.value)


def start_menu_dir():
    return known_folder(0x17) / "Programs"      # CSIDL_COMMON_PROGRAMS (all users)


def desktop_dir():
    return known_folder(0x10)                   # CSIDL_DESKTOPDIRECTORY (this user)


# ---------------------------------------------------------------- the transaction

class Transaction:
    """Do steps; remember how to undo each; undo everything if one fails."""

    def __init__(self, log):
        self.undo = []
        self.log = log

    def step(self, title, do, undo=None):
        self.log(title)
        # Register the undo first: if `do` fails halfway, whatever it already changed is undone too.
        # (Every undo below is written to be safe when its step only partly happened.)
        if undo:
            self.undo.append((title, undo))
        return do()

    def rollback(self):
        errors = []
        for title, undo in reversed(self.undo):
            try:
                write_log(f"Undoing: {title}")
                undo()
            except Exception as e:  # keep undoing the rest
                errors.append(f"{title}: {e}")
        self.undo.clear()
        return errors


def install(payload_zip, choices, system=None, progress=None, fail_at=None, hive=None):
    """Install or update Swiftlane. choices: see setup.py. Returns a summary dict.
    On failure, rolls everything back and raises InstallError."""
    import winreg
    system = system or WindowsSystem()
    hive = hive or winreg.HKEY_LOCAL_MACHINE
    progress = progress or (lambda fraction, text: None)
    steps_total = 9
    done = [0]

    def log(text):
        write_log(text)
        progress(done[0] / steps_total, text)
        done[0] += 1
        if fail_at and fail_at in text:  # used by the tests to prove rollback works
            raise InstallError(f"Simulated failure at: {text}")

    dest = Path(choices["install_dir"])
    exe = dest / f"{APP}.exe"
    stamp = time.strftime("%Y%m%d-%H%M%S") + "-" + uuid.uuid4().hex[:6]
    tx = Transaction(log)
    backup = dest.with_name(dest.name + f".backup-{stamp}")
    try:
        tx.step("Closing Swiftlane", system.stop_app)

        # 1. program files: move the old install aside (undo = move it back)
        had_old = dest.exists()

        def move_old():
            if not had_old:
                return
            # Programs that just closed can hold the folder for a few seconds: keep trying.
            for attempt in range(20):
                try:
                    dest.rename(backup)
                    return
                except PermissionError as e:
                    last = e
                    time.sleep(0.75)
            holders = programs_using([dest / f"{APP}.exe", *list(dest.glob("_internal/*.dll"))[:40]])
            raise InstallError("The old Swiftlane folder is in use" +
                               (f" by: {', '.join(h['name'] for h in holders)}." if holders else " by another program.") +
                               f" Close it and run Setup again. ({last})")

        def restore_old():
            if had_old:
                if not backup.exists():
                    return  # the move never happened: the old install is still in place, untouched
                shutil.rmtree(dest, ignore_errors=True)  # whatever the new version left there
                backup.rename(dest)
            else:
                shutil.rmtree(dest, ignore_errors=True)  # fresh install: remove the half-copied files
        tx.step("Making room for the new version", move_old, restore_old)

        def extract():
            dest.mkdir(parents=True, exist_ok=True)
            with zipfile.ZipFile(payload_zip) as z:
                names = z.namelist()
                for i, name in enumerate(names):
                    z.extract(name, dest)
                    if i % 50 == 0:
                        progress((done[0] - 1 + i / len(names)) / steps_total, "Copying files")
            if not exe.exists():
                raise InstallError("The installer package is incomplete (Swiftlane.exe missing).")
        tx.step("Copying files", extract)

        # 2. choices the app picks up on its next start
        def write_choices():
            data = {k: v for k, v in choices.items() if k in (
                "save_dir", "sort_files", "download_dir", "calc_angle", "language", "notify", "features", "theme")}
            data["stamp"] = stamp
            if choices.get("open_page"):
                data["open_page"] = choices["open_page"]
            (dest / "setup-choices.json").write_text(json.dumps(data, indent=2), "utf-8")
            Path(choices["save_dir"]).mkdir(parents=True, exist_ok=True)
        tx.step("Saving your choices", write_choices)

        # 3. firewall (only this program, only this port, only private networks)
        old_rules = system.firewall_rules()

        def firewall():
            system.remove_firewall_rules()
            if choices.get("firewall", True) and choices.get("features", {}).get("transfer", True):
                system.add_firewall_rule(APP, str(exe))

        def firewall_undo():
            system.remove_firewall_rules()
            for name, program in old_rules:
                system.add_firewall_rule(name, program)
        tx.step("Setting up the firewall", firewall, firewall_undo)

        # 4. network category
        changed = []

        def network():
            for alias in choices.get("private_networks", []):
                before = next((n["category"] for n in system.networks() if n["alias"] == alias), None)
                if before and before != "Private":
                    system.set_network_category(alias, "Private")
                    changed.append((alias, before))

        def network_undo():
            for alias, before in changed:
                system.set_network_category(alias, before)
        tx.step("Setting up your network", network, network_undo)

        # 5. shortcuts (remember any existing ones byte for byte)
        links = {start_menu_dir() / f"{APP}.lnk": choices.get("start_menu", True),
                 desktop_dir() / f"{APP}.lnk": choices.get("desktop", True)}
        saved_links = {lnk: (lnk.read_bytes() if lnk.exists() else None) for lnk in links}

        def shortcuts():
            for lnk, wanted in links.items():
                if wanted:
                    lnk.parent.mkdir(parents=True, exist_ok=True)
                    system.make_shortcut(lnk, exe, "Send files between your phone and this PC, plus everyday tools")
                elif lnk.exists():
                    lnk.unlink()

        def shortcuts_undo():
            for lnk, data in saved_links.items():
                if data is None:
                    lnk.unlink(missing_ok=True)
                else:
                    lnk.write_bytes(data)
        tx.step("Creating shortcuts", shortcuts, shortcuts_undo)

        # 6. start with Windows (per user)
        old_run = system.read_value(winreg.HKEY_CURRENT_USER, RUN_KEY, APP)

        def autostart():
            if choices.get("autostart"):
                system.write_value(winreg.HKEY_CURRENT_USER, RUN_KEY, APP, (f'"{exe}" --minimized', winreg.REG_SZ))
            else:
                system.write_value(winreg.HKEY_CURRENT_USER, RUN_KEY, APP, None)
        tx.step("Start with Windows", autostart, lambda: system.write_value(winreg.HKEY_CURRENT_USER, RUN_KEY, APP, old_run))

        # 7. Settings > Apps entry
        old_key = system.read_key(hive, UNINSTALL_KEY)

        def register():
            size_kb = sum(f.stat().st_size for f in dest.rglob("*") if f.is_file()) // 1024
            system.delete_key(hive, UNINSTALL_KEY)
            system.write_key(hive, UNINSTALL_KEY, {
                "DisplayName": (APP, winreg.REG_SZ), "DisplayVersion": (choices["version"], winreg.REG_SZ),
                "Publisher": (APP, winreg.REG_SZ), "DisplayIcon": (str(exe), winreg.REG_SZ),
                "InstallLocation": (str(dest), winreg.REG_SZ),
                "UninstallString": (f'"{exe}" --uninstall', winreg.REG_SZ),
                "EstimatedSize": (size_kb, winreg.REG_DWORD), "NoModify": (1, winreg.REG_DWORD), "NoRepair": (1, winreg.REG_DWORD)})

        def register_undo():
            system.delete_key(hive, UNINSTALL_KEY)
            if old_key:
                system.write_key(hive, UNINSTALL_KEY, old_key)
        tx.step("Registering in Settings > Apps", register, register_undo)
    except Exception as e:
        import traceback
        write_log("FAILED: " + traceback.format_exc())
        errors = tx.rollback()
        write_log("Rollback finished" + (" with problems: " + "; ".join(errors) if errors else ", everything restored"))
        detail = str(e) if isinstance(e, InstallError) else f"{e.__class__.__name__}: {e}"
        raise InstallError(detail + ("  (Some things couldn't be undone: " + "; ".join(errors) + ")" if errors else "")) from e

    # Everything worked: commit (none of this can break the new install).
    progress(0.95, "Cleaning up")
    shutil.rmtree(backup, ignore_errors=True)
    _cleanup_old_name(system, hive)
    progress(1.0, "Done")
    return {"exe": str(exe), "stamp": stamp}


def _cleanup_old_name(system, hive):
    """Leftovers of the app's earlier name (PocketDrop)."""
    import winreg
    old = Path(os.environ.get("ProgramFiles", r"C:\Program Files")) / OLD_APP
    shutil.rmtree(old, ignore_errors=True)
    (start_menu_dir() / f"{OLD_APP}.lnk").unlink(missing_ok=True)
    system.delete_key(hive, rf"Software\Microsoft\Windows\CurrentVersion\Uninstall\{OLD_APP}")
    system.write_value(winreg.HKEY_CURRENT_USER, RUN_KEY, OLD_APP, None)


def installed_version(hive=None):
    """(version, install_dir) of an existing install, or None."""
    import winreg
    hive = hive or winreg.HKEY_LOCAL_MACHINE
    values = WindowsSystem().read_key(hive, UNINSTALL_KEY)
    if not values:
        return None
    return values.get("DisplayVersion", ("?",))[0], values.get("InstallLocation", ("",))[0]
