"""Updates from GitHub Releases, installed only if signed with the owner's key.

Every release on github.com/Lewy366/swiftlane carries three files:
  manifest.json      version, what's new, the setup's size and SHA-256
  manifest.sig       Ed25519 signature of manifest.json (hex), made with the owner's private key
  Swiftlane-Setup.exe
Swiftlane trusts nothing from GitHub itself: the manifest must verify against PUBLIC_KEY below,
and the downloaded setup must match the manifest's size and SHA-256 (checked again right before
it starts). Nothing installs without the user clicking Update, and Windows asks for admin.
The only thing sent is an ordinary request for the latest release (GitHub sees the IP address).
"""
import hashlib
import json
import os
import re
import secrets
import tempfile
import urllib.request
from pathlib import Path

REPO = "Lewy366/swiftlane"
LATEST = os.environ.get("SWIFTLANE_UPDATE_URL") or f"https://api.github.com/repos/{REPO}/releases/latest"
PUBLIC_KEY = "67ad5096eec22a2fe4fb1290f7f3f199150d319a2b45409ae836bf14276fb0b2"
SETUP_NAME = "Swiftlane-Setup.exe"
MAX_SETUP = 400 * 1024 * 1024
CHUNK = 256 * 1024


class UpdateError(Exception):
    """Shown to the user as it is."""


def version_tuple(v):
    parts = re.findall(r"\d+", str(v))[:3]
    return tuple(int(p) for p in parts) + (0,) * (3 - len(parts))


def verify(manifest_bytes, sig_hex, public_key=None):
    from cryptography.exceptions import InvalidSignature
    from cryptography.hazmat.primitives.asymmetric import ed25519
    key = ed25519.Ed25519PublicKey.from_public_bytes(bytes.fromhex(public_key or PUBLIC_KEY))
    try:
        key.verify(bytes.fromhex(sig_hex.strip()), manifest_bytes)
    except (InvalidSignature, ValueError):
        raise UpdateError("The update isn't signed by Swiftlane's owner, so it was rejected.")


def _open(url, timeout=20):
    if not url.startswith(("https://", "http://127.0.0.1")):
        raise UpdateError("Unexpected update address.")
    req = urllib.request.Request(url, headers={"User-Agent": "Swiftlane-updater", "Accept": "application/vnd.github+json"})
    return urllib.request.urlopen(req, timeout=timeout)


def _get(url, limit):
    with _open(url) as r:
        data = r.read(limit + 1)
    if len(data) > limit:
        raise UpdateError("The update information is unexpectedly large.")
    return data


def check(current, public_key=None):
    """Latest release, verified. Returns {available, version, notes, size, ...}; raises UpdateError."""
    try:
        release = json.loads(_get(LATEST, 2 * 1024 * 1024))
    except UpdateError:
        raise
    except OSError:
        raise UpdateError("Couldn't reach GitHub. Check your internet connection.")
    except ValueError:
        raise UpdateError("GitHub sent something unexpected.")
    assets = {a.get("name"): a.get("browser_download_url") for a in release.get("assets", [])}
    if "manifest.json" not in assets or "manifest.sig" not in assets:
        raise UpdateError("The latest release is missing its signature.")
    manifest_bytes = _get(assets["manifest.json"], 256 * 1024)
    sig = _get(assets["manifest.sig"], 4096).decode("ascii", "replace")
    verify(manifest_bytes, sig, public_key)
    m = json.loads(manifest_bytes)
    if m.get("app") != "Swiftlane" or m.get("file") not in assets or not re.fullmatch(r"[0-9a-f]{64}", m.get("sha256", "")):
        raise UpdateError("The update information is incomplete.")
    if not 0 < int(m.get("size", 0)) <= MAX_SETUP:
        raise UpdateError("The update has an unexpected size.")
    return {"available": version_tuple(m["version"]) > version_tuple(current), "version": m["version"],
            "notes": str(m.get("notes", ""))[:4000], "size": int(m["size"]), "published": m.get("published", ""),
            "manifest": m, "url": assets[m["file"]]}


def download(info, progress=None, cancelled=None):
    """Download the setup into a fresh private temp folder and check it. Returns its path."""
    m = info["manifest"]
    folder = Path(tempfile.gettempdir()) / f"Swiftlane-update-{secrets.token_hex(6)}"
    folder.mkdir()
    path = folder / "Swiftlane Setup.exe"
    h, done = hashlib.sha256(), 0
    try:
        with _open(info["url"], timeout=60) as r, open(path, "xb") as f:
            while True:
                if cancelled and cancelled():
                    raise UpdateError("Update cancelled.")
                chunk = r.read(CHUNK)
                if not chunk:
                    break
                done += len(chunk)
                if done > m["size"]:
                    raise UpdateError("The download is bigger than announced, so it was rejected.")
                h.update(chunk)
                f.write(chunk)
                if progress:
                    progress(done / m["size"])
        if done != m["size"] or h.hexdigest() != m["sha256"]:
            raise UpdateError("The download doesn't match the signed update, so it was rejected.")
    except OSError:
        path.unlink(missing_ok=True)
        raise UpdateError("The download was interrupted. Try again.")
    except UpdateError:
        path.unlink(missing_ok=True)
        raise
    return path


def still_matches(path, info):
    """Re-check right before starting it (nothing may have swapped the file meanwhile)."""
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(CHUNK), b""):
            h.update(chunk)
    return h.hexdigest() == info["manifest"]["sha256"]