"""AirPlay to this PC: the iPhone's Screen Mirroring / AirPlay list shows this PC, and the picture
opens in its own window (Alt+Enter switches full screen on and off).

The receiver is UxPlay (open source, GPL-3.0, https://github.com/FDH2/UxPlay), built from its source
on Swiftlane's GitHub (.github/workflows/airplay-receiver.yml) and run as a separate program.
- It's a one-time download, refused unless it matches the SHA-256 below (part of the signed app).
- It's installed as administrator into its own Program Files folder, so nothing else on the PC can
  swap it for something that rides on its firewall rule (only uxplay.exe, only Private networks).
- It only runs while you have AirPlay switched on and Swiftlane is open.
- A phone has to type a 4-digit PIN shown by Swiftlane the first time; after that it's remembered
  (until you press Forget).
"""
import hashlib
import os
import re
import secrets
import socket
import subprocess
import tempfile
import threading
import time
import urllib.request
from pathlib import Path

from .common import Cancelled

RELEASE = "airplay-receiver-1"
URL = f"https://github.com/Lewy366/swiftlane/releases/download/{RELEASE}/airplay-receiver.zip"
# from GitHub run 44834256264 (UxPlay 48f5ce4); an empty value would mean "not available yet"
SHA256 = "f6a983285750a6f17c3b55578ba269800bd344231dd360ed8f395be934e16696"
SIZE = 77378682
INSTALL_DIR = Path(os.environ.get("ProgramFiles", r"C:\Program Files")) / "Swiftlane AirPlay"
RULE = "Swiftlane AirPlay"
CHUNK = 256 * 1024


def exe():
    return INSTALL_DIR / "uxplay.exe"


def installed():
    return exe().is_file()


def available():
    return bool(SHA256) and SIZE > 0


def download(progress=None, cancelled=None):
    """Fetch the receiver zip into a fresh temp folder and check it. Returns its path."""
    if not available():
        raise RuntimeError("The AirPlay receiver isn't ready for download yet.")
    folder = Path(tempfile.mkdtemp(prefix="Swiftlane-airplay-"))
    path = folder / "airplay-receiver.zip"
    h, done = hashlib.sha256(), 0
    req = urllib.request.Request(URL, headers={"User-Agent": "Swiftlane"})
    try:
        with urllib.request.urlopen(req, timeout=60) as r, open(path, "xb") as f:
            while True:
                if cancelled and cancelled():
                    raise Cancelled()
                chunk = r.read(CHUNK)
                if not chunk:
                    break
                done += len(chunk)
                if done > SIZE:
                    raise RuntimeError("The download is bigger than expected, so it was rejected.")
                h.update(chunk)
                f.write(chunk)
                if progress:
                    progress(done / SIZE)
        if done != SIZE or h.hexdigest() != SHA256:
            raise RuntimeError("The download doesn't match the AirPlay receiver Swiftlane expects, so it was rejected.")
    except OSError:
        path.unlink(missing_ok=True)
        raise RuntimeError("The download was interrupted. Try again.")
    except BaseException:
        path.unlink(missing_ok=True)
        raise
    return path


def network_is_private():
    """True/False for the network this PC is on (None if Windows wouldn't say).
    The firewall rule only covers Private (home) networks, so on Public ones phones can't see the PC."""
    try:
        out = subprocess.run(["powershell.exe", "-NoProfile", "-Command",
                              "(Get-NetConnectionProfile | Where-Object IPv4Connectivity -ne 'Disconnected').NetworkCategory"],
                             capture_output=True, text=True, timeout=15, creationflags=subprocess.CREATE_NO_WINDOW)
    except (OSError, subprocess.SubprocessError):
        return None
    cats = [c.strip() for c in out.stdout.splitlines() if c.strip()]
    if not cats:
        return None
    return all(c in ("Private", "DomainAuthenticated") for c in cats)


_job = None


def _end_with_swiftlane(proc):
    """Put uxplay in a Windows job that ends it when Swiftlane ends, even after a crash, so it can
    never keep listening on its own."""
    global _job
    import ctypes
    from ctypes import wintypes

    class Basic(ctypes.Structure):
        _fields_ = [("PerProcessUserTimeLimit", ctypes.c_int64), ("PerJobUserTimeLimit", ctypes.c_int64),
                    ("LimitFlags", wintypes.DWORD), ("MinimumWorkingSetSize", ctypes.c_size_t),
                    ("MaximumWorkingSetSize", ctypes.c_size_t), ("ActiveProcessLimit", wintypes.DWORD),
                    ("Affinity", ctypes.c_size_t), ("PriorityClass", wintypes.DWORD), ("SchedulingClass", wintypes.DWORD)]

    class Io(ctypes.Structure):
        _fields_ = [(n, ctypes.c_uint64) for n in ("Read", "Write", "Other", "ReadBytes", "WriteBytes", "OtherBytes")]

    class Extended(ctypes.Structure):
        _fields_ = [("Basic", Basic), ("Io", Io), ("ProcessMemoryLimit", ctypes.c_size_t),
                    ("JobMemoryLimit", ctypes.c_size_t), ("PeakProcessMemoryUsed", ctypes.c_size_t),
                    ("PeakJobMemoryUsed", ctypes.c_size_t)]

    k = ctypes.windll.kernel32
    k.CreateJobObjectW.restype = wintypes.HANDLE
    if _job is None:
        job = k.CreateJobObjectW(None, None)
        info = Extended()
        info.Basic.LimitFlags = 0x2000  # JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
        if not job or not k.SetInformationJobObject(wintypes.HANDLE(job), 9, ctypes.byref(info), ctypes.sizeof(info)):
            return False
        _job = job  # kept open for as long as Swiftlane runs; Windows closes it when Swiftlane ends
    return bool(k.AssignProcessToJobObject(wintypes.HANDLE(_job), wintypes.HANDLE(int(proc._handle))))


def pc_name():
    return (os.environ.get("COMPUTERNAME") or socket.gethostname() or "PC")[:40]


class Receiver:
    """Runs uxplay.exe in the background and turns its messages into events for the window."""

    def __init__(self, data_dir, on_event):
        self.dir = Path(data_dir)
        self.on_event = on_event          # on_event(kind, info)
        self.proc = None
        self.pin = None
        self.status = "off"               # off | ready | showing | error
        self.client = None                # name of the phone that's showing right now
        self.error = None
        self.log = []                     # last lines, for when something goes wrong
        self._lock = threading.Lock()

    @property
    def register(self):
        return self.dir / "phones.txt"

    def phones(self):
        """Phones that typed the PIN once and may connect again without it."""
        try:
            lines = self.register.read_text("utf-8", "replace").splitlines()
        except OSError:
            return []
        out = []
        for line in lines:
            # UxPlay writes one line per phone: public key, date added, device id, name
            parts = line.split(",", 3)
            if len(parts) == 4:
                out.append({"name": parts[3].strip()[:60] or "Phone", "added": parts[1].strip()[:20],
                            "id": parts[2].strip().upper()[:17]})
        return out

    def forget_phones(self):
        running = self.running()
        if running:
            self.stop()
        self.register.unlink(missing_ok=True)
        if running:
            self.start()

    def running(self):
        return self.proc is not None and self.proc.poll() is None

    def start(self):
        with self._lock:
            if self.running():
                return
            if not installed():
                raise RuntimeError("The AirPlay receiver isn't installed yet.")
            self.dir.mkdir(parents=True, exist_ok=True)
            self.pin = f"{secrets.randbelow(9000) + 1000}"
            plugins = INSTALL_DIR / "lib" / "gstreamer-1.0"
            env = {k: v for k, v in os.environ.items() if not k.startswith("GST_")}
            system32 = Path(os.environ.get("SystemRoot", r"C:\Windows")) / "System32"
            env.update({
                # only the receiver's own parts, never anything from a user-writable folder
                "PATH": f"{INSTALL_DIR};{system32}",
                "GST_PLUGIN_SYSTEM_PATH_1_0": str(plugins),
                "GST_PLUGIN_PATH_1_0": "",
                "GST_PLUGIN_SCANNER_1_0": str(INSTALL_DIR / "gst-plugin-scanner.exe"),
                "GST_REGISTRY_1_0": str(self.dir / "plugins.bin"),
                "HOME": str(self.dir),
            })
            args = [str(exe()), "-n", f"{pc_name()} (Swiftlane)", "-nh",
                    "-pin", self.pin, "-reg", str(self.register), "-key", str(self.dir / "key.pem"),
                    "-vs", "d3d11videosink", "-nohold"]
            self.log.clear()
            self.error = None
            self.client = None
            self.proc = subprocess.Popen(args, cwd=str(self.dir), env=env, stdin=subprocess.DEVNULL,
                                         stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                                         creationflags=subprocess.CREATE_NO_WINDOW)
            if not _end_with_swiftlane(self.proc):
                self.proc.kill()
                self.proc = None
                raise RuntimeError("Windows wouldn't tie the AirPlay receiver to Swiftlane, so it wasn't started.")
            self.status = "ready"
            threading.Thread(target=self._read, args=(self.proc,), daemon=True).start()
        self.on_event("status", None)

    def stop(self):
        with self._lock:
            p, self.proc = self.proc, None
            self.status, self.client, self.pin = "off", None, None
        if p and p.poll() is None:
            p.terminate()
            try:
                p.wait(5)
            except subprocess.TimeoutExpired:
                p.kill()
        self.on_event("status", None)

    def _read(self, proc):
        for raw in proc.stdout:
            line = raw.decode("utf-8", "replace").rstrip()
            if not line:
                continue
            self.log.append(line)
            del self.log[:-60]
            self._handle(line)
        code = proc.wait()
        if proc is self.proc:  # it stopped by itself
            with self._lock:
                self.proc, self.status, self.client = None, "error", None
                tail = [l for l in self.log if "ERROR" in l][-1:] or self.log[-1:]
                self.error = f"The AirPlay receiver stopped (code {code})." + (f" {tail[0][:200]}" if tail else "")
            self.on_event("status", None)

    def _handle(self, line):
        if "CLIENT MUST NOW ENTER PIN" in line:  # a new phone is showing the PIN prompt right now
            self.on_event("pin", {"pin": self.pin})
            return
        m = re.search(r"connection request from (.+?) \((.*?)\) with deviceID = (\S+)", line)
        if m:
            self.client = m.group(1).strip()[:60]
            self.status = "showing"
            self.on_event("connect", {"name": self.client})
            return
        if "registered new client" in line:
            self.on_event("paired", {"name": self.client})
            return
        if "on_video_stop" in line or "lost connection with client" in line or "Connection closed" in line:
            if self.status == "showing":
                self.status = "ready"
                self.client = None
                self.on_event("status", None)
