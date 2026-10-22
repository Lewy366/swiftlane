"""Swiftlane sharing: links that let people outside your home download files from this PC.

How it is kept small and safe:
- The share server listens on 127.0.0.1 only. The internet reaches it through Tailscale Funnel
  (switched on only while something is shared); your home network can't reach it at all.
- Download only. Nothing can be sent *to* this PC through it.
- A link is a random 128-bit token. URLs never contain file paths: files are picked by their
  number in a list fixed when the share was made. Folders are listed once, without following
  shortcuts/links/junctions and without hidden or system files.
- Every share has an end time; links can have a download limit, be personal (one person each),
  be blocked, and the share can have a password (scrypt, with lockouts).
- Pages are plain server-made HTML with no scripts; file names are escaped; every file is
  sent as a download, never shown in the browser.
"""
import contextlib
import hashlib
import hmac
import html
import json
import os
import re
import secrets
import shutil
import socket
import stat
import subprocess
import sys
import threading
import time
import zipfile
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import parse_qs, quote, urlsplit

import server as core

SHARE_PORT = 8766
CHUNK = 1024 * 1024
MAX_HOURS = 24 * 365
MAX_LINKS = 100
MAX_CONNECTIONS = 48
OPEN_SESSIONS_PER_LINK = 300           # uncounted page visits remembered per link (link previews, bots)
PW_FAILS_PER_ADDRESS = 5               # wrong passwords from one address before it waits
PW_FAILS_PER_LINK = 40                 # wrong passwords on one link (any address) before it waits
PW_LOCK_SECONDS = 15 * 60
RESUME_SECONDS = 15 * 60               # a used-up link stays reachable this long after its last download
HIDDEN, SYSTEM, REPARSE = 0x2, 0x4, 0x400   # Windows file attributes
SENSITIVE = re.compile(r"(^id_(rsa|ed25519|ecdsa|dsa)$|\.(pem|key|pfx|p12|kdbx|ovpn)$|^\.env$|^wallet\.dat$|"
                       r"^(login data|cookies|web data)$|password)", re.I)
PAGE_CSP = ("default-src 'none'; style-src 'unsafe-inline'; img-src 'self'; form-action 'self'; "
            "frame-ancestors 'none'; base-uri 'none'")
TOKEN_RE = re.compile(r"^[A-Za-z0-9_-]{20,40}$")


class ShareError(Exception):
    """A problem to show to the user as it is."""


# ---------------------------------------------------------------- what may be shared

def _attrs(st):
    return getattr(st, "st_file_attributes", 0)


def _is_link(path, st=None):
    """Symlinks, junctions and other reparse points (also cloud placeholders): never followed."""
    st = st or os.lstat(path)
    return stat.S_ISLNK(st.st_mode) or bool(_attrs(st) & REPARSE)


def _known_folders():
    home = Path.home()
    names = ("Desktop", "Documents", "Downloads", "Pictures", "Videos", "Music", "OneDrive")
    folders = {home, core.documents_folder(), *(home / n for n in names)}
    return {os.path.normcase(str(p)) for p in folders}


def _forbidden_folders():
    """Places that are never shared: Windows, programs, app data (incl. Swiftlane's own settings)."""
    env = os.environ
    places = [env.get("SystemRoot", r"C:\Windows"), env.get("ProgramFiles", r"C:\Program Files"),
              env.get("ProgramFiles(x86)", r"C:\Program Files (x86)"), env.get("ProgramData", r"C:\ProgramData"),
              Path.home() / "AppData", core.CONFIG_DIR]
    return [os.path.normcase(os.path.abspath(str(p))) for p in places if p]


def _inside(path, folder):
    path, folder = os.path.normcase(path), os.path.normcase(folder)
    return path == folder or path.startswith(folder.rstrip("\\/") + os.sep)


def collect(paths):
    """The fixed list of files a share will offer, plus what was left out and why.

    Returns {"items": [{"name", "path", "size"}], "total", "skipped": {...}, "warnings": [...]}.
    Raises ShareError for things that are never shared."""
    items, warnings = [], []
    skipped = {"hidden": 0, "links": 0, "unreadable": 0}
    sensitive = []
    known, forbidden = _known_folders(), _forbidden_folders()
    used_names = set()

    def unique(name):
        stem, ext = os.path.splitext(name)
        n, candidate = 1, name
        while candidate.lower() in used_names:
            n += 1
            candidate = f"{stem} ({n}){ext}"
        used_names.add(candidate.lower())
        return candidate

    for raw in paths:
        path = os.path.abspath(str(raw))
        try:
            st = os.lstat(path)
        except OSError:
            raise ShareError(f"Can't find {path}")
        if _is_link(path, st):
            raise ShareError(f"“{os.path.basename(path)}” is a shortcut or link to somewhere else. Share the real file or folder instead.")
        if any(_inside(path, f) for f in forbidden):
            raise ShareError(f"Swiftlane never shares system or program folders ({path}).")
        if stat.S_ISREG(st.st_mode):
            name = unique(os.path.basename(path))
            items.append({"name": name, "path": path, "size": st.st_size})
            if SENSITIVE.search(os.path.basename(path)):
                sensitive.append(name)
            continue
        if not stat.S_ISDIR(st.st_mode):
            raise ShareError(f"Can't share {path}")
        if os.path.normcase(path) in known or os.path.dirname(path.rstrip("\\/")) == path.rstrip("\\/") or len(Path(path).parts) <= 1:
            warnings.append(f"“{path}” is a whole personal folder or drive. Everything in it will be downloadable.")
        root = unique(os.path.basename(path.rstrip("\\/")) or path[:1])
        stack = [(path, root)]
        while stack:
            folder, rel = stack.pop()
            try:
                entries = sorted(os.scandir(folder), key=lambda e: e.name.lower())
            except OSError:
                skipped["unreadable"] += 1
                continue
            for entry in entries:
                try:
                    est = entry.stat(follow_symlinks=False)
                except OSError:
                    skipped["unreadable"] += 1
                    continue
                if _is_link(entry.path, est):
                    skipped["links"] += 1
                    continue
                if entry.name.startswith(".") or _attrs(est) & (HIDDEN | SYSTEM):
                    skipped["hidden"] += 1
                    continue
                if stat.S_ISDIR(est.st_mode):
                    stack.append((entry.path, f"{rel}/{entry.name}"))
                elif stat.S_ISREG(est.st_mode):
                    items.append({"name": f"{rel}/{entry.name}", "path": entry.path, "size": est.st_size})
                    if SENSITIVE.search(entry.name):
                        sensitive.append(f"{rel}/{entry.name}")
    if not items:
        raise ShareError("There's nothing to share: no files found (hidden files and links are left out).")
    if sensitive:
        shown = ", ".join(sensitive[:4]) + (f" and {len(sensitive) - 4} more" if len(sensitive) > 4 else "")
        warnings.append(f"Some files look private (keys, passwords or wallets): {shown}.")
    return {"items": items, "total": sum(i["size"] for i in items), "skipped": skipped, "warnings": warnings}


# ---------------------------------------------------------------- passwords

def hash_password(password, salt=None):
    salt = salt or secrets.token_bytes(16)
    digest = hashlib.scrypt(password.encode(), salt=salt, n=2 ** 14, r=8, p=1, dklen=32)
    return {"salt": salt.hex(), "hash": digest.hex()}


def check_password(password, stored):
    candidate = hash_password(password, bytes.fromhex(stored["salt"]))["hash"]
    return hmac.compare_digest(candidate, stored["hash"])


class Lockout:
    """Wrong passwords: per address and per link, both wait PW_LOCK_SECONDS once over the limit."""

    def __init__(self, now=time.time):
        self.now = now
        self.lock = threading.Lock()
        self.fails = {}  # key -> [count, window_start, locked_until]

    def locked(self, *keys):
        with self.lock:
            return any(self.fails.get(k, [0, 0, 0])[2] > self.now() for k in keys)

    def fail(self, key, limit):
        with self.lock:
            count, start, until = self.fails.get(key, [0, self.now(), 0])
            if self.now() - start > PW_LOCK_SECONDS:
                count, start = 0, self.now()
            count += 1
            until = self.now() + PW_LOCK_SECONDS if count >= limit else until
            self.fails[key] = [count, start, until]

    def clear(self, key):
        with self.lock:
            self.fails.pop(key, None)


# ---------------------------------------------------------------- the list of shares

def _new_link(label, limit, counts="files"):
    """counts "files": every file download (or "Download all") uses one of `limit`.
    counts "visits": one per visitor, who may then take every file (personal links)."""
    return {"id": secrets.token_hex(4), "token": secrets.token_urlsafe(16), "label": label,
            "max": limit, "uses": 0, "blocked": False, "last": None, "counts": counts}


class Shares:
    """Shares, their links and who has downloaded. Saved in %APPDATA%\\Swiftlane\\shares.json."""

    def __init__(self, path, on_change=None, now=time.time):
        self.path = Path(path)
        self.on_change = on_change
        self.on_download = None  # (share, link) when a visit starts its first download
        self.now = now
        self.lock = threading.RLock()
        self.shares = {}
        self.open_sessions = {}  # session id -> {"share", "link", "created"}: visits that haven't downloaded
        try:
            data = json.loads(self.path.read_text("utf-8"))
            self.shares = {s["id"]: s for s in data.get("shares", [])}
        except (OSError, ValueError, KeyError, TypeError):
            pass
        self.prune()

    # ---- saving
    def save(self):
        with self.lock:
            data = json.dumps({"shares": list(self.shares.values())}, indent=1)
            self.path.parent.mkdir(parents=True, exist_ok=True)
            tmp = self.path.with_suffix(".tmp")
            tmp.write_text(data, "utf-8")
            os.replace(tmp, self.path)

    def changed(self):
        self.save()
        if self.on_change:
            try:
                self.on_change()
            except Exception:
                pass

    # ---- state
    def is_active(self, share):
        return not share.get("ended") and share["expires"] > self.now()

    def link_open(self, share, link):
        return self.is_active(share) and not link["blocked"] and (link["max"] is None or link["uses"] < link["max"])

    def any_active(self):
        """True while at least one link could still be used, or a used-up link downloaded in the
        last RESUME_SECONDS (so an interrupted download can still resume). Funnel is on only then."""
        with self.lock:
            return any(self.link_open(s, l) or (self.is_active(s) and not l["blocked"] and l["last"]
                                                and self.now() - l["last"] < RESUME_SECONDS)
                       for s in self.shares.values() for l in s["links"])

    def prune(self):
        """Forget shares that ended or expired more than a day ago."""
        with self.lock:
            cutoff = self.now() - 86400
            gone = [k for k, s in self.shares.items() if (s.get("ended") or s["expires"]) < cutoff]
            for k in gone:
                del self.shares[k]
            if gone:
                self.save()
            return bool(gone)

    # ---- changes from the app
    def create(self, paths, hours=24, mode="open", count=None, password="", labels=None, confirmed=False):
        """mode "open": one link for anyone, optionally limited to `count` downloads.
        mode "personal": one single-use link per person (`count` people, or one per label)."""
        hours = float(hours)
        if not 0 < hours <= MAX_HOURS:
            raise ShareError("Choose a time between a few minutes and one year.")
        found = collect(paths)
        if found["warnings"] and not confirmed:
            return {"confirm": found["warnings"], "files": len(found["items"]), "total": found["total"]}
        if mode == "personal":
            labels = [str(x).strip()[:60] for x in (labels or []) if str(x).strip()]
            n = len(labels) or int(count or 1)
            if not 1 <= n <= MAX_LINKS:
                raise ShareError(f"Personal links: between 1 and {MAX_LINKS} people.")
            links = [_new_link(labels[i] if i < len(labels) else f"Person {i + 1}", 1, "visits") for i in range(n)]
        else:
            limit = int(count) if count not in (None, "", 0, "0") else None
            if limit is not None and limit < 1:
                raise ShareError("The download limit must be at least 1.")
            links = [_new_link("Anyone with the link", limit)]
        roots = []
        for p in paths:
            name = os.path.basename(str(p).rstrip("\\/"))
            if name not in roots:
                roots.append(name)
        title = roots[0] if len(roots) == 1 else f"{roots[0]} + {len(roots) - 1} more"
        share = {"id": secrets.token_hex(6), "title": title, "created": self.now(),
                 "expires": self.now() + hours * 3600, "ended": None,
                 "items": found["items"], "total": found["total"], "skipped": found["skipped"],
                 "password": hash_password(password) if password else None,
                 "links": links, "sessions": {}}
        with self.lock:
            self.shares[share["id"]] = share
            self.changed()
        core.log(f"Share made: {title} ({len(found['items'])} file(s), {core.human(found['total'])}), "
                 f"{len(links)} link(s), ends {time.strftime('%Y-%m-%d %H:%M', time.localtime(share['expires']))}")
        return {"share": share["id"]}

    def _get(self, share_id):
        share = self.shares.get(share_id)
        if not share:
            raise ShareError("That share doesn't exist any more.")
        return share

    def extend(self, share_id, hours):
        hours = float(hours)
        if not 0 < hours <= MAX_HOURS:
            raise ShareError("Choose a time between a few minutes and one year.")
        with self.lock:
            share = self._get(share_id)
            share["expires"] = self.now() + hours * 3600
            share["ended"] = None
            self.changed()

    def end(self, share_id):
        with self.lock:
            share = self._get(share_id)
            share["ended"] = self.now()
            self._drop_sessions(share_id)
            self.changed()
        core.log(f"Share ended: {share['title']}")

    def end_all(self):
        with self.lock:
            for share in self.shares.values():
                if not share.get("ended"):
                    share["ended"] = self.now()
            self.open_sessions.clear()
            self.changed()
        core.log("All shares ended")

    def remove(self, share_id):
        with self.lock:
            self.shares.pop(share_id, None)
            self._drop_sessions(share_id)
            self.changed()

    def set_password(self, share_id, password):
        with self.lock:
            share = self._get(share_id)
            share["password"] = hash_password(password) if password else None
            for sess in share["sessions"].values():  # everyone has to enter the new one
                sess["unlocked"] = False
            self._drop_sessions(share_id)
            self.changed()

    def add_links(self, share_id, count=1, labels=None, limit=1):
        with self.lock:
            share = self._get(share_id)
            labels = [str(x).strip()[:60] for x in (labels or []) if str(x).strip()]
            n = len(labels) or int(count)
            if not 1 <= n or len(share["links"]) + n > MAX_LINKS:
                raise ShareError(f"A share can have up to {MAX_LINKS} links.")
            people = sum(1 for l in share["links"] if l["label"].startswith("Person "))
            for i in range(n):
                share["links"].append(_new_link(labels[i] if i < len(labels) else f"Person {people + i + 1}", limit, "visits"))
            self.changed()

    def set_link(self, share_id, link_id, blocked=None, limit="keep", label=None):
        with self.lock:
            share = self._get(share_id)
            link = next((l for l in share["links"] if l["id"] == link_id), None)
            if not link:
                raise ShareError("That link doesn't exist any more.")
            if blocked is not None:
                link["blocked"] = bool(blocked)
            if limit != "keep":
                link["max"] = None if limit in (None, "", 0) else max(1, int(limit))
            if label is not None and str(label).strip():
                link["label"] = str(label).strip()[:60]
            self.changed()

    def _drop_sessions(self, share_id):
        for sid in [k for k, v in self.open_sessions.items() if v["share"] == share_id]:
            del self.open_sessions[sid]

    # ---- for the app's list
    def summary(self, base_url=None):
        with self.lock:
            out = []
            for s in sorted(self.shares.values(), key=lambda s: s["created"], reverse=True):
                out.append({
                    "id": s["id"], "title": s["title"], "created": s["created"], "expires": s["expires"],
                    "ended": s.get("ended"), "active": self.is_active(s), "files": len(s["items"]), "total": s["total"],
                    "skipped": s["skipped"], "password": bool(s["password"]),
                    "preview": [{"name": i["name"], "size": i["size"], "downloads": i.get("downloads", 0)} for i in s["items"][:200]],
                    "downloads": sum(i.get("downloads", 0) for i in s["items"]),
                    "links": [{"id": l["id"], "label": l["label"], "max": l["max"], "uses": l["uses"],
                               "blocked": l["blocked"], "last": l["last"], "open": self.link_open(s, l),
                               "counts": l.get("counts", "visits"),
                               "url": f"{base_url}/s/{l['token']}" if base_url else None} for l in s["links"]],
                })
            return out

    # ---- for the share server
    def find(self, token):
        """(share, link) for a link token, whatever its state; None if unknown."""
        if not TOKEN_RE.match(token or ""):
            return None
        with self.lock:
            for share in self.shares.values():
                for link in share["links"]:
                    if hmac.compare_digest(link["token"], token):
                        return share, link
        return None

    def session(self, share, link, sid):
        """The visit's session if it belongs to this link, else None."""
        if not sid or len(sid) > 64:
            return None
        with self.lock:
            sess = share["sessions"].get(sid)
            if sess and sess["link"] == link["id"]:
                return sess
            sess = self.open_sessions.get(sid)
            if sess and sess["share"] == share["id"] and sess["link"] == link["id"]:
                return sess
        return None

    def new_session(self, share, link):
        sid = secrets.token_urlsafe(18)
        with self.lock:
            mine = [k for k, v in self.open_sessions.items() if v["link"] == link["id"]]
            for k in mine[:max(0, len(mine) - OPEN_SESSIONS_PER_LINK + 1)]:  # forget the oldest previews/bots
                del self.open_sessions[k]
            self.open_sessions[sid] = {"share": share["id"], "link": link["id"], "created": self.now(),
                                       "counted": False, "unlocked": False}
        return sid

    def unlock(self, share, sid):
        with self.lock:
            sess = share["sessions"].get(sid) or self.open_sessions.get(sid)
            if sess:
                sess["unlocked"] = True
                if sid in share["sessions"]:
                    self.save()

    def start_download(self, share, link, sid, item="zip", resume=False):
        """Called before sending file data. `item` is the file's number or "zip" (Download all);
        `resume` is True for a request that continues a download (a range not starting at 0).

        Every download counts once: on a "files" link it uses one of the link's downloads, and
        the file's own counter goes up. Continuing a download this visit already started is
        free, so a broken download can finish. On a "visits" link (personal links) only the
        visit's first download uses one up. Returns None if allowed, else the reason."""
        with self.lock:
            if not self.is_active(share):
                return "ended"
            if link["blocked"]:
                return "blocked"
            sess = self.session(share, link, sid)
            if not sess:
                return "no-session"
            if share["password"] and not sess.get("unlocked"):
                return "password"
            started = sess.get("started", [])
            fresh = not (resume and item in started)
            per_file = link.get("counts") == "files"
            uses_one = fresh if per_file else not sess.get("counted")
            if uses_one:
                if link["max"] is not None and link["uses"] >= link["max"]:
                    return "used-up"
                link["uses"] += 1
            if fresh:
                for it in (share["items"] if item == "zip" else [share["items"][item]]):
                    it["downloads"] = it.get("downloads", 0) + 1
            if item not in started:
                started = started + [item]
            if sid in share["sessions"]:
                sess["started"] = started
            else:  # the visit's first download: remember it across restarts so it can resume
                self.open_sessions.pop(sid, None)
                share["sessions"][sid] = {"link": link["id"], "counted": True, "started": started,
                                          "unlocked": bool(sess.get("unlocked")), "created": sess.get("created", self.now())}
            link["last"] = self.now()
            self.changed()
        if fresh and self.on_download:
            try:
                self.on_download(share, link)
            except Exception:
                pass
        return None


# ---------------------------------------------------------------- the pages people see

PAGE_STYLE = """
:root{color-scheme:light dark;--bg:#f6f5f2;--fg:#1d1d1b;--muted:#6b6a65;--line:#dcdad3;--card:#fff;--acc:#1f5fbf}
@media (prefers-color-scheme:dark){:root{--bg:#1b1b1a;--fg:#ecebe6;--muted:#9a988f;--line:#34332f;--card:#232321;--acc:#7fb0ff}}
*{box-sizing:border-box}body{margin:0;background:var(--bg);color:var(--fg);font:15px/1.5 "Segoe UI",system-ui,-apple-system,sans-serif}
main{max-width:720px;margin:0 auto;padding:40px 18px}h1{font-size:22px;font-weight:600;margin:0 0 4px;overflow-wrap:anywhere}
p{margin:0 0 16px;color:var(--muted)}.box{background:var(--card);border:1px solid var(--line);border-radius:8px}
.row{display:flex;align-items:center;gap:12px;padding:10px 14px;border-top:1px solid var(--line)}.row:first-child{border-top:0}
.name{flex:1;min-width:0;overflow-wrap:anywhere}.size{color:var(--muted);font-variant-numeric:tabular-nums;white-space:nowrap}
a.btn,button{display:inline-block;background:var(--acc);color:#fff;border:0;border-radius:6px;padding:8px 14px;font:inherit;
text-decoration:none;cursor:pointer;white-space:nowrap}a.dl{color:var(--acc);text-decoration:none;white-space:nowrap}
input{font:inherit;padding:8px 10px;border:1px solid var(--line);border-radius:6px;background:var(--card);color:var(--fg);width:100%;max-width:320px}
.top{display:flex;gap:12px;align-items:center;margin:0 0 16px;flex-wrap:wrap}.err{color:#c0392b}.foot{margin-top:24px;font-size:13px}
"""


def _page(title, body, status=200):
    doc = (f'<!doctype html><html><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">'
           f'<meta name="robots" content="noindex,nofollow"><meta name="referrer" content="no-referrer">'
           f'<title>{html.escape(title)}</title><style>{PAGE_STYLE}</style></head><body><main>{body}</main></body></html>')
    return status, doc.encode("utf-8")


def _gone_page():
    return _page("Link not available", "<h1>This link isn’t available</h1>"
                 "<p>It may have expired, been used up or been switched off by the person who sent it.</p>", 404)


def _left(seconds):
    if seconds < 3600:
        return f"{max(1, round(seconds / 60))} min"
    if seconds < 86400 * 2:
        return f"{round(seconds / 3600)} h"
    return f"{round(seconds / 86400)} days"


def human(n):
    return core.human(n)


# ---------------------------------------------------------------- the share server

class Handler(BaseHTTPRequestHandler):
    server_version = "Swiftlane"
    sys_version = ""
    timeout = 60

    def log_message(self, fmt, *args):
        pass

    @property
    def shares(self):
        return self.server.shares

    # ---- helpers
    def client_ip(self):
        # Tailscale adds the visitor's address as the last X-Forwarded-For entry. Only
        # programs on this PC can reach this server, so the header can be trusted that far.
        forwarded = self.headers.get("X-Forwarded-For", "")
        last = forwarded.split(",")[-1].strip()
        return last[:64] if last else self.client_address[0]

    def host_ok(self):
        host = (self.headers.get("Host") or "").lower()
        return host in self.server.allowed_hosts()

    def send_page(self, status, body, extra=()):
        self.send_response(status)
        self.send_header("Content-Type", "text/html; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
        self.common_headers()
        self.send_header("Content-Security-Policy", PAGE_CSP)
        for k, v in extra:
            self.send_header(k, v)
        self.end_headers()
        if self.command != "HEAD":
            self.wfile.write(body)

    def common_headers(self):
        self.send_header("Cache-Control", "no-store")
        self.send_header("X-Content-Type-Options", "nosniff")
        self.send_header("Referrer-Policy", "no-referrer")
        self.send_header("X-Robots-Tag", "noindex, nofollow")
        self.send_header("X-Frame-Options", "DENY")

    def not_found(self):
        self.send_page(*_gone_page())

    def route(self):
        """(share, link, rest) for /s/<token>[/rest], or None."""
        url = urlsplit(self.path)
        parts = url.path.split("/")
        if len(parts) < 3 or parts[1] != "s":
            return None
        found = self.shares.find(parts[2])
        if not found:
            return None
        return found[0], found[1], "/".join(parts[3:]), parse_qs(url.query)

    def cookie_session(self, link):
        for part in (self.headers.get("Cookie") or "").split(";"):
            k, _, v = part.strip().partition("=")
            if k == f"sl_{link['id']}":
                return v
        return None

    # ---- requests
    def do_HEAD(self):
        # Only the page answers HEAD; file links would otherwise use up a download without sending it.
        found = self.route() if self.host_ok() else None
        if found and found[2] == "":
            self.do_GET()
        else:
            self.send_error(405)

    def do_GET(self):
        if not self.host_ok():
            self.send_error(421)
            return
        found = self.route()
        if not found:
            self.not_found()
            return
        share, link, rest, query = found
        if rest == "":
            self.show_page(share, link)
            return
        with self.server.transfer():
            if rest == "zip":
                self.send_zip(share, link, query.get("s", [None])[0])
            elif rest.startswith("f/") and rest[2:].isdigit() and len(rest) < 12:
                self.send_file(share, link, int(rest[2:]), query.get("s", [None])[0])
            else:
                self.not_found()

    def do_POST(self):
        if not self.host_ok():
            self.send_error(421)
            return
        found = self.route()
        if not found or found[2] != "" or not found[0]["password"]:
            self.not_found()
            return
        share, link, _, _ = found
        try:
            length = int(self.headers.get("Content-Length", "0"))
        except ValueError:
            length = -1
        if not 0 <= length <= 4096:
            self.send_error(413)
            return
        form = parse_qs(self.rfile.read(length).decode("utf-8", "replace"))
        sid = self.cookie_session(link)
        sess = self.shares.session(share, link, sid)
        ip = self.client_ip()
        lock = self.server.lockout
        if not self.shares.link_open(share, link) and not (sess and sess.get("counted")):
            self.not_found()
            return
        if not sess:
            self.show_page(share, link)
            return
        if lock.locked(("ip", link["id"], ip), ("link", link["id"])):
            self.send_page(*self.password_page(share, "Too many wrong tries. Wait 15 minutes and try again.", 429))
            return
        if check_password(form.get("password", [""])[0], share["password"]):
            lock.clear(("ip", link["id"], ip))
            self.shares.unlock(share, sid)
            self.send_response(303)
            self.send_header("Location", f"/s/{link['token']}")
            self.send_header("Content-Length", "0")
            self.common_headers()
            self.end_headers()
        else:
            lock.fail(("ip", link["id"], ip), PW_FAILS_PER_ADDRESS)
            lock.fail(("link", link["id"]), PW_FAILS_PER_LINK)
            core.log(f"Share {share['title']!r}: wrong password from {ip}")
            self.send_page(*self.password_page(share, "Wrong password."))

    # ---- the download page
    def password_page(self, share, error="", status=200):
        err = f'<p class="err">{html.escape(error)}</p>' if error else ""
        return _page("Password needed", f"<h1>Password needed</h1><p>Enter the password you were given to see the files.</p>{err}"
                     f'<form method="post"><div class="top"><input type="password" name="password" autofocus '
                     f'autocomplete="off" aria-label="Password"><button type="submit">Open</button></div></form>', status)

    def show_page(self, share, link):
        sid = self.cookie_session(link)
        sess = self.shares.session(share, link, sid)
        counted = bool(sess and sess.get("counted"))
        if not self.shares.is_active(share) or link["blocked"] or (not counted and not self.shares.link_open(share, link)):
            self.not_found()
            return
        extra = []
        if not sess:
            sid = self.shares.new_session(share, link)
            sess = self.shares.session(share, link, sid)
            secure = "; Secure" if self.headers.get("X-Forwarded-Proto") == "https" else ""
            extra.append(("Set-Cookie", f"sl_{link['id']}={sid}; Path=/s/{link['token']}; HttpOnly; SameSite=Lax{secure}"))
        if share["password"] and not sess.get("unlocked"):
            self.send_page(*self.password_page(share), extra=extra)
            return
        q = f"?s={quote(sid)}"
        rows = []
        for n, item in enumerate(share["items"][:2000]):
            rows.append(f'<div class="row"><span class="name">{html.escape(item["name"])}</span>'
                        f'<span class="size">{human(item["size"])}</span>'
                        f'<a class="dl" href="/s/{link["token"]}/f/{n}{q}" download>Download</a></div>')
        more = len(share["items"]) - 2000
        if more > 0:
            rows.append(f'<div class="row"><span class="name">and {more} more files (in “Download all”)</span></div>')
        files = len(share["items"])
        left = _left(share["expires"] - time.time())
        all_btn = (f'<a class="btn" href="/s/{link["token"]}/zip{q}">Download all ({human(share["total"])}, .zip)</a>'
                   if files > 1 else
                   f'<a class="btn" href="/s/{link["token"]}/f/0{q}">Download ({human(share["total"])})</a>')
        body = (f"<h1>{html.escape(share['title'])}</h1>"
                f"<p>{files} file{'s' if files != 1 else ''} · {human(share['total'])} · available for {left}</p>"
                f'<div class="top">{all_btn}</div><div class="box">{"".join(rows)}</div>'
                f'<p class="foot">Sent with Swiftlane, straight from the sender’s PC.</p>')
        self.send_page(*_page(share["title"], body), extra=extra)

    # ---- sending files
    def allowed(self, share, link, sid, item="zip", resume=False):
        sid = sid or self.cookie_session(link)
        reason = self.shares.start_download(share, link, sid, item, resume)
        if reason:
            if reason == "password" or reason == "no-session":
                self.send_response(303)
                self.send_header("Location", f"/s/{link['token']}")
                self.send_header("Content-Length", "0")
                self.common_headers()
                self.end_headers()
            else:
                self.not_found()
            return False
        return True

    def send_file(self, share, link, index, sid):
        if not 0 <= index < len(share["items"]):
            self.not_found()
            return
        item = share["items"][index]
        try:
            st = os.lstat(item["path"])
            if _is_link(item["path"], st) or not stat.S_ISREG(st.st_mode):
                raise OSError("not a plain file any more")
            f = open(item["path"], "rb")
        except OSError:
            self.send_page(*_page("File not available", "<h1>That file isn’t available any more</h1>"
                                  "<p>The sender moved or deleted it.</p>", 410))
            return
        with f:
            size = os.fstat(f.fileno()).st_size
            start, end = 0, size - 1
            ranged = self.parse_range(size)
            if ranged == "bad":
                self.send_response(416)
                self.send_header("Content-Range", f"bytes */{size}")
                self.send_header("Content-Length", "0")
                self.common_headers()
                self.end_headers()
                return
            if not self.allowed(share, link, sid, index, resume=bool(ranged) and ranged[0] > 0):
                return
            if ranged:
                start, end = ranged
            self.send_response(206 if ranged else 200)
            self.send_header("Content-Type", "application/octet-stream")
            self.send_header("Content-Length", str(end - start + 1 if size else 0))
            self.send_header("Accept-Ranges", "bytes")
            if ranged:
                self.send_header("Content-Range", f"bytes {start}-{end}/{size}")
            self.send_header("Content-Disposition", disposition(os.path.basename(item["name"])))
            self.common_headers()
            self.end_headers()
            if self.command == "HEAD" or not size:
                return
            f.seek(start)
            remaining = end - start + 1
            started = time.monotonic()
            try:
                while remaining:
                    chunk = f.read(min(CHUNK, remaining))
                    if not chunk:
                        break
                    self.wfile.write(chunk)
                    remaining -= len(chunk)
            except OSError:
                return
            if not ranged:
                secs = max(time.monotonic() - started, 0.001)
                core.log(f"Share {share['title']!r}: sent {os.path.basename(item['name'])} ({human(size)}, "
                         f"{human(size / secs)}/s) via “{link['label']}”")

    def parse_range(self, size):
        """(start, end) for a single "bytes=" range, None for the whole file, "bad" if unsatisfiable."""
        value = (self.headers.get("Range") or "").strip()
        m = re.fullmatch(r"bytes=(\d*)-(\d*)", value)
        if not value or not m or (not m.group(1) and not m.group(2)):
            return None
        if m.group(1):
            start = int(m.group(1))
            end = min(int(m.group(2)), size - 1) if m.group(2) else size - 1
        else:
            start, end = max(0, size - int(m.group(2))), size - 1
        if start >= size or start > end:
            return "bad"
        return start, end

    def send_zip(self, share, link, sid):
        if len(share["items"]) < 2:
            self.not_found()
            return
        if not self.allowed(share, link, sid):
            return
        self.send_response(200)
        self.send_header("Content-Type", "application/zip")
        self.send_header("Content-Disposition", disposition(f"{share['title']}.zip"))
        self.common_headers()
        self.end_headers()  # no length: the zip is made while it's sent; the connection closing ends it
        if self.command == "HEAD":
            return
        out = _Stream(self.wfile)
        try:
            with zipfile.ZipFile(out, "w", zipfile.ZIP_STORED, allowZip64=True) as zf:
                for item in share["items"]:
                    try:
                        st = os.lstat(item["path"])
                        if _is_link(item["path"], st) or not stat.S_ISREG(st.st_mode):
                            continue
                        src = open(item["path"], "rb")
                    except OSError:
                        continue
                    with src:
                        info = zipfile.ZipInfo(item["name"], time.localtime(max(st.st_mtime, 315532800))[:6])
                        info.file_size = os.fstat(src.fileno()).st_size
                        with zf.open(info, "w") as dst:
                            shutil.copyfileobj(src, dst, CHUNK)
        except OSError:
            return
        core.log(f"Share {share['title']!r}: sent all as zip ({human(out.n)}) via “{link['label']}”")


class _Stream:
    """A write-only, unseekable file for zipfile, counting what was written."""

    def __init__(self, wfile):
        self.w, self.n = wfile, 0

    def write(self, b):
        self.w.write(b)
        self.n += len(b)
        return len(b)

    def tell(self):
        return self.n

    def flush(self):
        self.w.flush()


def disposition(name):
    ascii_name = re.sub(r'[^\x20-\x7e]|["\\]', "_", name)
    return f"attachment; filename=\"{ascii_name}\"; filename*=UTF-8''{quote(name)}"


class ShareServer(ThreadingHTTPServer):
    daemon_threads = True
    allow_reuse_address = False

    def __init__(self, port, shares, host_name=None):
        self.shares = shares
        self.host_name = host_name  # e.g. mypc.tail1234.ts.net
        self.lockout = Lockout()
        self.slots = threading.BoundedSemaphore(MAX_CONNECTIONS)
        self.busy = 0  # downloads running right now
        self._busy_lock = threading.Lock()
        super().__init__(("127.0.0.1", port), Handler)

    @contextlib.contextmanager
    def transfer(self):
        with self._busy_lock:
            self.busy += 1
        try:
            yield
        finally:
            with self._busy_lock:
                self.busy -= 1

    def allowed_hosts(self):
        port = self.server_address[1]
        hosts = {f"127.0.0.1:{port}", f"localhost:{port}"}
        if self.host_name:
            hosts |= {self.host_name.lower(), f"{self.host_name.lower()}:443"}
        return hosts

    def server_bind(self):
        if hasattr(socket, "SO_EXCLUSIVEADDRUSE"):
            self.socket.setsockopt(socket.SOL_SOCKET, socket.SO_EXCLUSIVEADDRUSE, 1)
        super().server_bind()

    def process_request(self, request, client_address):
        if not self.slots.acquire(blocking=False):  # too many at once: drop instead of queueing
            self.shutdown_request(request)
            return
        try:
            super().process_request(request, client_address)
        except Exception:
            self.slots.release()
            raise

    def process_request_thread(self, request, client_address):
        try:
            super().process_request_thread(request, client_address)
        finally:
            self.slots.release()


# ---------------------------------------------------------------- Tailscale Funnel

class Funnel:
    """Switches Tailscale Funnel on (pointing at the share server) and off. Only ever touches
    its own entry; if something else already uses Funnel/serve on port 443, it leaves it alone."""

    def __init__(self, port, run=subprocess.run):
        self.port = port
        self.run = run
        self.target = f"http://127.0.0.1:{port}"

    def exe(self):
        found = shutil.which("tailscale")
        if found:
            return found
        default = Path(os.environ.get("ProgramFiles", r"C:\Program Files")) / "Tailscale" / "tailscale.exe"
        return str(default) if default.exists() else None

    def _call(self, *args, timeout=20):
        exe = self.exe()
        if not exe:
            raise ShareError("Tailscale isn't installed.")
        flags = {"creationflags": subprocess.CREATE_NO_WINDOW} if sys.platform == "win32" else {}
        try:
            r = self.run([exe, *args], capture_output=True, text=True, timeout=timeout, **flags)
        except subprocess.TimeoutExpired as e:
            out = (e.stdout or "") + (e.stderr or "")
            out = out.decode("utf-8", "replace") if isinstance(out, bytes) else out
            return 124, out
        return r.returncode, (r.stdout or "") + (r.stderr or "")

    def status(self):
        """What the Share page shows: is Tailscale ready, is Funnel allowed, is our link on?"""
        if not self.exe():
            return {"state": "no-tailscale"}
        code, out = self._call("status", "--json", timeout=10)
        try:
            data = json.loads(out)
        except ValueError:
            return {"state": "not-running"}
        if data.get("BackendState") != "Running":
            return {"state": "not-running"}
        me = data.get("Self") or {}
        host = (me.get("DNSName") or "").rstrip(".")
        caps = set(me.get("Capabilities") or []) | set((me.get("CapMap") or {}).keys())
        https = any(c == "https" or c.endswith("/https") for c in caps)
        funnel_ok = any("funnel" in c for c in caps)
        mine, other = self.config()
        state = "on" if mine else ("needs-https" if not https else "needs-funnel" if not funnel_ok else "off")
        if other and not mine:
            state = "in-use"
        return {"state": state, "host": host, "url": f"https://{host}" if host else None}

    def config(self):
        """(our entry is on, something else uses 443)."""
        code, out = self._call("funnel", "status", "--json", timeout=10)
        try:
            cfg = json.loads(out or "{}")
        except ValueError:
            return False, False
        mine = other = False
        for hostport, web in (cfg.get("Web") or {}).items():
            if not hostport.endswith(":443"):
                continue
            for handler in (web.get("Handlers") or {}).values():
                if handler.get("Proxy", "").rstrip("/") == self.target:
                    mine = bool((cfg.get("AllowFunnel") or {}).get(hostport))
                else:
                    other = True
        if "443" in (cfg.get("TCP") or {}) and not (cfg.get("Web") or {}):
            other = True
        return mine, other

    def on(self):
        mine, other = self.config()
        if mine:
            return {"ok": True}
        if other:
            raise ShareError("Tailscale Funnel or Serve is already used by something else on this PC, so Swiftlane won't change it.")
        code, out = self._call("funnel", "--bg", self.target, timeout=25)
        approve = re.search(r"https://login\.tailscale\.com/\S+", out or "")
        if approve:
            return {"approve": approve.group(0)}
        mine, _ = self.config()
        if not mine:
            raise ShareError("Tailscale didn't switch Funnel on: " + (out or "").strip()[:200])
        core.log("Tailscale Funnel on (sharing)")
        return {"ok": True}

    def off(self):
        mine, _ = self.config()
        if not mine:
            return
        self._call("funnel", "--https=443", "off", timeout=20)
        core.log("Tailscale Funnel off (nothing shared)")


# ---------------------------------------------------------------- keeping the PC awake

def keep_awake():
    """Tell Windows the PC is in use (call regularly while shares are active)."""
    if sys.platform == "win32":
        import ctypes
        ctypes.windll.kernel32.SetThreadExecutionState(0x00000001)  # ES_SYSTEM_REQUIRED
