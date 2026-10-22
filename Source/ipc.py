"""Hand files from Explorer's right-click menu to the Swiftlane that is already running.

Explorer starts a second Swiftlane.exe with the files; it passes them to the running one over a
local named pipe and exits. Every connection must prove it knows a random key that only this
Windows account can read (HMAC challenge in multiprocessing.connection), messages are plain
JSON (never pickle), and only a fixed set of commands with existing paths is accepted.
"""
import getpass
import json
import os
import re
import secrets
import threading
import time
from multiprocessing.connection import Client, Listener
from pathlib import Path

COMMANDS = ("share", "send", "convert")
MAX_PATHS = 500


def _address():
    user = re.sub(r"[^A-Za-z0-9_.-]", "_", getpass.getuser())[:60]
    return rf"\\.\pipe\Swiftlane-{user}"


def _key(config_dir):
    path = Path(config_dir) / "ipc.key"
    try:
        return path.read_bytes()
    except OSError:
        key = secrets.token_bytes(32)
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(key)
        return key


def clean(msg):
    """The command and the paths that really exist, or None."""
    if not isinstance(msg, dict) or msg.get("cmd") not in COMMANDS or not isinstance(msg.get("paths"), list):
        return None
    paths = [os.path.abspath(p) for p in msg["paths"][:MAX_PATHS] if isinstance(p, str) and len(p) < 1024 and os.path.exists(p)]
    return {"cmd": msg["cmd"], "paths": paths} if paths else None


def serve(config_dir, handler):
    """Run in the background in the main Swiftlane: handler(cmd, paths) for each valid request."""
    key = _key(config_dir)

    def loop():
        try:
            listener = Listener(_address(), family="AF_PIPE", authkey=key)
        except OSError:
            return
        while True:
            try:
                with listener.accept() as conn:  # fails unless the client knows the key
                    msg = clean(json.loads(conn.recv_bytes(256 * 1024).decode("utf-8")))
                    conn.send_bytes(b"ok" if msg else b"refused")
                if msg:
                    handler(msg["cmd"], msg["paths"])
            except Exception:
                time.sleep(0.2)  # a bad or rejected connection: never spin
                continue
    threading.Thread(target=loop, daemon=True).start()


def send(config_dir, cmd, paths):
    """From the second Swiftlane: True if the running one took the request."""
    try:
        with Client(_address(), family="AF_PIPE", authkey=_key(config_dir)) as conn:
            conn.send_bytes(json.dumps({"cmd": cmd, "paths": list(paths)}).encode("utf-8"))
            return conn.recv_bytes(64) == b"ok"
    except Exception:
        return False
