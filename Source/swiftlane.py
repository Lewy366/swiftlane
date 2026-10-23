"""Swiftlane desktop app: iPhone <-> PC transfer plus a set of everyday tools
(file converter, image tools, video downloader, currency converter, calculator).

Opens the Swiftlane window; the iPhone server runs in the background while the
"Phone transfer" feature is on. `--headless` runs just the server in a console.
"""
import argparse
import collections
import ctypes
import itertools
import json
import os
import re
import shutil
import subprocess
import sys
import threading
import time
import traceback
from pathlib import Path

import server as core
import share
import updater
from toolkit import adblock, calc, convert, currency, downloader, images, pdftools, speedtest, trimmer, vault, diskmap, netscan, exposure
from toolkit.common import Cancelled, clipboard_text

RUN_KEY = r"Software\Microsoft\Windows\CurrentVersion\Run"
FEATURES = ("transfer", "share", "remote", "pdf", "trimmer", "vault", "diskmap", "netscan", "exposure", "converter", "images", "downloader", "currency", "calculator", "speedtest", "adblock")
TAILSCALE_PAGES = ("https://login.tailscale.com/",)  # the only outside pages the Share page may open
CALC_METHODS = {  # what the calculator page may call, nothing else
    "evaluate", "solve", "set_mode", "memory", "memory_add", "clear_memory", "base_n", "statistics",
    "distribution", "polynomial", "simultaneous", "inequality", "set_matrix", "matrix_eval",
    "table", "ratio",
}


def message_box(text, title=core.APP):
    if sys.platform == "win32":
        ctypes.windll.user32.MessageBoxW(None, text, title, 0x40)
    else:
        print(text)


def already_running():
    """True if another Swiftlane window is open (a per-user lock, independent of the network)."""
    if sys.platform != "win32":
        return False
    kernel32 = ctypes.windll.kernel32
    kernel32.CreateMutexW.restype = ctypes.c_void_p
    already_running._handle = kernel32.CreateMutexW(None, False, "Local\\SwiftlaneSingleInstance")
    return kernel32.GetLastError() == 183  # ERROR_ALREADY_EXISTS


def focus_existing_window():
    if sys.platform != "win32":
        return False
    hwnd = ctypes.windll.user32.FindWindowW(None, core.APP)
    if hwnd:
        ctypes.windll.user32.ShowWindow(hwnd, 9)  # SW_RESTORE
        ctypes.windll.user32.SetForegroundWindow(hwnd)
    return bool(hwnd)


def launch_command():
    if getattr(sys, "frozen", False):
        return f'"{sys.executable}" --minimized'
    return f'"{sys.executable}" "{Path(__file__).resolve()}" --minimized'


def autostart_enabled():
    import winreg
    try:
        with winreg.OpenKey(winreg.HKEY_CURRENT_USER, RUN_KEY) as key:
            winreg.QueryValueEx(key, core.APP)
            return True
    except OSError:
        return False


def set_autostart(enabled):
    import winreg
    with winreg.OpenKey(winreg.HKEY_CURRENT_USER, RUN_KEY, 0, winreg.KEY_SET_VALUE) as key:
        if enabled:
            winreg.SetValueEx(key, core.APP, 0, winreg.REG_SZ, launch_command())
        else:
            try:
                winreg.DeleteValue(key, core.APP)
            except FileNotFoundError:
                pass


def file_info(path):
    p = Path(path)
    return {"path": str(p), "name": p.name, "size": p.stat().st_size if p.is_file() else 0,
            "kind": convert.kind_of(p), "targets": convert.targets_for(convert.kind_of(p))}


class Jobs:
    """Long-running work (conversions, downloads) in background threads with progress."""

    def __init__(self, push, notifier):
        self.push = push
        self.notifier = notifier
        self.ids = itertools.count(1)
        self.cancel_flags = {}

    def start(self, kind, title, work, notify=True):
        job_id = str(next(self.ids))
        flag = threading.Event()
        self.cancel_flags[job_id] = flag

        def report(fraction=None, text=None):
            self.push("job", {"id": job_id, "kind": kind, "title": title, "status": "running",
                              "progress": fraction, "text": text})

        def run():
            report(0, "Starting…")
            try:
                outputs = work(report, flag.is_set)
                core.record_created(outputs)
                self.push("job", {"id": job_id, "kind": kind, "title": title, "status": "done",
                                  "progress": 1, "outputs": [str(o) for o in outputs]})
                if notify:
                    self.notifier.show_soon("Done", title)
            except Cancelled:
                self.push("job", {"id": job_id, "kind": kind, "title": title, "status": "cancelled"})
            except Exception as e:
                core.log(f"{kind} job failed: {e}")
                self.push("job", {"id": job_id, "kind": kind, "title": title, "status": "error",
                                  "text": _friendly_error(e)})
            finally:
                self.cancel_flags.pop(job_id, None)

        threading.Thread(target=run, daemon=True).start()
        return job_id

    def cancel(self, job_id):
        flag = self.cancel_flags.get(str(job_id))
        if flag:
            flag.set()


def _friendly_error(e):
    text = str(e) or e.__class__.__name__
    text = text.replace("ERROR: ", "")
    if "Unsupported URL" in text:
        return "That site or link isn't supported."
    if "Video unavailable" in text or "Private video" in text:
        return "That video isn't available (private, removed or region-locked)."
    return text[:300]


class Api:
    """Methods the window's JavaScript can call (window.pywebview.api.*).

    Attributes start with _ so pywebview doesn't expose or walk them.
    """

    def __init__(self, state):
        self._state = state
        self._window = None
        self._server = None
        self._server_error = None
        self._calc = calc.Calculator()
        self._calc.set_mode(angle=state.cfg.get("calc_angle", "deg"))
        self._rates = currency.Rates(core.CONFIG_DIR / "rates.json")
        self._jobs = None  # set once the window exists
        self._start_page = None
        self._vault = None          # the encrypted vault (locked until you type the password)
        self._vault_used = 0.0      # last vault activity, for locking it again on its own
        self._disk = None           # last disk scan: (root folder, biggest files)
        self._net = None            # last network scan
        self._net_lock = threading.Lock()
        self._exposure = None       # last 'Is my PC exposed?' result
        self._remote_log = collections.deque(maxlen=12)  # recent PC-remote presses, shown on the PC remote page
        self._phone_names = {}      # ip -> name the router knows (e.g. "Lewys-iPhone")
        # Sharing: the share server and Tailscale Funnel run only while a link can still be used.
        self._shares = share.Shares(core.CONFIG_DIR / "shares.json", on_change=self._shares_changed)
        self._shares.on_download = self._share_downloaded
        self._shares.on_upload = lambda s, name, size: self._state.notifier.show_soon("File received through a link", f"{name} · {s['title']}")
        self._funnel = share.Funnel(share.SHARE_PORT)
        self._share_server = None
        self._share_host = None
        self._share_lock = threading.Lock()
        self._funnel_note = None  # last problem switching Funnel on, shown on the Share page
        state.remote = self._remote

    # ---------------------------------------------------------------- app state
    def get_state(self):
        s = self._state
        port = s.cfg["port"]
        ca = s.ca
        scheme = "https" if ca and s.cfg.get("https_only") else "http"
        addresses = [{"label": label, "host": f"{ip}:{port}",
                      "shortcut": f"{scheme}://{ip}:{port}/upload?name="}
                     for label, ip in core.local_addresses()]
        first_ip = core.local_addresses()[0][1]
        tls_info = {"ready": bool(ca), "https_only": bool(s.cfg.get("https_only")),
                    "fingerprint": ca.fingerprint if ca else "",
                    "cert_qr": core.qr_svg(f"http://{first_ip}:{port}/cert") if ca else None,
                    "secure_qr": core.qr_svg(f"https://{first_ip}:{port}/#{s.code}") if ca else None}
        return {
            "version": core.VERSION,
            "features": s.cfg["features"],
            "server_running": self._server is not None,
            "server_error": self._server_error,
            "code_pretty": core.pretty_code(s.code),
            "qr": core.qr_svg(f"{scheme}://{first_ip}:{port}/#{s.code}"),
            "tls": tls_info,
            "addresses": addresses,
            "save_dir": str(s.save_dir),
            "notify": bool(s.cfg.get("notify")),
            "autostart": autostart_enabled() if sys.platform == "win32" else False,
            "config_dir": str(core.CONFIG_DIR),
            "outbox": s.outbox.list(),
            "received": list(s.received),
            "formats": convert.supported_formats(),
            "download_choices": {k: v["label"] for k, v in downloader.CHOICES.items()},
            "image_ratios": list(images.RATIOS),
            "calc": self._calc.memory(),
            "sort_files": bool(s.cfg.get("sort_files", True)),
            "download_dir": s.cfg.get("download_dir") or "",
            "calc_angle": s.cfg.get("calc_angle", "deg"),
            "language": s.cfg.get("language", "en"),
            "theme": s.cfg["theme"],
            "constants": [list(c) for c in calc.CONSTANTS],
            "conversions": [[c[0], c[1], c[2]] for c in calc.CONVERSIONS],
            "regressions": calc.REGRESSIONS,
            "node": bool(downloader.find_program("node")),
            "start_page": self._start_page,
            "update_check": s.cfg.get("update_check", True),
            "sidebar": s.cfg.get("sidebar"),
            "remote_power": bool(s.cfg.get("remote_power")),
            "context_menu": self._context_menu_on(),
            "explorer": self._take_explorer(),
            "update": self._update_public(),
            "updated": self._take_whats_new(),
        }

    def set_feature(self, name, enabled):
        if name not in FEATURES:
            return self.get_state()
        self._state.cfg["features"][name] = bool(enabled)
        core.save_config(self._state.cfg)
        if name == "transfer":
            self._start_server() if enabled else self._stop_server()
        if name == "share" and not enabled:
            self._shares.end_all()  # switching sharing off ends every link and closes Funnel
            threading.Thread(target=self._sync_sharing, daemon=True).start()
        return self.get_state()

    def _start_server(self):
        if self._server:
            return
        try:
            self._server = core.start_server(self._state)
            self._server_error = None
            self._setup_tls()
        except core.PortInUse as e:
            self._server_error = f"Port {e.args[0]} is in use by another program, so phone transfer is off."
            core.log(self._server_error)

    _tls_addrs = ()

    def _setup_tls(self):
        """Encrypted phone transfer: load (or make) Swiftlane's CA and a certificate for today's addresses."""
        try:
            import tls
            ca = self._state.ca or tls.Authority(core.CONFIG_DIR / "tls")
            self._tls_addrs = tuple(ip for _, ip in core.local_addresses())
            self._server.tls = ca.server_context(self._tls_addrs)
            self._state.ca = ca
        except Exception as e:
            core.log(f"Encrypted phone transfer unavailable: {e}")
            self._state.ca = None

    def _stop_server(self):
        if self._server:
            self._server.shutdown()
            self._server.server_close()
            self._server = None
            core.log("Phone transfer switched off; no longer listening on the network")

    # ---------------------------------------------------------------- files & folders
    def pick_files(self, purpose="any"):
        import webview
        paths = self._window.create_file_dialog(webview.FileDialog.OPEN, allow_multiple=True) or []
        return [file_info(p) for p in paths]

    def pick_folder(self):
        import webview
        picked = self._window.create_file_dialog(webview.FileDialog.FOLDER)
        return picked[0] if picked else None

    def open_path(self, path):
        if path and os.path.exists(path):
            os.startfile(path)

    def reveal_path(self, path):
        if path and os.path.exists(path):
            subprocess.Popen(["explorer", "/select,", os.path.normpath(path)])

    def copy_text(self, text):
        subprocess.run(["clip"], input=str(text).encode("utf-16-le"),
                       creationflags=subprocess.CREATE_NO_WINDOW, check=False)

    def paste_text(self):
        return clipboard_text()

    def cancel_job(self, job_id):
        self._jobs.cancel(job_id)

    # ---------------------------------------------------------------- Phone transfer
    def add_outbox(self, paths):
        self._state.outbox.add(paths)
        return self._state.outbox.list()

    def get_outbox(self):
        return self._state.outbox.list()

    def remove_outbox(self, item_id):
        self._state.outbox.remove(item_id)
        return self._state.outbox.list()

    def clear_outbox(self):
        self._state.outbox.clear()
        return []

    def open_received(self, entry_id):
        entry = self._state.find_received(entry_id)
        if entry:
            self.open_path(entry["path"])

    def reveal_received(self, entry_id):
        entry = self._state.find_received(entry_id)
        if entry:
            self.reveal_path(entry["path"])

    # ---------------------------------------------------------------- Share links
    def share_info(self, check=False):
        """Shares plus where Tailscale stands. check=True asks Tailscale again first."""
        if check or self._funnel_state is None:
            self._refresh_funnel()
        return {"shares": self._shares.summary(self._base_url()), "funnel": self._funnel_state,
                "note": self._funnel_note, "defaults": self._state.cfg["share"],
                "busy": self._share_server.busy if self._share_server else 0}

    def share_create(self, paths, options=None, confirmed=False):
        """Makes the share and copies its link right away; Funnel is switched on in the background."""
        if not self._state.cfg["features"].get("share"):
            return {"error": "Sharing is switched off in Settings."}
        o = options or {}
        try:
            r = self._shares.create(list(paths or []), hours=o.get("hours") or 24, mode=o.get("mode", "open"),
                                    count=o.get("count"), password=str(o.get("password") or ""),
                                    labels=o.get("labels"), confirmed=bool(confirmed))
        except share.ShareError as e:
            return {"error": str(e)}
        except (ValueError, TypeError):
            return {"error": "Check the share settings."}
        if "confirm" in r:
            return r
        self._ensure_share_server()
        links = next(s for s in self._shares.summary(self._base_url()) if s["id"] == r["share"])["links"]
        copied = None
        if len(links) == 1 and links[0]["url"]:
            self.copy_text(links[0]["url"])
            copied = links[0]["url"]
        threading.Thread(target=self._sync_sharing, daemon=True).start()
        return {"share": r["share"], "copied": copied, **self.share_info()}

    def share_request(self, options=None):
        """A link people use to send files to this PC. Files land in <save folder>\\From links\\<title>."""
        if not self._state.cfg["features"].get("share"):
            return {"error": "Sharing is switched off in Settings."}
        o = options or {}
        title = str(o.get("title") or "").strip()[:80] or "Files for me"
        folder = self._state.save_dir / "From links" / core.safe_name(f"{title} {time.strftime('%Y-%m-%d')}")
        try:
            r = self._shares.create_request(folder, title, hours=o.get("hours") or 24, max_mb=o.get("max_mb") or 2048,
                                            max_files=o.get("max_files") or 50, password=str(o.get("password") or ""))
        except share.ShareError as e:
            return {"error": str(e)}
        except (ValueError, TypeError):
            return {"error": "Check the request settings."}
        self._ensure_share_server()
        link = next(s for s in self._shares.summary(self._base_url()) if s["id"] == r["share"])["links"][0]
        if link["url"]:
            self.copy_text(link["url"])
        threading.Thread(target=self._sync_sharing, daemon=True).start()
        return {"share": r["share"], "copied": link["url"], **self.share_info()}

    def share_open_folder(self, share_id):
        s = self._shares.shares.get(share_id)
        if s and s.get("folder"):
            Path(s["folder"]).mkdir(parents=True, exist_ok=True)
            os.startfile(s["folder"])

    def _share_call(self, fn, *args, sync=False):
        try:
            fn(*args)
        except share.ShareError as e:
            return {"error": str(e)}
        except (ValueError, TypeError):
            return {"error": "That value isn't valid."}
        if sync:
            threading.Thread(target=self._sync_sharing, daemon=True).start()
        return self.share_info()

    def share_extend(self, share_id, hours):
        return self._share_call(self._shares.extend, share_id, hours, sync=True)

    def share_end(self, share_id):
        return self._share_call(self._shares.end, share_id, sync=True)

    def share_end_all(self):
        return self._share_call(self._shares.end_all, sync=True)

    def share_remove(self, share_id):
        return self._share_call(self._shares.remove, share_id, sync=True)

    def share_password(self, share_id, password):
        return self._share_call(self._shares.set_password, share_id, str(password or ""))

    def share_add_links(self, share_id, count=1, labels=None):
        return self._share_call(self._shares.add_links, share_id, int(count or 1), labels, sync=True)

    def share_link(self, share_id, link_id, changes):
        c = changes or {}
        return self._share_call(lambda: self._shares.set_link(share_id, link_id, blocked=c.get("blocked"),
                                                              limit=c.get("limit", "keep"), label=c.get("label")), sync=True)

    def share_copy(self, share_id, link_id):
        for s in self._shares.summary(self._base_url()):
            for link in s["links"]:
                if s["id"] == share_id and link["id"] == link_id and link["url"]:
                    self.copy_text(link["url"])
                    return link["url"]
        return None

    def share_retry(self):
        """Ask Tailscale again (after you've switched something on in its admin page)."""
        self._sync_sharing()
        return self.share_info()

    def share_open_page(self, url):
        if isinstance(url, str) and url.startswith(TAILSCALE_PAGES) and len(url) < 500:
            os.startfile(url)

    def share_pick(self, folder=False):
        import webview
        if folder:
            picked = self._window.create_file_dialog(webview.FileDialog.FOLDER)
        else:
            picked = self._window.create_file_dialog(webview.FileDialog.OPEN, allow_multiple=True)
        return list(picked or [])

    def set_share_defaults(self, values):
        d = self._state.cfg["share"]
        v = values or {}
        try:
            if "hours" in v and 0 < float(v["hours"]) <= share.MAX_HOURS:
                d["hours"] = float(v["hours"])
            if v.get("mode") in ("open", "personal"):
                d["mode"] = v["mode"]
            if "count" in v:
                d["count"] = int(v["count"]) if v["count"] not in (None, "", 0) and int(v["count"]) > 0 else None
            if "people" in v and 1 <= int(v["people"]) <= share.MAX_LINKS:
                d["people"] = int(v["people"])
            if "keep_awake" in v:
                d["keep_awake"] = bool(v["keep_awake"])
        except (ValueError, TypeError):
            pass
        core.save_config(self._state.cfg)
        return d

    # ---- sharing in the background
    _funnel_state = None

    def _base_url(self):
        return f"https://{self._share_host}" if self._share_host else None

    def _refresh_funnel(self):
        try:
            self._funnel_state = self._funnel.status()
        except Exception as e:
            self._funnel_state = {"state": "error", "error": str(e)[:200]}
        host = self._funnel_state.get("host")
        if host:
            self._share_host = host
            if self._share_server:
                self._share_server.host_name = host

    def _ensure_share_server(self):
        if self._share_server:
            return
        if self._share_host is None:
            self._refresh_funnel()
        try:
            srv = share.ShareServer(share.SHARE_PORT, self._shares, self._share_host)
        except OSError:
            self._funnel_note = {"error": f"Port {share.SHARE_PORT} is used by another program, so links can't work."}
            return
        threading.Thread(target=srv.serve_forever, daemon=True).start()
        self._share_server = srv
        core.log(f"Share server on (127.0.0.1:{share.SHARE_PORT}, only reachable through Tailscale Funnel)")

    def _stop_share_server(self):
        if self._share_server:
            self._share_server.shutdown()
            self._share_server.server_close()
            self._share_server = None
            core.log("Share server off")

    def _sharing_needed(self):
        busy = self._share_server.busy if self._share_server else 0
        return bool(self._state.cfg["features"].get("share")) and (self._shares.any_active() or busy > 0)

    def _sync_sharing(self):
        """Server and Funnel on while a link can be used (or a download is running), off otherwise."""
        with self._share_lock:
            if self._sharing_needed():
                self._ensure_share_server()
                try:
                    r = self._funnel.on()
                    self._funnel_note = {"approve": r["approve"]} if r.get("approve") else None
                except share.ShareError as e:
                    self._funnel_note = {"error": str(e)}
                except Exception as e:
                    self._funnel_note = {"error": f"Tailscale: {e}"[:200]}
            else:
                try:
                    self._funnel.off()
                except Exception as e:
                    core.log(f"Couldn't switch Tailscale Funnel off: {e}")
                self._stop_share_server()
                self._funnel_note = None
            self._refresh_funnel()
        self._push_shares()

    def _share_loop(self):
        """Every 30 s: forget old shares, close everything once nothing is shared, keep the PC awake."""
        self._sync_sharing()  # also closes a Funnel left open by a crash
        while True:
            time.sleep(30)
            self._shares.prune()
            if self._server and self._state.ca and tuple(ip for _, ip in core.local_addresses()) != self._tls_addrs:
                self._setup_tls()  # new network address: new certificate
            needed = self._sharing_needed()
            if needed != bool(self._share_server):
                self._sync_sharing()
            elif self._shares.shares:
                self._push_shares()
            if needed and self._state.cfg["share"].get("keep_awake", True):
                share.keep_awake()

    def _shutdown_sharing(self):
        """Swiftlane is closing: links stop working until it runs again, so close Funnel too."""
        try:
            self._funnel.off()
        except Exception:
            pass
        self._stop_share_server()

    def _push_shares(self):
        if self._jobs:
            self._jobs.push("shares", self.share_info())

    def _shares_changed(self):
        self._push_shares()

    def _share_downloaded(self, s, link):
        self._state.notifier.show_soon("Download started", f"{s['title']} · {link['label']}")

    def open_folder(self):
        os.startfile(self._state.save_dir)

    def choose_folder(self):
        import webview
        picked = self._window.create_file_dialog(webview.FileDialog.FOLDER,
                                                 directory=str(self._state.save_dir))
        if not picked:
            return None
        self._state.set_save_dir(picked[0])
        core.log(f"Save folder changed to {picked[0]}")
        return str(self._state.save_dir)

    # ---------------------------------------------------------------- updates (GitHub Releases, signed)
    _update = None          # the latest verified release, if newer than this version
    _updating = False
    WHATS_NEW = core.CONFIG_DIR / "update-notes.json"

    def _update_public(self):
        u = self._update
        return {k: u[k] for k in ("version", "notes", "size", "published")} if u else None

    def check_update(self):
        """Ask GitHub for the latest release and verify its signature. Never installs anything."""
        try:
            info = updater.check(core.VERSION)
        except updater.UpdateError as e:
            return {"error": str(e)}
        self._update = info if info["available"] else None
        return {"available": info["available"], "current": core.VERSION, "latest": info["version"], "update": self._update_public()}

    def install_update(self):
        """The user clicked Update: download, verify, then start Setup's update window (Windows asks for admin)."""
        if not self._update or self._updating:
            return {"error": "There's no update to install."}
        if self._shares.any_active() and not getattr(self, "_update_confirmed_shares", False):
            self._update_confirmed_shares = True
            return {"confirm": "Updating closes Swiftlane for a minute, so your share links pause until it reopens."}
        self._updating = True
        try:
            push = (lambda f: self._jobs.push("update-progress", {"fraction": f})) if self._jobs else None
            path = updater.download(self._update, progress=push)
            if not updater.still_matches(path, self._update):
                raise updater.UpdateError("The download changed after it was checked, so it was rejected.")
            self.WHATS_NEW.write_text(json.dumps({"version": self._update["version"], "notes": self._update["notes"]}), "utf-8")
            if not run_elevated(str(path), "--update", show=True, directory=path.parent):
                self.WHATS_NEW.unlink(missing_ok=True)
                return {"error": "Windows didn't give permission, so nothing was changed."}
        except updater.UpdateError as e:
            return {"error": str(e)}
        finally:
            self._updating = False
        core.log(f"Updating to {self._update['version']}")
        threading.Timer(1.0, lambda: self._window.destroy()).start()  # Setup takes over from here
        return {"ok": True}

    def _take_whats_new(self):
        """After an update: what's new in this version, shown once."""
        try:
            data = json.loads(self.WHATS_NEW.read_text("utf-8"))
        except (OSError, ValueError):
            return None
        if updater.version_tuple(data.get("version")) != updater.version_tuple(core.VERSION):
            return None
        self.WHATS_NEW.unlink(missing_ok=True)
        return {"version": core.VERSION, "notes": data.get("notes", "")}

    def _update_loop(self):
        """Look for updates shortly after starting and then once a day; tell the user, never install."""
        time.sleep(25)
        while True:
            if self._state.cfg.get("update_check", True) and not self._updating:
                r = self.check_update()
                u = r.get("update")
                if u and self._state.cfg.get("update_notified") != u["version"]:
                    self._state.cfg["update_notified"] = u["version"]
                    core.save_config(self._state.cfg)
                    self._state.notifier.show_soon("Update available", f"Swiftlane {u['version']} is ready. Open Swiftlane to see what's new and update.")
                if u and self._jobs:
                    self._jobs.push("update-available", u)
            time.sleep(24 * 3600)

    # ---------------------------------------------------------------- Explorer right-click menu
    _explorer_batch = None
    _explorer_lock = threading.Lock()

    def _from_explorer(self, cmd, paths):
        """Files from the right-click menu. Explorer may start one Swiftlane per file, so requests
        arriving within a moment are gathered into one batch."""
        with self._explorer_lock:
            if self._explorer_batch is None:
                self._explorer_batch = {"cmd": cmd, "paths": []}
                threading.Timer(0.8, self._flush_explorer).start()
            if self._explorer_batch["cmd"] == cmd:
                self._explorer_batch["paths"] += [p for p in paths if p not in self._explorer_batch["paths"]]

    _explorer_pending = None

    def _flush_explorer(self):
        with self._explorer_lock:
            batch, self._explorer_batch = self._explorer_batch, None
        files = [file_info(p) for p in batch["paths"] if os.path.isfile(p)]
        request = {"cmd": batch["cmd"], "files": files, "folders": [p for p in batch["paths"] if os.path.isdir(p)]}
        if self._jobs:
            self._jobs.push("explorer", request)
        else:
            self._explorer_pending = request  # the window isn't ready yet: picked up by get_state

    def _take_explorer(self):
        request, self._explorer_pending = self._explorer_pending, None
        return request

    def _context_menu_on(self):
        try:
            import context_menu
            return context_menu.is_enabled()
        except Exception:
            return False

    def set_context_menu(self, enabled):
        import context_menu
        try:
            context_menu.enable() if enabled else context_menu.disable()
        except OSError as e:
            return {"error": f"Couldn't change the right-click menu ({e.__class__.__name__})"}
        core.log(f"Explorer right-click menu {'added' if enabled else 'removed'}")
        return {"ok": True, "enabled": context_menu.is_enabled()}

    def _remote(self, action, ip):
        """Called by the phone server for a PC-remote press. Returns None, or what to tell the phone."""
        from toolkit import remote
        if action not in remote.ACTIONS:
            return "Unknown action."
        if action in remote.POWER and not self._state.cfg.get("remote_power"):
            return "Sleep and shut down from the phone are switched off in Swiftlane's settings."
        if action in remote.POWER or action == "lock":
            core.log(f"PC remote from {ip}: {action}")
        if action == "shutdown":
            self._state.notifier.show_soon("Shutting down in 60 seconds", "Your phone asked to shut down this PC. Cancel it on the phone, or run: shutdown /a")
        problem = None
        try:
            remote.perform(action)
        except Exception as e:
            core.log(f"PC remote {action} failed: {e}")
            problem = "That didn't work on the PC."
        self._remote_seen(action, ip, problem)
        return problem

    def _remote_seen(self, action, ip, problem=None):
        """Remember a press for the PC remote page (so you can see it arrive), with the phone's name if the router knows it."""
        entry = {"action": action, "ip": ip, "name": self._phone_names.get(ip), "time": time.time(), "error": problem}
        self._remote_log.appendleft(entry)
        if self._jobs:
            self._jobs.push("remote-press", self.remote_state())
        if ip not in self._phone_names:
            def look_up():
                from toolkit import netscan
                self._phone_names[ip] = netscan.reverse_name(ip) if ip not in ("127.0.0.1", "::1") else None
                for e in self._remote_log:
                    if e["ip"] == ip:
                        e["name"] = self._phone_names[ip]
                if self._jobs and self._phone_names[ip]:
                    self._jobs.push("remote-press", self.remote_state())
            threading.Thread(target=look_up, daemon=True).start()

    def remote_state(self):
        cfg = self._state.cfg
        return {"on": bool(cfg["features"].get("remote")), "power": bool(cfg.get("remote_power")),
                "transfer": bool(cfg["features"].get("transfer")) and self._server is not None, "log": list(self._remote_log)}

    def remote_try(self, action):
        """'Try it on this PC': the same key a phone would press, from the PC remote page (sound and music keys only)."""
        from toolkit import remote
        if action not in ("play_pause", "previous", "next", "volume_down", "volume_up", "mute"):
            return {"error": "Unknown action."}
        try:
            remote.perform(action)
        except Exception as e:
            return {"error": f"That didn't work on this PC ({e.__class__.__name__})."}
        return True

    def set_sidebar(self, layout):
        """Pinned tools and folders. Only plain names and tool ids are stored; the page checks ids itself."""
        ok_id = lambda x: isinstance(x, str) and re.fullmatch(r"[a-z]{2,24}", x)
        try:
            clean = {"pinned": [x for x in layout.get("pinned", []) if ok_id(x)][:60], "folders": []}
            for f in layout.get("folders", [])[:20]:
                name = re.sub(r"[\x00-\x1f]", "", str(f.get("name", "")))[:40].strip() or "Folder"
                clean["folders"].append({"name": name, "items": [x for x in f.get("items", []) if ok_id(x)][:60],
                                         "open": bool(f.get("open", True))})
        except (AttributeError, TypeError):
            return False
        self._state.cfg["sidebar"] = clean
        core.save_config(self._state.cfg)
        return True

    def set_option(self, name, value):
        """Simple settings from the Settings page (only these names are accepted)."""
        cfg = self._state.cfg
        if name == "sort_files":
            cfg["sort_files"] = bool(value)
        elif name == "calc_angle" and value in ("deg", "rad", "gra"):
            cfg["calc_angle"] = value
        elif name == "language" and value in ("en", "de", "pl", "it"):
            cfg["language"] = value
        elif name == "update_check":
            cfg["update_check"] = bool(value)
        elif name == "https_only":
            cfg["https_only"] = bool(value)
            core.log(f"Phone transfer: unencrypted connections {'refused' if value else 'allowed'}")
        elif name == "net_watch":
            cfg["net_watch"] = bool(value)
        elif name == "vault_lock_minutes" and value in (1, 5, 15, 30, 60):
            cfg["vault_lock_minutes"] = value
        elif name == "remote_power":
            cfg["remote_power"] = bool(value)
            core.log(f"PC remote: sleep and shut down from the phone {'allowed' if value else 'not allowed'}")
        else:
            return False
        core.save_config(cfg)
        return True

    def choose_download_dir(self):
        start = self._state.cfg.get("download_dir") or self._state.save_dir
        folder = self._ask_folder(start)
        if folder:
            self._state.cfg["download_dir"] = folder
            core.save_config(self._state.cfg)
        return folder

    def reset_download_dir(self):
        self._state.cfg["download_dir"] = ""
        core.save_config(self._state.cfg)

    # ---------------------------------------------------------------- appearance
    def set_theme(self, theme):
        t = self._state.cfg["theme"]
        for k, v in (theme or {}).items():
            if k == "accent" and isinstance(v, str) and len(v) < 20:
                t["accent"] = v
            elif k == "custom" and isinstance(v, str) and len(v) == 7 and v.startswith("#"):
                t["custom"] = v
            elif k in ("blur", "dim"):
                t[k] = max(0, min(100, int(v)))
            elif k == "blur_on":
                t[k] = bool(v)
            elif k == "mode" and v in ("dark", "light", "auto"):
                t[k] = v
        core.save_config(self._state.cfg)
        return t

    BG_ANIMATED = {".gif": "image/gif", ".webp": "image/webp", ".mp4": "video/mp4", ".webm": "video/webm"}
    BG_MAX_MB = 60

    def choose_background(self):
        """Pick a picture, GIF or short video. Pictures are kept as a resized copy; animated
        GIF/WebP and videos are copied as they are (up to BG_MAX_MB), all in the settings folder."""
        import webview
        picked = self._window.create_file_dialog(webview.FileDialog.OPEN, file_types=(
            "Pictures GIFs and videos (*.jpg;*.jpeg;*.png;*.webp;*.heic;*.bmp;*.gif;*.mp4;*.webm)", "All files (*.*)"))
        if not picked:
            return None
        src = Path(picked[0])
        ext = src.suffix.lower()
        try:
            from PIL import Image, ImageOps
            animated = ext in (".mp4", ".webm")
            if ext in (".gif", ".webp"):
                with Image.open(src) as im:
                    animated = getattr(im, "is_animated", False)
            for old in core.CONFIG_DIR.glob("background.*"):
                old.unlink(missing_ok=True)
            if animated:
                if src.stat().st_size > self.BG_MAX_MB * 1024 * 1024:
                    return {"error": f"That file is over {self.BG_MAX_MB} MB. Pick a shorter or smaller animation."}
                target = core.CONFIG_DIR / f"background{ext}"
                shutil.copyfile(src, target)
            else:
                images._open(src).close()  # registers HEIC support
                target = core.CONFIG_DIR / "background.jpg"
                with Image.open(src) as im:
                    im = ImageOps.exif_transpose(im).convert("RGB")
                    im.thumbnail((2560, 2560))
                    im.save(target, "JPEG", quality=88)
        except Exception as e:
            return {"error": f"Can't use that file ({e.__class__.__name__})"}
        self._state.cfg["theme"]["background"] = True
        self._state.cfg["theme"]["bg_file"] = target.name
        core.save_config(self._state.cfg)
        return self.get_background()

    def clear_background(self):
        self._state.cfg["theme"]["background"] = False
        self._state.cfg["theme"]["bg_file"] = ""
        core.save_config(self._state.cfg)
        for old in core.CONFIG_DIR.glob("background.*"):
            old.unlink(missing_ok=True)

    def get_background(self):
        """The background as a data: URL (image/... or video/...), or None for Swiftlane's own wallpaper."""
        name = self._state.cfg["theme"].get("bg_file") or "background.jpg"
        path = core.CONFIG_DIR / name
        if not self._state.cfg["theme"].get("background") or not path.exists():
            return None
        import base64
        mime = self.BG_ANIMATED.get(path.suffix.lower(), "image/jpeg")
        return f"data:{mime};base64," + base64.b64encode(path.read_bytes()).decode()

    def set_notify(self, enabled):
        self._state.cfg["notify"] = bool(enabled)
        core.save_config(self._state.cfg)

    def set_autostart(self, enabled):
        try:
            set_autostart(bool(enabled))
            return True
        except OSError:
            return False

    def new_code(self):
        self._state.regenerate_code()
        core.log("Pairing code reset")
        return self.get_state()

    # ---------------------------------------------------------------- "where to save?" dialogs
    def _ask_save(self, directory, filename, ext, other_exts=()):
        """Windows 'Save as' box, pre-filled. Returns the chosen path, or None if cancelled.
        Windows itself asks before replacing an existing file."""
        import webview
        types = tuple(f"{e.upper()} file (*.{e})" for e in (ext, *[x for x in other_exts if x != ext])) + ("All files (*.*)",)
        picked = self._window.create_file_dialog(webview.FileDialog.SAVE, directory=str(directory),
                                                 save_filename=filename, file_types=types)
        if not picked:
            return None
        return picked[0] if isinstance(picked, (list, tuple)) else picked

    def _ask_folder(self, directory):
        import webview
        picked = self._window.create_file_dialog(webview.FileDialog.FOLDER, directory=str(directory))
        return (picked[0] if isinstance(picked, (list, tuple)) else picked) if picked else None

    # ---------------------------------------------------------------- converter
    def convert(self, items, options):
        """items: [{"path", "target"}]; options: quality, combine_pdf. Asks where to save first.
        Returns the job ids, or None if the user cancelled the dialog."""
        combine_pdf = bool((options or {}).get("combine_pdf"))
        options = {k: v for k, v in (options or {}).items() if k == "quality"}
        to_pdf = [i["path"] for i in items if i.get("target") == "pdf" and convert.kind_of(i["path"]) == "image"]
        combined = combine_pdf and len(to_pdf) > 1
        rest = [i for i in items if not (combined and i["path"] in to_pdf)]

        # Ask first, start converting only once every question is answered.
        pdf_out = None
        if combined:
            first = Path(to_pdf[0])
            pdf_out = self._ask_save(first.parent, f"{first.stem} + {len(to_pdf) - 1} more.pdf", "pdf")
            if not pdf_out:
                return None
        single = len(rest) == 1 and convert.kind_of(rest[0]["path"]) != "pdf"
        if single:
            src, target = Path(rest[0]["path"]), rest[0]["target"]
            out = self._ask_save(src.parent, f"{src.stem}.{target}", target)
            if not out:
                return None
            rest[0] = {**rest[0], "output": out}
        elif rest:
            folder = self._ask_folder(Path(rest[0]["path"]).parent)
            if not folder:
                return None
            options["folder"] = folder

        job_ids = []
        if combined:
            job_ids.append(self._jobs.start("convert", f"{len(to_pdf)} images → one PDF",
                                            lambda report, cancelled: [convert.images_to_pdf(to_pdf, output=pdf_out)]))
        for item in rest:
            src, target = item["path"], item["target"]
            opts = {**options, **({"output": item["output"]} if item.get("output") else {})}
            job_ids.append(self._jobs.start(
                "convert", f"{Path(src).name} → {target.upper()}",
                lambda report, cancelled, src=src, target=target, opts=opts: convert.convert_file(
                    src, target, opts, progress=lambda f: report(f, None), cancelled=cancelled)))
        return job_ids

    # ---------------------------------------------------------------- image editor
    def edit_images(self, paths, ops):
        """Asks where to save (a file for one image, a folder for several), then saves.
        Returns the job id, or None if the user cancelled the dialog."""
        ops = dict(ops or {})
        if len(paths) == 1:
            src = Path(paths[0])
            _, ext = images._target_format(ops, None, src)
            out = self._ask_save(src.parent, f"{src.stem} (edited).{ext}", ext, ("jpg", "png", "webp"))
            if not out:
                return None
            ops["output"] = out
        else:
            folder = self._ask_folder(Path(paths[0]).parent)
            if not folder:
                return None
            ops["folder"] = folder
        results = []

        def work(report, cancelled):
            outs = []
            for n, p in enumerate(paths):
                if cancelled():
                    raise Cancelled()
                r = images.edit_image(p, ops)
                results.append(r)
                outs.append(r["output"])
                report((n + 1) / len(paths), f"{n + 1} of {len(paths)} · {Path(p).name}: "
                       f"{_kb(r['before'])} → {_kb(r['after'])}")
            return outs

        title = f"Edit {len(paths)} image{'s' if len(paths) != 1 else ''}"
        return self._jobs.start("images", title, work)

    def image_preview(self, path):
        try:
            return images.preview_source(path)
        except Exception as e:
            return {"error": f"Can't open this image ({e.__class__.__name__})"}

    def image_render(self, path, ops):
        try:
            return images.render_preview(path, ops or {})
        except Exception as e:
            return {"error": f"Preview failed: {e}"}

    # ---------------------------------------------------------------- PDF tools
    def pdf_pick(self):
        import webview
        picked = self._window.create_file_dialog(webview.FileDialog.OPEN, allow_multiple=True, file_types=("PDF files (*.pdf)", "All files (*.*)"))
        return list(picked or [])

    def pdf_open(self, paths):
        out = []
        for p in paths:
            try:
                out.append({"path": p, **pdftools.thumbnails(p)})
            except (ValueError, OSError) as e:
                out.append({"path": p, "name": Path(p).name, "error": str(e)})
        return out

    def pdf_page(self, path, index):
        try:
            return pdftools.page_image(path, index)
        except (ValueError, OSError, IndexError):
            return None

    def pdf_save(self, sources, plan, signatures=None):
        if not plan:
            return {"error": "There are no pages to save."}
        first = Path(sources[int(plan[0]["src"])])
        out = self._ask_save(first.parent, f"{first.stem} (edited).pdf", "pdf")
        if not out:
            return None
        return self._jobs.start("pdf", f"{len(plan)} pages → {Path(out).name}",
                                lambda report, cancelled: pdftools.build(sources, plan, out, signatures or []))

    def pdf_split(self, path, groups):
        folder = self._ask_folder(Path(path).parent)
        if not folder:
            return None
        return self._jobs.start("pdf", f"Split {Path(path).name} into {len(groups)}",
                                lambda report, cancelled: pdftools.split(path, folder, groups))

    def pdf_smaller(self, path):
        src = Path(path)
        out = self._ask_save(src.parent, f"{src.stem} (smaller).pdf", "pdf")
        if not out:
            return None
        return self._jobs.start("pdf", f"Make {src.name} smaller",
                                lambda report, cancelled: pdftools.make_smaller(path, out, progress=lambda f: report(f, None), cancelled=cancelled))

    def pick_signature(self):
        """A picture of a signature, cut down to a sensible size."""
        import webview, base64, io
        picked = self._window.create_file_dialog(webview.FileDialog.OPEN, file_types=("Images (*.png;*.jpg;*.jpeg)", "All files (*.*)"))
        if not picked:
            return None
        try:
            from PIL import Image
            with Image.open(picked[0]) as im:
                im = im.convert("RGBA"); im.thumbnail((1200, 600))
                buf = io.BytesIO(); im.save(buf, "PNG")
            return "data:image/png;base64," + base64.b64encode(buf.getvalue()).decode()
        except Exception:
            return {"error": "Can't use that picture."}

    # ---------------------------------------------------------------- video trimmer
    def trim_open(self, path):
        try:
            info = trimmer.probe(path)
        except (ValueError, OSError, subprocess.SubprocessError) as e:
            return {"error": str(e) or "Can't read this video."}
        return {**info, "path": path, "strip": trimmer.strip(path, info["duration"], 10), "preview": trimmer.frame(path, 0)}

    def trim_frame(self, path, t):
        try:
            return trimmer.frame(path, t)
        except (OSError, subprocess.SubprocessError):
            return None

    def trim_save(self, path, start, end, mode, target_mb=10, height=0):
        src = Path(path)
        ext = "mp3" if mode == "audio" else (src.suffix.lstrip(".").lower() if mode == "fast" else "mp4") or "mp4"
        out = self._ask_save(src.parent, f"{src.stem} (cut).{ext}", ext)
        if not out:
            return None
        label = {"fast": "Cut", "exact": "Exact cut", "size": f"Cut to {target_mb} MB", "audio": "Sound only"}.get(mode, "Cut")
        return self._jobs.start("trim", f"{label} · {src.name}", lambda report, cancelled: trimmer.trim(
            path, start, end, out, mode, target_mb, height, progress=lambda f: report(f, None), cancelled=cancelled))

    # ---------------------------------------------------------------- encrypted vault
    def _vault_box(self, touch=True):
        folder = self._state.cfg.get("vault_dir") or str(core.documents_folder() / f"{core.APP} Vault")
        if self._vault is None or str(self._vault.folder) != folder:
            if self._vault:
                self._vault.lock()
            self._vault = vault.Vault(folder)
        if touch:
            self._vault_used = time.monotonic()
        return self._vault

    def vault_state(self):
        v = self._vault_box(touch=False)
        return {"folder": str(v.folder), "exists": v.exists(), "unlocked": v.unlocked,
                "items": v.items() if v.unlocked else [], "lock_minutes": self._state.cfg.get("vault_lock_minutes", 5)}

    def vault_create(self, password, again):
        if password != again:
            return {"error": "The two passwords don't match."}
        try:
            self._vault_box().create(password)
        except (vault.VaultError, OSError) as e:
            return {"error": str(e)}
        core.log("Vault created")
        return self.vault_state()

    def vault_unlock(self, password):
        try:
            self._vault_box().unlock(password)
        except vault.WrongPassword as e:
            core.log("Vault: wrong password")
            time.sleep(1)  # slows down guessing through the window
            return {"error": str(e)}
        except (vault.VaultError, OSError) as e:
            return {"error": str(e)}
        return self.vault_state()

    def vault_lock(self):
        if self._vault:
            self._vault.lock()
        return self.vault_state()

    def vault_choose_folder(self):
        """Open a vault somewhere else (e.g. on a USB stick), or pick where a new one goes."""
        start = Path(self._vault_box(touch=False).folder).parent
        folder = self._ask_folder(start if start.exists() else Path.home())
        if not folder:
            return None
        chosen = Path(folder)
        if not (chosen / "vault.json").exists():
            chosen = chosen / f"{core.APP} Vault"  # never mix vault files into one of your own folders
        self.vault_lock()
        self._state.cfg["vault_dir"] = str(chosen)
        core.save_config(self._state.cfg)
        return self.vault_state()

    def _vault_open_box(self):
        v = self._vault_box()
        if not v.unlocked:
            raise vault.VaultError("The vault is locked.")
        return v

    def vault_add(self, paths=None, shred=False):
        import webview
        try:
            v = self._vault_open_box()
        except vault.VaultError as e:
            return {"error": str(e)}
        if not paths:
            paths = list(self._window.create_file_dialog(webview.FileDialog.OPEN, allow_multiple=True) or [])
        paths = [p for p in paths if Path(p).is_file()]
        if not paths:
            return {"error": "Only files can go into the vault (not folders)."}

        def work(report, cancelled):
            def progress(f):
                self._vault_used = time.monotonic()  # don't lock in the middle of a big file
                report(f, None)
            try:
                v.add(paths, progress=progress, cancelled=cancelled, shred_originals=bool(shred))
            finally:
                self._jobs.push("vault", self.vault_state())
            return []
        n = len(paths)
        return self._jobs.start("vault", f"Lock {n} file{'s' if n != 1 else ''} in the vault", work, notify=False)

    def vault_export(self, ids):
        try:
            v = self._vault_open_box()
        except vault.VaultError as e:
            return {"error": str(e)}
        folder = self._ask_folder(self._state.save_dir)
        if not folder:
            return None
        ids = [str(i) for i in ids]
        return self._jobs.start("vault", f"Take {len(ids)} file{'s' if len(ids) != 1 else ''} out of the vault",
                                lambda report, cancelled: v.export(ids, folder, cancelled))

    def vault_open(self, file_id):
        try:
            path = self._vault_open_box().open_copy(str(file_id))
        except (vault.VaultError, OSError) as e:
            return {"error": str(e)}
        os.startfile(path)
        return True

    def vault_remove(self, ids):
        try:
            self._vault_open_box().remove([str(i) for i in ids])
        except (vault.VaultError, OSError) as e:
            return {"error": str(e)}
        return self.vault_state()

    def vault_rename(self, file_id, name):
        try:
            self._vault_open_box().rename(str(file_id), str(name))
        except (vault.VaultError, OSError) as e:
            return {"error": str(e)}
        return self.vault_state()

    def vault_password(self, old, new, again):
        if new != again:
            return {"error": "The two new passwords don't match."}
        try:
            self._vault_box().change_password(old, new)
        except vault.WrongPassword as e:
            time.sleep(1)
            return {"error": str(e)}
        except (vault.VaultError, OSError) as e:
            return {"error": str(e)}
        core.log("Vault password changed")
        return True

    def _vault_loop(self):
        """Wipes copies left by a crash, then locks the vault after a few quiet minutes."""
        vault.wipe_leftovers()
        while True:
            time.sleep(15)
            v = self._vault
            idle = time.monotonic() - self._vault_used
            if v and v.unlocked and idle > 60 * self._state.cfg.get("vault_lock_minutes", 5):
                v.lock()
                core.log("Vault locked (not used for a while)")
                if self._jobs:
                    self._jobs.push("vault", self.vault_state())

    # ---------------------------------------------------------------- disk space map
    def disk_drives(self):
        return diskmap.drives()

    def disk_pick(self):
        return self._ask_folder(Path.home())

    def disk_scan(self, path):
        if not path or not Path(path).is_dir():
            return {"error": "That folder doesn't exist."}
        self._disk = None

        def work(report, cancelled):
            root, top, stats = diskmap.scan(path, progress=lambda n, current: report(None, f"{n:,} items · {current[-70:]}"),
                                            cancelled=cancelled)
            self._disk = (root, top)
            self._jobs.push("disk", self.disk_view(root.name))
            return []
        return self._jobs.start("disk", f"Scan {Path(path).name or path}", work, notify=False)

    def disk_view(self, path=None):
        if not self._disk:
            return None
        root, top = self._disk
        node = diskmap.find(root, path) if path else root
        v = diskmap.view(node or root)
        v["root"] = root.name
        v["top"] = [{"name": Path(p).name, "path": p, "size": s, "protected": diskmap.protected(p)} for s, p in top][:40] if (node or root) is root else []
        for item in v["items"]:
            item["protected"] = bool(item["path"]) and diskmap.protected(item["path"])
        return v

    def _in_scan(self, path):
        return bool(self._disk and path and diskmap.find(self._disk[0], str(Path(path).parent)) is not None)

    def disk_reveal(self, path):
        if self._in_scan(path) and os.path.lexists(path):
            subprocess.Popen(["explorer", "/select,", os.path.normpath(path)])

    def disk_trash(self, path):
        """Recycle Bin only (it can be restored), only for something from the current scan, never system folders."""
        if not self._in_scan(path):
            return {"error": "Scan again first."}
        root, top = self._disk
        node = diskmap.find(root, path)
        size = node.size if node else (os.path.getsize(path) if os.path.isfile(path) else 0)
        try:
            diskmap.to_recycle_bin(path)
        except (OSError, PermissionError) as e:
            return {"error": str(e)}
        core.log(f"Disk map: moved to the Recycle Bin: {path}")
        diskmap.remove_from_tree(root, path, size)
        self._disk = (root, [t for t in top if t[1] != path and not t[1].startswith(path.rstrip("\\") + "\\")])
        return self.disk_view(str(Path(path).parent))

    # ---------------------------------------------------------------- network scanner
    def _net_memory(self):
        return netscan.Memory(core.CONFIG_DIR / "network.json")

    def _net_scan(self, report=None, cancelled=None):
        with self._net_lock:  # the background check and the button never run at once
            result = netscan.scan(progress=(lambda f, n: report(f, f"{n} found")) if report else None, cancelled=cancelled)
            new = self._net_memory().merge(result)
            self._net = result
        for d in new:
            core.log(f"Network: new device {d['ip']} {d['mac']} {d['name'] or ''}")
        if new:
            names = ", ".join(d["name"] or d["ip"] for d in new[:3])
            self._state.notifier.show_soon("New device on your network", names + (f" and {len(new) - 3} more" if len(new) > 3 else ""))
        if self._jobs:
            self._jobs.push("net", self.net_state())
        return result

    def net_state(self):
        return {"result": self._net, "watch": bool(self._state.cfg.get("net_watch", False))}

    def net_scan(self):
        def work(report, cancelled):
            self._net_scan(report, cancelled)
            return []
        return self._jobs.start("net", "Scan the home network", work, notify=False)

    def net_label(self, key, text):
        if not self._net or not self._net_memory().label(self._net, str(key), str(text)):
            return None
        for d in self._net["devices"]:
            if d["key"] == key:
                d["label"], d["new"] = str(text)[:60], False
        return self.net_state()

    def net_ack(self):
        if self._net:
            self._net_memory().acknowledge(self._net)
            for d in self._net["devices"]:
                d["new"] = False
        return self.net_state()

    def net_forget(self, key):
        if self._net:
            self._net_memory().forget(self._net, str(key))
            self._net["missing"] = [m for m in self._net.get("missing", []) if m["key"] != key]
        return self.net_state()

    def _net_loop(self):
        """With 'tell me about new devices' on: a quiet scan every 10 minutes."""
        while True:
            time.sleep(600)
            if self._state.cfg.get("net_watch") and self._state.cfg["features"].get("netscan"):
                try:
                    self._net_scan()
                except Exception as e:
                    core.log(f"Network check skipped: {e}")

    # ---------------------------------------------------------------- is my PC exposed?
    def exposure_check(self):
        cfg = self._state.cfg
        app = {"server_port": cfg["port"] if self._server else None, "https_only": cfg.get("https_only"),
               "remote": cfg["features"].get("remote"), "remote_power": cfg.get("remote_power"),
               "share_links": sum(1 for s in self._shares.summary(None) if s["active"])}

        def work(report, cancelled):
            self._exposure = exposure.check(app)
            self._jobs.push("exposure", self._exposure_view())
            return []
        return self._jobs.start("exposure", "Check how exposed this PC is", work, notify=False)

    def _exposure_view(self):
        if not self._exposure:
            return None
        view = dict(self._exposure)
        if view.get("upnp"):  # the window never gets (or sends back) the router's control address
            view["upnp"] = {"forwards": view["upnp"]["forwards"]}
        return view

    def exposure_state(self):
        return self._exposure_view()

    def exposure_fix(self, action, args=None):
        """Only the fixes the check offered; Windows asks for admin for each one."""
        offered = {f["fix"]["action"]: f["fix"]["args"] for f in (self._exposure or {}).get("findings", []) if f.get("fix")}
        if action not in offered or action == "page":
            return {"error": "Run the check again first."}
        try:
            exe, params = exposure.fix_command(action, offered[action])
        except ValueError as e:
            return {"error": str(e)}
        core.log(f"Exposure fix requested: {action}")
        if not run_elevated(exe, params):
            return {"error": "Windows didn't get permission, so nothing was changed."}
        return True

    def exposure_unforward(self, external, protocol):
        """Asks the router to close one forwarded port (UPnP). Only ports it just listed."""
        upnp = (self._exposure or {}).get("upnp")
        match = next((f for f in (upnp or {}).get("forwards", []) if f["external"] == int(external) and f["protocol"] == protocol), None)
        if not match:
            return {"error": "Run the check again first."}
        try:
            exposure.remove_forward(upnp["control"], upnp["service"], self._exposure["gateway"], match["external"], match["protocol"], match["remote"])
        except Exception as e:
            return {"error": f"The router refused ({e.__class__.__name__}). Remove it in the router's settings instead."}
        core.log(f"Asked the router to close port {external}/{protocol}")
        upnp["forwards"].remove(match)
        return self._exposure_view()

    # ---------------------------------------------------------------- downloader
    def video_info(self, url):
        try:
            return downloader.get_info(url.strip())
        except Exception as e:
            return {"error": _friendly_error(e)}

    def download(self, url, choice, playlist=False, video_title=None):
        """Asks where to save (file name for one video, folder for a playlist), then downloads.
        Returns the job id, or None if the user cancelled the dialog."""
        if choice not in downloader.CHOICES:
            return None
        chosen = self._state.cfg.get("download_dir")
        default_dir = Path(chosen) if chosen else self._state.save_dir / ("Audio" if choice in ("mp3", "m4a") else "Videos")
        default_dir.mkdir(parents=True, exist_ok=True)
        ext = downloader.extension(choice)
        name = None
        if playlist:
            folder = self._ask_folder(default_dir)
            if not folder:
                return None
        else:
            suggested = core.safe_name(video_title or "video")
            out = self._ask_save(default_dir, f"{suggested}.{ext}", ext)
            if not out:
                return None
            out = Path(out)
            folder, name = out.parent, (out.stem if out.suffix.lower() == "." + ext else out.name)
        title = f"Download · {downloader.CHOICES[choice]['label']}"

        def work(report, cancelled):
            return downloader.download(url.strip(), choice, folder, bool(playlist),
                                       progress=lambda f, t: report(f, t), cancelled=cancelled, name=name)
        return self._jobs.start("download", title, work)

    # ---------------------------------------------------------------- speed test & ad blocker test
    def speed_test(self):
        """Runs in the background; live numbers arrive as "speed" events, the result as a "job"."""
        def work(report, cancelled):
            result = speedtest.run(
                progress=lambda phase, value, fraction: self._jobs.push("speed", {"phase": phase, "value": value, "fraction": fraction}),
                cancelled=cancelled)
            self._jobs.push("speed-result", result)
            return []
        return self._jobs.start("speed", "Speed test", work, notify=False)

    def adblock_test(self):
        return adblock.run(progress=lambda done, total: self._jobs.push("adblock-progress", {"done": done, "total": total}))

    # ---------------------------------------------------------------- currency
    def currency_status(self, refresh=False):
        return self._rates.refresh(force=bool(refresh))

    def currency_convert(self, amount, src, dst):
        try:
            return self._rates.convert(float(amount), src, dst)
        except (ValueError, TypeError):
            return {"error": "Enter a number"}

    # ---------------------------------------------------------------- calculator
    def calc(self, method, args=None):
        if method not in CALC_METHODS:
            return {"error": "Unknown"}
        result = getattr(self._calc, method)(**(args or {}))
        if method == "set_mode" and (args or {}).get("angle"):
            self._state.cfg["calc_angle"] = args["angle"]
            core.save_config(self._state.cfg)
        return result

    def calc_convert(self, value, conv_id, reverse=False):
        try:
            return calc.convert_unit(value, conv_id, reverse)
        except (calc.CalcError, ValueError) as e:
            return {"error": str(e) or "Math ERROR"}


def _kb(n):
    return f"{n / 1024:.0f} KB" if n < 1024 * 1024 else f"{n / 1024 / 1024:.1f} MB"


def run_window(state, api, minimized):
    import webview
    from webview.dom import DOMEventHandler

    window = webview.create_window(
        core.APP, url=str(core.WEB_DIR / "desktop.html"), js_api=api,
        width=1240, height=860, min_size=(1040, 660), background_color="#07070d",
        minimized=minimized)
    api._window = window

    def push(event, data):
        window.evaluate_js(f"window.onCore && window.onCore({json.dumps(event)}, {json.dumps(data)})")
    state.listeners.append(push)
    api._jobs = Jobs(push, state.notifier)

    def on_drop(e):
        paths = [f.get("pywebviewFullPath") for f in e.get("dataTransfer", {}).get("files", [])]
        files = [file_info(p) for p in paths if p and os.path.isfile(p)]
        folders = [p for p in paths if p and os.path.isdir(p)]
        if folders:
            push("dropped-all", {"files": files, "folders": folders})  # only the Share page takes folders
        elif files:
            push("dropped", files)  # the page decides what to do based on the open tool

    def on_loaded():
        # Dropped files only carry their full path to Python, never into the page.
        window.dom.document.events.dragover += DOMEventHandler(lambda e: None, True, True)
        window.dom.document.events.drop += DOMEventHandler(on_drop, True, True)

    window.events.loaded += on_loaded
    webview.start(icon=str(core.WEB_DIR / "icon.ico"), private_mode=True)


def run_elevated(exe, params, show=False, directory=None):
    """Start a program as administrator (Windows asks). Returns False if the user said no.
    show: give it a visible window. directory: its current folder (never leave it in ours, or that
    folder counts as in use and Setup can't replace it)."""
    from ctypes import wintypes

    class SHELLEXECUTEINFO(ctypes.Structure):
        _fields_ = [("cbSize", wintypes.DWORD), ("fMask", ctypes.c_ulong), ("hwnd", wintypes.HWND),
                    ("lpVerb", wintypes.LPCWSTR), ("lpFile", wintypes.LPCWSTR), ("lpParameters", wintypes.LPCWSTR),
                    ("lpDirectory", wintypes.LPCWSTR), ("nShow", ctypes.c_int), ("hInstApp", wintypes.HINSTANCE),
                    ("lpIDList", ctypes.c_void_p), ("lpClass", wintypes.LPCWSTR), ("hkeyClass", wintypes.HKEY),
                    ("dwHotKey", wintypes.DWORD), ("hIcon", wintypes.HANDLE), ("hProcess", wintypes.HANDLE)]
    info = SHELLEXECUTEINFO(cbSize=ctypes.sizeof(SHELLEXECUTEINFO), fMask=0x40, lpVerb="runas",
                            lpFile=exe, lpParameters=params, lpDirectory=str(directory or os.environ.get("TEMP") or Path.home()),
                            nShow=1 if show else 0)
    return bool(ctypes.windll.shell32.ShellExecuteExW(ctypes.byref(info)))


class UninstallApi:
    """The window behind "Swiftlane.exe --uninstall" (Settings > Apps > Uninstall)."""

    def __init__(self):
        self._window = None
        self._cfg = {}
        try:
            self._cfg = json.loads(core.CONFIG_PATH.read_text("utf-8"))
        except (OSError, ValueError):
            pass

    def _install_dir(self):
        return Path(sys.executable).parent if getattr(sys, "frozen", False) else None

    def info(self):
        files = core.created_files()
        return {"install_dir": str(self._install_dir() or "(not installed: running from source)"),
                "config_dir": str(core.CONFIG_DIR), "files": len(files),
                "files_size": sum(os.path.getsize(f) for f in files), "language": self._cfg.get("language", "en")}

    def run(self, remove_files):
        install_dir = self._install_dir()
        # 1. Machine-wide parts need admin rights: ask first, so "no" changes nothing.
        if install_dir and (install_dir / "uninstall.ps1").exists():
            ok = run_elevated("powershell.exe", f'-NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File '
                                                f'"{install_dir / "uninstall.ps1"}" -WaitPid {os.getpid()} -InstallDir "{install_dir}"')
            if not ok:
                return {"error": "Windows didn’t give permission, so nothing was removed."}
        # 2. This user's parts (the elevated script waits until this window has closed).
        subprocess.run(["taskkill", "/F", "/IM", f"{core.APP}.exe", "/FI", f"PID ne {os.getpid()}"],
                       capture_output=True, creationflags=subprocess.CREATE_NO_WINDOW)
        try:  # close Tailscale Funnel if Swiftlane had it open for share links (only Swiftlane's own entry)
            share.Funnel(share.SHARE_PORT).off()
        except Exception:
            pass
        deleted = 0
        if remove_files:
            for f in core.created_files():
                try:
                    os.remove(f)
                    deleted += 1
                except OSError:
                    pass
            save_dir = Path(self._cfg.get("save_dir") or core.DEFAULTS["save_dir"])
            for folder in [save_dir / c for c in (*core.CATEGORIES, core.OTHER_CATEGORY)] + [save_dir]:
                try:
                    folder.rmdir()  # only if empty: never removes anything you put there yourself
                except OSError:
                    pass
        import winreg
        for name in (core.APP, core.OLD_APP):
            try:
                with winreg.OpenKey(winreg.HKEY_CURRENT_USER, RUN_KEY, 0, winreg.KEY_SET_VALUE) as k:
                    winreg.DeleteValue(k, name)
            except OSError:
                pass
        from setup_paths import desktop_shortcut
        desktop_shortcut().unlink(missing_ok=True)
        try:  # the right-click menu entries
            import context_menu
            context_menu.disable()
        except Exception:
            pass
        for leftover in (core.CONFIG_DIR, core.APPDATA / core.OLD_APP, Path.home() / ".cache" / "yt-dlp"):
            shutil.rmtree(leftover, ignore_errors=True)
        return {"ok": True, "deleted_files": deleted}

    def close(self):
        self._window.destroy()


def run_uninstaller():
    import webview
    api = UninstallApi()
    window = webview.create_window(f"Uninstall {core.APP}", url=str(core.WEB_DIR / "uninstall.html"), js_api=api,
                                   width=720, height=640, background_color="#07070d")
    api._window = window
    webview.start(private_mode=True, icon=str(core.WEB_DIR / "icon.ico"))


def run_headless(state):
    port = state.cfg["port"]
    print(f"\n  {core.APP} {core.VERSION} (headless). Pairing code: {core.pretty_code(state.code)}")
    for label, ip in core.local_addresses():
        print(f"  [{label}] http://{ip}:{port}/#{state.code}")
    print("  Press Ctrl+C to stop.\n")
    try:
        threading.Event().wait()
    except KeyboardInterrupt:
        pass


def selftest(out_file):
    """Checks that every tool's parts made it into the build. Writes a report and exits."""
    import tempfile
    lines = []

    def step(name, fn):
        try:
            lines.append(f"PASS {name}: {fn()}")
        except Exception as e:
            lines.append(f"FAIL {name}: {e.__class__.__name__}: {e}")

    tmp = Path(tempfile.mkdtemp())
    step("ffmpeg", lambda: subprocess.run([convert.ffmpeg_exe(), "-version"], capture_output=True, text=True,
                                          **convert.no_window()).stdout.split("\n")[0])
    step("calculator", lambda: calc.Calculator().evaluate("sin(45)+∫(x^2,0,3)")["decimal"])

    def image_and_pdf():
        from PIL import Image
        Image.new("RGB", (40, 30), (255, 0, 0)).save(tmp / "a.png")
        pdf = convert.convert_file(tmp / "a.png", "pdf")[0]
        return [p.name for p in convert.convert_file(pdf, "png")]
    step("image + pdf", image_and_pdf)
    step("image editor preview", lambda: images.render_preview(tmp / "a.png", {"straighten": 5, "crop_box": [0, 0, .5, .5],
                                                                               "adjust": {"contrast": 1.3}})["width"])
    step("heic reader", lambda: __import__("pillow_heif").__version__)
    step("video", lambda: convert.convert_file(_test_clip(tmp), "mp3")[0].name)
    step("yt-dlp + YouTube", lambda: downloader.get_info("https://www.youtube.com/watch?v=jNQXAC9IVRw")["title"])
    step("currency", lambda: len(currency.Rates(tmp / "r.json").refresh()["currencies"]))
    step("qr", lambda: bool(core.qr_svg("x")))
    step("speed test server", lambda: speedtest.server_info()["city"])
    step("ad blocker test", lambda: "ok" if not adblock.check_domain("www.wikipedia.org", set())["blocked"] else "wrong")
    def share_server():
        from http.client import HTTPConnection
        sh = share.Shares(tmp / "shares.json")
        srv = share.ShareServer(0, sh)  # any free port
        threading.Thread(target=srv.serve_forever, daemon=True).start()
        try:
            conn = HTTPConnection("127.0.0.1", srv.server_address[1], timeout=5)
            conn.request("GET", "/", headers={"Host": f"127.0.0.1:{srv.server_address[1]}"})
            status = conn.getresponse().status
        finally:
            srv.shutdown(); srv.server_close()
        return f"answers {status}, password hashing {'ok' if share.check_password('x', share.hash_password('x')) else 'broken'}"
    step("share links server", share_server)
    step("uninstaller page", lambda: (core.WEB_DIR / "uninstall.html").exists() and (core.WEB_DIR / "js" / "i18n.js").exists())
    Path(out_file).write_text("\n".join(lines), "utf-8")


def _test_clip(folder):
    out = folder / "clip.mp4"
    subprocess.run([convert.ffmpeg_exe(), "-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i",
                    "sine=frequency=440:duration=1", "-f", "lavfi", "-i", "color=c=blue:s=64x48:d=1",
                    "-shortest", str(out)], check=True, **convert.no_window())
    return out


def main():
    if len(sys.argv) >= 2 and sys.argv[1] == "--uninstall":
        run_uninstaller()
        return
    if len(sys.argv) == 3 and sys.argv[1] == "--selftest":
        selftest(sys.argv[2])
        return
    parser = argparse.ArgumentParser(description="Send files between your phone and this PC, plus everyday tools.")
    parser.add_argument("--folder", help="where received files are saved (remembered)")
    parser.add_argument("--port", type=int, help="port to listen on (remembered)")
    parser.add_argument("--new-code", action="store_true", help="make a new pairing code")
    parser.add_argument("--minimized", action="store_true", help="start minimized")
    parser.add_argument("--headless", action="store_true", help="server only, no window")
    parser.add_argument("--page", help="open on this page, e.g. calculator")
    for flag in ("share", "send", "convert"):  # from Explorer's right-click menu
        parser.add_argument(f"--{flag}", nargs="+", metavar="PATH", help=argparse.SUPPRESS)
    args = parser.parse_args()
    if sys.stdout:
        sys.stdout.reconfigure(encoding="utf-8", errors="replace")

    explorer = next(((c, getattr(args, c)) for c in ("share", "send", "convert") if getattr(args, c)), None)
    if not args.headless and already_running():
        if explorer:  # hand the files to the Swiftlane that's already open
            import ipc
            ipc.send(core.CONFIG_DIR, *explorer)
            focus_existing_window()
            return
        if not focus_existing_window():
            message_box(f"{core.APP} is already running. Check the taskbar.")
        return

    cfg = core.load_config(args.folder, args.port, args.new_code)
    state = core.State(cfg)
    if args.headless:
        try:
            core.start_server(state)
        except core.PortInUse as e:
            print(f"Port {e.args[0]} is already in use.")
            sys.exit(1)
        run_headless(state)
        return

    api = Api(state)
    api._start_page = args.page or cfg.pop("_open_page", None)
    import ipc
    ipc.serve(core.CONFIG_DIR, api._from_explorer)
    if explorer:
        api._from_explorer(*explorer)
    if cfg["features"]["transfer"]:
        api._start_server()
    threading.Thread(target=api._share_loop, daemon=True).start()
    threading.Thread(target=api._update_loop, daemon=True).start()
    threading.Thread(target=api._vault_loop, daemon=True).start()
    threading.Thread(target=api._net_loop, daemon=True).start()
    run_window(state, api, args.minimized)
    if api._vault:
        api._vault.lock()  # also wipes files opened from it
    api._shutdown_sharing()


if __name__ == "__main__":
    try:
        main()
    except SystemExit:
        raise
    except Exception:
        details = traceback.format_exc()
        core.log(details)
        message_box(f"{core.APP} hit an error and stopped:\n\n{details[-1500:]}\n"
                    f"Details are in {core.LOG_PATH}")
        sys.exit(1)
