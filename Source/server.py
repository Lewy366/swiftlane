"""Swiftlane core: the local web server your iPhone talks to.

Receives uploads from the iPhone (Shortcut or web app) and serves files you
queue on the PC for the iPhone to download. Standard library only, except the
optional `qrcode` package for the pairing QR code.
"""
import hashlib
import hmac
import ipaddress
import itertools
import json
import mimetypes
import os
import re
import secrets
import shutil
import socket
import ssl
import subprocess
import sys
import threading
import time
from collections import deque
from datetime import datetime
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import parse_qs, quote, unquote, urlsplit

APP = "Swiftlane"
VERSION = "1.1.1"
OLD_APP = "PocketDrop"  # the app's name before 2.2; its settings and files are carried over
APPDATA = Path(os.environ.get("APPDATA") or Path.home())
CONFIG_DIR = APPDATA / APP
CONFIG_PATH = CONFIG_DIR / "config.json"
LOG_PATH = CONFIG_DIR / "swiftlane.log"
RESOURCES = Path(getattr(sys, "_MEIPASS", Path(__file__).resolve().parent))
WEB_DIR = RESOURCES / "web"


def documents_folder():
    """The real Documents folder, wherever Windows keeps it (it can be relocated)."""
    if sys.platform == "win32":
        import ctypes
        buf = ctypes.create_unicode_buffer(260)
        if ctypes.windll.shell32.SHGetFolderPathW(None, 5, None, 0, buf) == 0:  # CSIDL_PERSONAL
            return Path(buf.value)
    return Path.home() / "Documents"


DEFAULTS = {
    "port": 8765,
    "save_dir": str(documents_folder() / APP),
    "max_file_gb": 20,
    "notify": True,
    # Which parts of the app are switched on (Settings > Features).
    "features": {"transfer": True, "converter": True, "images": True,
                 "downloader": True, "currency": True, "calculator": True,
                 "speedtest": True, "adblock": True, "share": True,
                 "pdf": True, "trimmer": True, "vault": True, "diskmap": True, "netscan": True, "exposure": True,
                 "remote": False},       # PC remote: off until you switch it on (Security-first)
    "remote_power": False,      # PC remote may also sleep / shut down the PC
    "https_only": False,        # refuse unencrypted phone connections (turn on once phones are set up)
    "sort_files": True,        # received files go into Photos/Videos/... subfolders
    "download_dir": "",        # where the video downloader suggests saving ("" = <save_dir>\Videos or \Audio)
    "calc_angle": "deg",       # calculator's starting angle unit
    "language": "en",          # "en", "de", "pl" or "it"
    "theme": {"accent": "cyan", "custom": "#14b8e0", "background": False, "blur": 14, "dim": 30, "blur_on": True, "mode": "dark", "bg_file": ""},
    # Share links: what a new share starts with (each share can be changed afterwards).
    "share": {"hours": 24, "mode": "open", "count": None, "people": 1, "keep_awake": True},
}
THEME_KEYS = ("accent", "custom", "background", "blur", "dim", "blur_on", "mode")
# Default save folders of earlier versions (1.x, then 2.0-2.1). If the settings still
# point at one of these, its files are moved into DEFAULTS["save_dir"], sorted by type.
LEGACY_SAVE_DIRS = [Path.home() / "Downloads" / "iPhone", documents_folder() / OLD_APP]

# Received files are sorted into these subfolders of the save folder by extension.
CATEGORIES = {
    "Photos": {"jpg", "jpeg", "heic", "heif", "png", "gif", "webp", "bmp", "tif", "tiff",
               "dng", "raw", "cr2", "nef", "arw", "svg"},
    "Videos": {"mov", "mp4", "m4v", "avi", "mkv", "webm", "hevc", "3gp", "wmv"},
    "Audio": {"mp3", "m4a", "wav", "aac", "flac", "ogg", "aiff", "caf", "wma", "opus"},
    "Documents": {"pdf", "doc", "docx", "xls", "xlsx", "ppt", "pptx", "txt", "rtf", "csv", "md",
                  "pages", "numbers", "key", "odt", "ods", "odp", "epub", "json"},
    "Archives": {"zip", "rar", "7z", "tar", "gz", "bz2", "xz"},
}
OTHER_CATEGORY = "Other"

CHUNK = 1024 * 1024
DISK_HEADROOM = 512 * 1024 * 1024
FAIL_LIMIT = 10
LOCKOUT_SECONDS = 15 * 60
DOWNLOAD_LINK_SECONDS = 10 * 60
CODE_ALPHABET = "ABCDEFGHJKMNPQRSTUVWXYZ23456789"  # no 0/O, 1/I/L lookalikes
CODE_LENGTH = 16  # ~79 bits, plus lockout after FAIL_LIMIT wrong tries
TAILSCALE_NET = ipaddress.ip_network("100.64.0.0/10")
RESERVED_NAMES = {"CON", "PRN", "AUX", "NUL",
                  *(f"COM{i}" for i in range(1, 10)), *(f"LPT{i}" for i in range(1, 10))}
TYPE_EXTENSIONS = {
    "image/jpeg": ".jpg", "image/heic": ".heic", "image/heif": ".heif",
    "image/png": ".png", "video/quicktime": ".mov", "video/mp4": ".mp4",
    "application/pdf": ".pdf",
}
STATIC_FILES = {
    "/": ("phone.html", "text/html; charset=utf-8"),
    "/manifest.webmanifest": ("manifest.webmanifest", "application/manifest+json"),
    "/icon-180.png": ("icon-180.png", "image/png"),
    "/icon-512.png": ("icon-512.png", "image/png"),
}
PAGE_CSP = ("default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; "
            "connect-src 'self'; img-src 'self' data:; manifest-src 'self'; "
            "form-action 'none'; frame-ancestors 'none'")


# ---------------------------------------------------------------- config

def new_code():
    return "".join(secrets.choice(CODE_ALPHABET) for _ in range(CODE_LENGTH))


def normalize_code(code):
    return re.sub(r"[\s-]", "", code).upper()


def pretty_code(code):
    return "-".join(code[i:i + 4] for i in range(0, len(code), 4))


def load_config(folder=None, port=None, regenerate=False):
    adopt_old_settings()
    CONFIG_DIR.mkdir(parents=True, exist_ok=True)
    cfg = dict(DEFAULTS)
    if CONFIG_PATH.exists():
        cfg.update(json.loads(CONFIG_PATH.read_text("utf-8")))
    cfg["features"] = {**DEFAULTS["features"], **cfg.get("features", {})}  # new tools default to on
    cfg["theme"] = {**DEFAULTS["theme"], **cfg.get("theme", {})}
    cfg["share"] = {**DEFAULTS["share"], **cfg.get("share", {})}
    apply_installer_choices(cfg)
    if folder:
        cfg["save_dir"] = str(Path(folder).expanduser().resolve())
    elif Path(cfg["save_dir"]) in LEGACY_SAVE_DIRS:
        old_dir = Path(cfg["save_dir"])
        cfg["save_dir"] = DEFAULTS["save_dir"]
        move_legacy_files(old_dir, Path(cfg["save_dir"]))
    if port:
        cfg["port"] = port
    if regenerate or not cfg.get("code"):
        cfg["code"] = new_code()
    save_config(cfg)
    return cfg


def installer_choices_file():
    """Choices made in Swiftlane Setup, saved next to the installed Swiftlane.exe."""
    base = Path(sys.executable).parent if getattr(sys, "frozen", False) else Path(__file__).resolve().parent
    return base / "setup-choices.json"


def apply_installer_choices(cfg):
    """Take over what was chosen in Swiftlane Setup, once per install (each install has a new stamp).
    The pairing code is never touched here."""
    path = installer_choices_file()
    try:
        choices = json.loads(path.read_text("utf-8"))
    except (OSError, ValueError):
        return
    stamp = choices.get("stamp")
    if not stamp or cfg.get("setup_applied") == stamp:
        return
    for key in ("save_dir", "sort_files", "download_dir", "calc_angle", "language", "notify"):
        if key in choices:
            cfg[key] = choices[key]
    if isinstance(choices.get("features"), dict):
        cfg["features"].update({k: bool(v) for k, v in choices["features"].items() if k in cfg["features"]})
    if isinstance(choices.get("theme"), dict):
        cfg["theme"].update({k: v for k, v in choices["theme"].items() if k in THEME_KEYS and k != "background"})
    if choices.get("open_page"):
        cfg["_open_page"] = choices["open_page"]  # used once, right after setup ("pair your phone now")
    cfg["setup_applied"] = stamp
    log(f"Applied choices from Swiftlane Setup ({stamp})")


def adopt_old_settings():
    """Before 2.2 the app was called PocketDrop: take over its settings (and pairing code)."""
    old_dir = APPDATA / OLD_APP
    if CONFIG_DIR.exists() or not (old_dir / "config.json").exists():
        return
    try:
        old_dir.rename(CONFIG_DIR)
    except OSError:  # e.g. a file in it is open; copy what matters instead
        CONFIG_DIR.mkdir(parents=True, exist_ok=True)
        shutil.copy2(old_dir / "config.json", CONFIG_PATH)
    old_log = CONFIG_DIR / "pocketdrop.log"
    if old_log.exists() and not LOG_PATH.exists():
        old_log.rename(LOG_PATH)
    log(f"Settings carried over from {old_dir}")


def move_legacy_files(old_dir, new_dir):
    """One-time move of files from an earlier default save folder, sorted by type."""
    moved = 0
    if old_dir.is_dir():
        for f in sorted(old_dir.rglob("*")):
            if f.is_file() and not f.name.endswith(".part"):
                place_file(f, new_dir / category_for(f.name), f.name)
                moved += 1
        # Remove the old folders, but only the ones that are now empty.
        for d in sorted((p for p in old_dir.rglob("*") if p.is_dir()), reverse=True):
            try:
                d.rmdir()
            except OSError:
                pass
        try:
            old_dir.rmdir()
        except OSError:
            pass
    log(f"Save folder moved from {old_dir} to {new_dir} ({moved} file(s) sorted into it)")


def save_config(cfg):
    CONFIG_DIR.mkdir(parents=True, exist_ok=True)
    CONFIG_PATH.write_text(json.dumps({k: v for k, v in cfg.items() if not k.startswith("_")}, indent=2), "utf-8")


def log(msg):
    line = f"[{datetime.now():%Y-%m-%d %H:%M:%S}] {msg}"
    if sys.stdout:
        print(line, flush=True)
    try:
        if LOG_PATH.exists() and LOG_PATH.stat().st_size > 1024 * 1024:
            LOG_PATH.replace(LOG_PATH.with_suffix(".old.log"))
        with open(LOG_PATH, "a", encoding="utf-8") as f:
            f.write(line + "\n")
    except OSError:
        pass


CREATED_LIST = CONFIG_DIR / "created-files.txt"
_created_lock = threading.Lock()


def record_created(paths):
    """Remember files Swiftlane made, so the uninstaller can offer to remove them."""
    try:
        with _created_lock, open(CREATED_LIST, "a", encoding="utf-8") as f:
            for p in paths:
                f.write(str(p) + "\n")
    except OSError:
        pass


def created_files():
    """Files Swiftlane made that still exist (no duplicates)."""
    try:
        lines = CREATED_LIST.read_text("utf-8").splitlines()
    except OSError:
        return []
    return [p for p in dict.fromkeys(lines) if p and os.path.isfile(p)]


def human(n):
    for unit in ("B", "KB", "MB", "GB"):
        if n < 1024 or unit == "GB":
            return f"{n:.0f} {unit}" if unit == "B" else f"{n:.1f} {unit}"
        n /= 1024


# ---------------------------------------------------------------- safety helpers

def allowed_client(ip):
    """Only your LAN, this PC, or your Tailscale tailnet may connect."""
    addr = ipaddress.ip_address(ip)
    if addr.version == 6 and addr.ipv4_mapped:
        addr = addr.ipv4_mapped
    return addr.is_loopback or addr.is_private or addr in TAILSCALE_NET


class Lockout:
    """Blocks an address for a while after too many wrong pairing codes."""

    def __init__(self):
        self.lock = threading.Lock()
        self.fails = {}

    def locked(self, ip):
        with self.lock:
            return self.fails.get(ip, (0, 0))[1] > time.time()

    def fail(self, ip):
        with self.lock:
            count, until = self.fails.get(ip, (0, 0))
            if until and until <= time.time():
                count = 0
            count += 1
            self.fails[ip] = (count, time.time() + LOCKOUT_SECONDS if count >= FAIL_LIMIT else 0)
            return count >= FAIL_LIMIT

    def clear(self, ip):
        with self.lock:
            self.fails.pop(ip, None)


def safe_name(raw):
    """Turn a client-supplied name into a plain Windows file name (no paths)."""
    name = re.split(r"[\\/]", raw)[-1]
    name = re.sub(r'[<>:"|?*\x00-\x1f]', "_", name).strip().rstrip(". ").lstrip(".")
    if not name:
        name = datetime.now().strftime("Phone %Y-%m-%d %H.%M.%S")
    stem, ext = os.path.splitext(name)
    if stem.split(".")[0].upper() in RESERVED_NAMES:
        stem = "_" + stem
    return stem[:150] + ext[:16]


def category_for(name):
    ext = os.path.splitext(name)[1].lower().lstrip(".")
    return next((folder for folder, exts in CATEGORIES.items() if ext in exts), OTHER_CATEGORY)


def place_file(src, folder, name):
    """Move src into folder as name, adding (1), (2)... rather than overwriting anything."""
    folder.mkdir(parents=True, exist_ok=True)
    stem, ext = os.path.splitext(name)
    for i in itertools.count():
        candidate = folder / (name if i == 0 else f"{stem} ({i}){ext}")
        if candidate.exists():
            continue
        try:
            os.rename(src, candidate)  # on Windows this fails instead of overwriting
        except FileExistsError:
            continue
        except OSError:  # different drive
            shutil.move(str(src), str(candidate))
        return candidate


def sniff_extension(path, content_type):
    """Best-guess extension for a file that arrived without one."""
    with open(path, "rb") as f:
        head = f.read(16)
    brand = head[8:12]
    if head.startswith(b"\xff\xd8\xff"):
        return ".jpg"
    if head.startswith(b"\x89PNG"):
        return ".png"
    if head.startswith(b"%PDF"):
        return ".pdf"
    if head[4:8] == b"ftyp":
        if brand in (b"heic", b"heix", b"mif1", b"msf1"):
            return ".heic"
        if brand == b"qt  ":
            return ".mov"
        return ".mp4"
    mime = (content_type or "").split(";")[0].strip().lower()
    return TYPE_EXTENSIONS.get(mime) or mimetypes.guess_extension(mime) or ""


# ---------------------------------------------------------------- notifications

TOAST_SCRIPT = r"""
[Windows.UI.Notifications.ToastNotificationManager, Windows.UI.Notifications, ContentType = WindowsRuntime] | Out-Null
$xml = [Windows.UI.Notifications.ToastNotificationManager]::GetTemplateContent([Windows.UI.Notifications.ToastTemplateType]::ToastText02)
$t = $xml.GetElementsByTagName('text')
$t.Item(0).AppendChild($xml.CreateTextNode($env:PD_TITLE)) | Out-Null
$t.Item(1).AppendChild($xml.CreateTextNode($env:PD_TEXT)) | Out-Null
$app = '{1AC14E77-02E7-4E5D-B744-2EB1AE5198B7}\WindowsPowerShell\v1.0\powershell.exe'
[Windows.UI.Notifications.ToastNotificationManager]::CreateToastNotifier($app).Show([Windows.UI.Notifications.ToastNotification]::new($xml))
"""


class Notifier:
    """Groups files that arrive close together into one Windows notification."""

    def __init__(self, cfg):
        self.cfg = cfg
        self.lock = threading.Lock()
        self.names = []
        self.timer = None

    def add(self, name):
        if not self.cfg.get("notify") or sys.platform != "win32":
            return
        with self.lock:
            self.names.append(name)
            if self.timer:
                self.timer.cancel()
            self.timer = threading.Timer(1.5, self.flush)
            self.timer.daemon = True
            self.timer.start()

    def flush(self):
        with self.lock:
            names, self.names = self.names, []
        if not names:
            return
        title = "File received" if len(names) == 1 else f"{len(names)} files received"
        self.show(title, ", ".join(names[:3]) + (" ..." if len(names) > 3 else ""))

    def show_soon(self, title, text):
        """Show a notification without blocking the caller."""
        if self.cfg.get("notify") and sys.platform == "win32":
            threading.Thread(target=self.show, args=(title, text), daemon=True).start()

    def show(self, title, text):
        # Text goes through environment variables, never into the script text.
        env = dict(os.environ, PD_TITLE=f"{APP}: {title}", PD_TEXT=text[:200])
        try:
            subprocess.run(
                ["powershell.exe", "-NoProfile", "-NonInteractive", "-Command", TOAST_SCRIPT],
                env=env, creationflags=subprocess.CREATE_NO_WINDOW, timeout=20,
                stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        except (OSError, subprocess.SubprocessError):
            pass


# ---------------------------------------------------------------- shared state

class Outbox:
    """Files queued on the PC for the iPhone to download. Files are not copied."""

    def __init__(self):
        self.lock = threading.Lock()
        self.items = {}
        self.ids = itertools.count(1)

    def add(self, paths):
        added = []
        with self.lock:
            known = {item["path"] for item in self.items.values()}
            for p in paths:
                path = Path(p)
                if not path.is_file() or str(path) in known:
                    continue
                item_id = str(next(self.ids))
                self.items[item_id] = {"id": item_id, "path": str(path), "name": path.name,
                                       "size": path.stat().st_size, "added": time.time(),
                                       "downloads": 0}
                added.append(item_id)
        return added

    def remove(self, item_id):
        with self.lock:
            self.items.pop(str(item_id), None)

    def clear(self):
        with self.lock:
            self.items.clear()

    def get(self, item_id):
        with self.lock:
            return dict(self.items[item_id]) if item_id in self.items else None

    def mark_downloaded(self, item_id):
        with self.lock:
            if item_id in self.items:
                self.items[item_id]["downloads"] += 1

    def list(self):
        with self.lock:
            items = [dict(i) for i in self.items.values()]
        return sorted(items, key=lambda i: i["added"], reverse=True)


class State:
    """Everything the server and the desktop window share."""

    def __init__(self, cfg):
        self.cfg = cfg
        self.code = normalize_code(cfg["code"])
        self.save_dir = Path(cfg["save_dir"])
        self.save_dir.mkdir(parents=True, exist_ok=True)
        self.lockout = Lockout()
        self.notifier = Notifier(cfg)
        self.outbox = Outbox()
        self.received = deque(maxlen=200)
        self.received_ids = itertools.count(1)
        self.place_lock = threading.Lock()
        self.link_secret = secrets.token_bytes(32)  # download links die with the app
        self.listeners = []
        self.remote = None                   # set by the app: (action, ip) -> None or a problem to show
        self.ca = None                       # tls.Authority once encrypted phone transfer is ready
        self.remote_times = deque(maxlen=10)  # at most 10 remote presses per 2 seconds
        for leftover in self.save_dir.glob(".*.part"):
            leftover.unlink(missing_ok=True)

    @property
    def max_bytes(self):
        return int(self.cfg["max_file_gb"] * 1024 ** 3)

    def emit(self, event, data=None):
        for listener in list(self.listeners):
            try:
                listener(event, data)
            except Exception:
                pass

    def set_save_dir(self, folder):
        self.save_dir = Path(folder)
        self.save_dir.mkdir(parents=True, exist_ok=True)
        self.cfg["save_dir"] = str(self.save_dir)
        save_config(self.cfg)

    def regenerate_code(self):
        self.cfg["code"] = new_code()
        self.code = self.cfg["code"]
        save_config(self.cfg)

    def add_received(self, path, size, ip):
        entry = {"id": str(next(self.received_ids)), "name": path.name, "path": str(path),
                 "folder": path.parent.name, "size": size, "time": time.time(), "from": ip}
        record_created([path])
        self.received.appendleft(entry)
        return entry

    def find_received(self, entry_id):
        return next((e for e in self.received if e["id"] == str(entry_id)), None)

    def sign(self, item_id, expires):
        msg = f"{item_id}:{expires}".encode()
        return hmac.new(self.link_secret, msg, hashlib.sha256).hexdigest()[:32]

    def download_url(self, item):
        expires = int(time.time()) + DOWNLOAD_LINK_SECONDS
        return (f"/dl/{item['id']}/{quote(item['name'])}"
                f"?exp={expires}&sig={self.sign(item['id'], expires)}")


# ---------------------------------------------------------------- server

class Handler(BaseHTTPRequestHandler):
    server_version = APP
    sys_version = ""
    timeout = 120  # drop connections that go silent mid-transfer

    def log_message(self, fmt, *args):
        pass  # we log our own concise lines

    @property
    def state(self):
        return self.server.state

    def send_body(self, code, body, content_type, extra=()):
        self.send_response(code)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Cache-Control", "no-store")
        self.send_header("X-Content-Type-Options", "nosniff")
        self.send_header("Referrer-Policy", "no-referrer")
        for k, v in extra:
            self.send_header(k, v)
        self.end_headers()
        self.wfile.write(body)

    def send_json(self, code, obj):
        self.send_body(code, json.dumps(obj).encode(), "application/json")

    def setup(self):
        super().setup()
        self.secure = isinstance(self.connection, ssl.SSLSocket)

    CERT_PATHS = ("/cert", "/swiftlane.mobileconfig", "/swiftlane-ca.cer")

    def plain_refused(self, path):
        """'Only encrypted connections' is on and this request isn't encrypted: refuse it
        (pages are sent to the encrypted address instead). The certificate pages stay reachable."""
        if self.secure or not self.state.cfg.get("https_only") or not self.state.ca or path in self.CERT_PATHS:
            return False
        if self.command == "GET" and path in STATIC_FILES:
            host, port = self.connection.getsockname()[:2]  # our own address, never the Host header
            self.send_response(301)
            self.send_header("Location", f"https://{host}:{port}{path}")
            self.send_header("Content-Length", "0")
            self.end_headers()
        else:
            self.send_json(403, {"error": "This PC only accepts encrypted connections. Open Swiftlane through its secure link (https://)."})
        return True

    def send_cert_files(self, path):
        ca = self.state.ca
        if not ca:
            self.send_json(404, {"error": "Encrypted connections aren't available on this PC."})
        elif path == "/cert":
            page = (WEB_DIR / "cert.html").read_text("utf-8").replace("{{FINGERPRINT}}", ca.fingerprint)
            self.send_body(200, page.encode("utf-8"), "text/html; charset=utf-8", [("Content-Security-Policy", PAGE_CSP)])
        elif path == "/swiftlane.mobileconfig":
            self.send_body(200, ca.mobileconfig(), "application/x-apple-aspen-config",
                           [("Content-Disposition", 'attachment; filename="Swiftlane.mobileconfig"')])
        else:
            self.send_body(200, ca.der, "application/x-x509-ca-cert", [("Content-Disposition", 'attachment; filename="Swiftlane.cer"')])

    def client_ok(self):
        ip = self.client_address[0]
        if not allowed_client(ip):
            log(f"Refused {ip}: not on your local network")
            self.send_json(403, {"error": "Not on the local network."})
            return False
        if self.state.lockout.locked(ip):
            self.send_json(429, {"error": "Too many wrong codes. Try again in 15 minutes."})
            return False
        return True

    def authorized(self):
        ip = self.client_address[0]
        supplied = normalize_code(self.headers.get("X-Token", "")).encode()
        if hmac.compare_digest(supplied, self.state.code.encode()):
            self.state.lockout.clear(ip)
            return True
        if self.state.lockout.fail(ip):
            log(f"Locked out {ip} for 15 minutes after {FAIL_LIMIT} wrong codes")
        else:
            log(f"Wrong pairing code from {ip}")
        self.send_json(401, {"error": "Wrong pairing code."})
        return False

    def do_GET(self):
        if not self.client_ok():
            return
        url = urlsplit(self.path)
        if self.plain_refused(url.path):
            return
        if url.path in self.CERT_PATHS:
            self.send_cert_files(url.path)
        elif url.path in STATIC_FILES:
            filename, content_type = STATIC_FILES[url.path]
            extra = [("Content-Security-Policy", PAGE_CSP)] if filename.endswith(".html") else []
            self.send_body(200, (WEB_DIR / filename).read_bytes(), content_type, extra)
        elif url.path == "/ping":
            if self.authorized():
                cfg = self.state.cfg
                self.send_json(200, {"ok": True, "app": APP, "pc": socket.gethostname(), "lang": cfg.get("language", "en"),
                                     "remote": bool(cfg["features"].get("remote")), "power": bool(cfg.get("remote_power")),
                                     "secure": self.secure, "tls": bool(self.state.ca)})
        elif url.path == "/api/outbox":
            if self.authorized():
                items = [{"id": i["id"], "name": i["name"], "size": i["size"],
                          "url": self.state.download_url(i)} for i in self.state.outbox.list()]
                self.send_json(200, {"items": items})
        elif url.path.startswith("/dl/"):
            self.send_download(url)
        else:
            self.send_json(404, {"error": "Not found."})

    def send_download(self, url):
        """Serve a queued file. Links are signed and expire, so they work in plain <a href>."""
        item_id = url.path.split("/")[2]
        query = parse_qs(url.query)
        expires = query.get("exp", ["0"])[0]
        sig = query.get("sig", [""])[0]
        valid = (expires.isdigit() and int(expires) > time.time()
                 and hmac.compare_digest(sig.encode(), self.state.sign(item_id, int(expires)).encode()))
        item = self.state.outbox.get(item_id) if valid else None
        if not valid:
            self.send_json(403, {"error": "This link has expired. Refresh the page."})
            return
        if not item or not os.path.isfile(item["path"]):
            self.send_json(404, {"error": "That file is no longer shared."})
            return
        size = os.path.getsize(item["path"])
        ascii_name = item["name"].encode("ascii", "replace").decode().replace('"', "'")
        self.send_response(200)
        self.send_header("Content-Type", mimetypes.guess_type(item["name"])[0] or "application/octet-stream")
        self.send_header("Content-Length", str(size))
        self.send_header("Content-Disposition",
                         f"attachment; filename=\"{ascii_name}\"; filename*=UTF-8''{quote(item['name'])}")
        self.send_header("Cache-Control", "no-store")
        self.send_header("X-Content-Type-Options", "nosniff")
        self.end_headers()
        started = time.monotonic()
        try:
            with open(item["path"], "rb") as f:
                shutil.copyfileobj(f, self.wfile, CHUNK)
        except OSError:
            log(f"Download of {item['name']} interrupted")
            return
        secs = max(time.monotonic() - started, 0.001)
        log(f"Sent {item['name']} to {self.client_address[0]}  ({human(size)}, {human(size / secs)}/s)")
        self.state.outbox.mark_downloaded(item_id)
        self.state.emit("sent", {"id": item_id, "name": item["name"]})

    def do_POST(self):
        if not self.client_ok():
            return
        url = urlsplit(self.path)
        if self.plain_refused(url.path):
            return
        if url.path == "/api/remote":
            self.remote_action()
            return
        if url.path != "/upload":
            self.send_json(404, {"error": "Not found."})
            return
        if not self.authorized():
            return
        if "chunked" in self.headers.get("Transfer-Encoding", "").lower():
            self.send_json(411, {"error": "Content-Length required."})
            return
        try:
            length = int(self.headers["Content-Length"])
            if length < 0:
                raise ValueError
        except (TypeError, ValueError):
            self.send_json(411, {"error": "Content-Length required."})
            return

        state = self.state
        save_dir = state.save_dir
        if length > state.max_bytes:
            self.send_json(413, {"error": f"File is larger than the {human(state.max_bytes)} limit."})
            return
        if shutil.disk_usage(save_dir).free < length + DISK_HEADROOM:
            self.send_json(507, {"error": "Not enough disk space on the PC."})
            return

        raw = parse_qs(url.query).get("name", [""])[0] or unquote(self.headers.get("X-Filename", ""))
        name = safe_name(raw)
        transfer = secrets.token_hex(4)
        tmp = save_dir / f".{secrets.token_hex(8)}.part"
        started = last_emit = time.monotonic()
        state.emit("incoming", {"transfer": transfer, "name": name, "done": 0, "total": length})
        try:
            with open(tmp, "xb") as f:
                remaining = length
                while remaining:
                    chunk = self.rfile.read(min(CHUNK, remaining))
                    if not chunk:
                        raise ConnectionError("upload interrupted")
                    f.write(chunk)
                    remaining -= len(chunk)
                    now = time.monotonic()
                    if now - last_emit > 0.2:
                        last_emit = now
                        state.emit("incoming", {"transfer": transfer, "name": name,
                                                "done": length - remaining, "total": length})
            if not os.path.splitext(name)[1]:
                name += sniff_extension(tmp, self.headers.get("Content-Type"))
            final = self.place(tmp, save_dir, name)
        except Exception as e:
            tmp.unlink(missing_ok=True)
            log(f"Failed receiving {name!r} from {self.client_address[0]}: {e}")
            state.emit("incoming-failed", {"transfer": transfer, "name": name})
            try:
                self.send_json(400, {"error": "Upload failed."})
            except OSError:
                pass
            return

        secs = max(time.monotonic() - started, 0.001)
        log(f"Saved {final.name}  ({human(length)}, {human(length / secs)}/s)")
        entry = state.add_received(final, length, self.client_address[0])
        state.emit("received", {"transfer": transfer, "entry": entry})
        state.notifier.add(final.name)
        self.send_json(200, {"saved": final.name, "bytes": length})

    def remote_action(self):
        """PC remote: one action name from the paired phone, from a fixed list (see toolkit/remote.py)."""
        if not self.authorized():
            return
        st = self.state
        if not st.cfg["features"].get("remote") or not st.remote:
            self.send_json(403, {"error": "The PC remote is switched off in Swiftlane's settings."})
            return
        try:
            length = int(self.headers.get("Content-Length", "0"))
        except ValueError:
            length = -1
        if not 0 < length <= 256:
            self.send_json(400, {"error": "Bad request."})
            return
        try:
            action = json.loads(self.rfile.read(length)).get("action")
        except (ValueError, AttributeError):
            action = None
        now = time.monotonic()
        st.remote_times.append(now)
        if len(st.remote_times) == st.remote_times.maxlen and now - st.remote_times[0] < 2:
            self.send_json(429, {"error": "Too many presses. Wait a moment."})
            return
        problem = st.remote(action, self.client_address[0])
        self.send_json(403 if problem else 200, {"error": problem} if problem else {"ok": True})

    def place(self, tmp, save_dir, name):
        """Move the finished upload into its type folder (Photos, Videos...), or straight
        into the save folder when sorting is switched off."""
        folder = save_dir / category_for(name) if self.state.cfg.get("sort_files", True) else save_dir
        with self.state.place_lock:
            return place_file(tmp, folder, name)


class Server(ThreadingHTTPServer):
    daemon_threads = True
    # http.server turns on address reuse, which on Windows lets a second program
    # bind the same port and intercept uploads. Claim the port exclusively instead.
    allow_reuse_address = False

    def server_bind(self):
        if hasattr(socket, "SO_EXCLUSIVEADDRUSE"):
            self.socket.setsockopt(socket.SOL_SOCKET, socket.SO_EXCLUSIVEADDRUSE, 1)
        super().server_bind()

    tls = None  # ssl.SSLContext once encrypted connections are ready

    def finish_request(self, request, client_address):
        """Encrypted and plain connections share the port: a TLS connection starts with byte 0x16."""
        if self.tls is not None:
            try:
                request.settimeout(15)
                first = request.recv(1, socket.MSG_PEEK)
                if first == b"\x16":
                    request = self.tls.wrap_socket(request, server_side=True)
            except (OSError, ssl.SSLError):
                return
        self.RequestHandlerClass(request, client_address, self)


class PortInUse(Exception):
    pass


def start_server(state):
    """Start serving in a background thread. Raises PortInUse if the port is taken."""
    try:
        server = Server(("0.0.0.0", state.cfg["port"]), Handler)
    except OSError as e:
        if getattr(e, "winerror", None) in (10013, 10048) or e.errno in (98, 48):
            raise PortInUse(state.cfg["port"]) from e
        raise
    server.state = state
    threading.Thread(target=server.serve_forever, daemon=True).start()
    log(f"{APP} {VERSION} listening on port {state.cfg['port']}, saving to {state.save_dir}")
    return server


def local_addresses():
    """[(label, ip)] for the LAN address and any Tailscale address."""
    found = []
    try:
        with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as s:
            s.connect(("8.8.8.8", 80))  # picks the LAN interface; sends nothing
            found.append(("Wi-Fi / LAN", s.getsockname()[0]))
    except OSError:
        pass
    try:
        for info in socket.getaddrinfo(socket.gethostname(), None, socket.AF_INET):
            ip = info[4][0]
            if ipaddress.ip_address(ip) in TAILSCALE_NET and all(ip != f[1] for f in found):
                found.append(("Tailscale", ip))
    except OSError:
        pass
    return found or [("This PC", "127.0.0.1")]


def qr_svg(text):
    """The QR code for `text` as an SVG string, or None without the qrcode package."""
    try:
        import qrcode
    except ImportError:
        return None
    qr = qrcode.QRCode(border=0, error_correction=qrcode.constants.ERROR_CORRECT_M)
    qr.add_data(text)
    matrix = qr.get_matrix()
    n = len(matrix)
    path = "".join(f"M{x},{y}h1v1h-1z" for y, row in enumerate(matrix) for x, on in enumerate(row) if on)
    return (f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {n} {n}" shape-rendering="crispEdges">'
            f'<path fill="#0b0b14" d="{path}"/></svg>')
