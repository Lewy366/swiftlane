"""Swiftlane Setup: the install wizard window.

Packed as "Swiftlane Setup.exe" (asks for admin rights when it starts) together with
payload.zip (the built app). The pages live in setup/ui; the install work is installer.py.
"""
import ctypes
import json
import os
import shutil
import subprocess
import sys
import threading
import zipfile
from pathlib import Path

import installer

ROOT = Path(getattr(sys, "_MEIPASS", Path(__file__).resolve().parent.parent))  # contains web/ (pages, styles, translations)
PAYLOAD = Path(os.environ.get("SWIFTLANE_PAYLOAD") or ROOT / "payload.zip")
VERSION = (ROOT / "version.txt").read_text("utf-8").strip() if (ROOT / "version.txt").exists() else "dev"
FEATURES = ("transfer", "share", "remote", "pdf", "trimmer", "vault", "diskmap", "netscan", "exposure", "contacts", "worldclock", "converter", "images", "downloader", "currency", "calculator", "speedtest", "adblock")


def windows_language():
    """en/de/pl/it from the Windows display language."""
    try:
        primary = ctypes.windll.kernel32.GetUserDefaultUILanguage() & 0x3FF
    except Exception:
        return "en"
    return {0x07: "de", 0x15: "pl", 0x10: "it"}.get(primary, "en")


def shell_open(target):
    """Open a file/URL through Explorer, so it runs as the normal user, not as admin."""
    subprocess.Popen(["explorer.exe", str(target)])


class SetupApi:
    def __init__(self):
        self._window = None
        self._system = installer.WindowsSystem()
        self._result = None

    def info(self):
        existing = installer.installed_version()
        program_files = Path(os.environ.get("ProgramFiles", r"C:\Program Files"))
        install_dir = Path(existing[1]) if existing and existing[1] else program_files / installer.APP
        cfg = {}
        try:  # prefill from this user's current settings (update)
            cfg = json.loads((Path(os.environ["APPDATA"]) / installer.APP / "config.json").read_text("utf-8"))
        except (OSError, ValueError, KeyError):
            pass
        documents = installer.known_folder(5)
        try:
            with zipfile.ZipFile(PAYLOAD) as z:
                needed = sum(i.file_size for i in z.infolist())
        except OSError:
            needed = 0
        drive = Path(install_dir.anchor or "C:\\")
        try:
            networks = self._system.networks()
        except Exception:
            networks = []
        return {
            "version": VERSION,
            "existing": {"version": existing[0], "dir": existing[1]} if existing else None,
            "install_dir": str(install_dir),
            "needed": needed, "free": shutil.disk_usage(drive).free,
            "language": cfg.get("language") or windows_language(),
            "save_dir": cfg.get("save_dir") or str(documents / installer.APP),
            "sort_files": cfg.get("sort_files", True),
            "download_dir": cfg.get("download_dir", ""),
            "calc_angle": cfg.get("calc_angle", "deg"),
            "notify": cfg.get("notify", True),
            "features": {f: cfg.get("features", {}).get(f, f != "remote") for f in FEATURES},  # the remote starts off
            "theme": {"accent": "cyan", "custom": "#14b8e0", **cfg.get("theme", {})},
            "networks": networks,
            "node": bool(shutil.which("node")),
            "autostart": self._system.read_value(__import__("winreg").HKEY_CURRENT_USER, installer.RUN_KEY, installer.APP) is not None,
            "desktop": (installer.desktop_dir() / f"{installer.APP}.lnk").exists() or not existing,
            "start_menu": (installer.start_menu_dir() / f"{installer.APP}.lnk").exists() or not existing,
            # started by Swiftlane's Update button: keep every choice, show progress, reopen the app
            "update": "--update" in sys.argv and bool(existing),
        }

    def pick_folder(self, start=""):
        import webview
        picked = self._window.create_file_dialog(webview.FileDialog.FOLDER, directory=start or "")
        return (picked[0] if isinstance(picked, (list, tuple)) else picked) if picked else None

    def open_url(self, url):
        if url in ("https://nodejs.org/en/download",):
            shell_open(url)

    def open_log(self):
        if installer.LOG_FILE.exists():
            shell_open(installer.LOG_FILE)

    def install(self, choices):
        choices = dict(choices)
        choices["version"] = VERSION
        installer.write_log(f"Swiftlane Setup {VERSION} starting. Choices: " +
                            json.dumps({k: v for k, v in choices.items() if k != "theme"}))

        def progress(fraction, text):
            self._push("progress", {"fraction": fraction, "text": text})

        def run():
            try:
                self._result = installer.install(PAYLOAD, choices, system=self._system, progress=progress,
                                                 fail_at=os.environ.get("SWIFTLANE_SETUP_FAIL_AT"))
                self._push("installed", self._result)
            except Exception as e:
                installer.write_log(f"Error: {e}")
                self._push("failed", {"error": str(e)})
        threading.Thread(target=run, daemon=True).start()

    def finish(self, launch, pair):
        if self._result and launch:
            exe = Path(self._result["exe"])
            if pair:  # open on "Connect phone" the first time
                choices_file = exe.parent / "setup-choices.json"
                data = json.loads(choices_file.read_text("utf-8"))
                data["open_page"] = "connect"
                choices_file.write_text(json.dumps(data, indent=2), "utf-8")
            shell_open(exe)
        self._window.destroy()

    def uninstall_existing(self):
        existing = installer.installed_version()
        if existing and existing[1]:
            exe = Path(existing[1]) / f"{installer.APP}.exe"
            if exe.exists():
                subprocess.Popen([str(exe), "--uninstall"])
                self._window.destroy()
                return True
        return False

    def close(self):
        self._window.destroy()

    def _push(self, event, data):
        self._window.evaluate_js(f"window.onSetup && window.onSetup({json.dumps(event)}, {json.dumps(data)})")


RESTARTED = "SWIFTLANE_SETUP_RESTARTED"


def restart_for_update():
    """Started by Swiftlane's Update button, Setup inherits two things that break the update: Swiftlane's
    folder as its current folder (Windows then won't move that folder: "in use") and, from older versions,
    a hidden window. Starting a fresh copy of Setup from the Temp folder drops both. Returns True if the
    fresh copy is running and this one should just exit."""
    if not getattr(sys, "frozen", False) or "--update" not in sys.argv or os.environ.get(RESTARTED):
        return False
    env = {**os.environ, RESTARTED: "1", "PYINSTALLER_RESET_ENVIRONMENT": "1"}  # a new, independent Setup
    try:
        subprocess.Popen([sys.executable, *sys.argv[1:]], cwd=os.environ.get("TEMP") or os.path.dirname(sys.executable),
                         env=env, close_fds=True)
    except OSError as e:
        installer.write_log(f"Couldn't restart Setup ({e}); carrying on")
        return False
    return True


def main():
    import webview
    if restart_for_update():
        return
    try:  # never stand in a folder Setup may have to move
        os.chdir(os.environ.get("TEMP") or os.path.dirname(sys.executable))
    except OSError:
        pass
    try:  # each run of Setup starts its own log
        installer.LOG_FILE.unlink(missing_ok=True)
    except OSError:
        pass
    api = SetupApi()
    title = "Updating Swiftlane" if "--update" in sys.argv else "Swiftlane Setup"
    window = webview.create_window(title, url=str(ROOT / "web" / "setup.html"), js_api=api,
                                   width=920, height=660, resizable=False, background_color="#07070d")
    api._window = window
    webview.start(private_mode=True, icon=str(ROOT / "web" / "icon.ico"))


if __name__ == "__main__":
    main()
