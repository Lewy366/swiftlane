"""Version builder & consistency checker for Swiftlane (Rust).

Usage:
  python Rust/pack/set-version.py                    # show versions across all files
  python Rust/pack/set-version.py --check            # fail if Rust Cargo.toml / Cargo.lock / tauri.conf.json disagree
  python Rust/pack/set-version.py --check-all        # also require Source/server.py VERSION to match
  python Rust/pack/set-version.py 2.0.1              # set Rust version (Cargo.toml, Cargo.lock, tauri.conf.json)
  python Rust/pack/set-version.py 2.0.1 --sync-all   # also update Source/server.py VERSION (used by Publish update.cmd)
"""
import argparse
import json
import re
import sys
from pathlib import Path

SEMVER_RE = re.compile(r"^\d+\.\d+\.\d+$")


def repo_root(override: str | None = None) -> Path:
    if override:
        return Path(override).resolve()
    return Path(__file__).resolve().parent.parent.parent


def read_cargo_toml_version(path: Path) -> str:
    text = path.read_text("utf-8")
    m = re.search(r'(?ms)^\[package\]\s.*?^version\s*=\s*"([^"]+)"', text)
    if not m:
        raise RuntimeError(f"Couldn't find [package] version in {path}")
    return m.group(1)


def write_cargo_toml_version(path: Path, new_version: str) -> None:
    text = path.read_text("utf-8")
    pkg = re.search(r'(?ms)(^\[package\]\s.*?^version\s*=\s*")([^"]+)(")', text)
    if not pkg:
        raise RuntimeError(f"Couldn't update [package] version in {path}")
    updated = text[:pkg.start(2)] + new_version + text[pkg.end(2):]
    path.write_text(updated, "utf-8", newline="")


def read_cargo_lock_version(path: Path) -> str:
    text = path.read_text("utf-8")
    m = re.search(r'(?ms)^\[\[package\]\]\s*\r?\nname\s*=\s*"swiftlane"\s*\r?\nversion\s*=\s*"([^"]+)"', text)
    if not m:
        raise RuntimeError(f"Couldn't find swiftlane package version in {path}")
    return m.group(1)


def write_cargo_lock_version(path: Path, new_version: str) -> None:
    text = path.read_text("utf-8")
    m = re.search(r'(?ms)(^\[\[package\]\]\s*\r?\nname\s*=\s*"swiftlane"\s*\r?\nversion\s*=\s*")([^"]+)(")', text)
    if not m:
        raise RuntimeError(f"Couldn't update swiftlane package version in {path}")
    updated = text[:m.start(2)] + new_version + text[m.end(2):]
    path.write_text(updated, "utf-8", newline="")


def read_tauri_conf_version(path: Path) -> str:
    data = json.loads(path.read_text("utf-8"))
    ver = data.get("version")
    if not isinstance(ver, str) or not ver:
        raise RuntimeError(f"Couldn't find version in {path}")
    return ver


def write_tauri_conf_version(path: Path, new_version: str) -> None:
    text = path.read_text("utf-8")
    m = re.search(r'("version"\s*:\s*")([^"]+)(")', text)
    if not m:
        raise RuntimeError(f"Couldn't update version in {path}")
    updated = text[:m.start(2)] + new_version + text[m.end(2):]
    # Validate resulting JSON before writing
    parsed = json.loads(updated)
    if parsed.get("version") != new_version:
        raise RuntimeError(f"Version replacement verification failed for {path}")
    path.write_text(updated, "utf-8", newline="")


def read_server_py_version(path: Path) -> str | None:
    if not path.is_file():
        return None
    m = re.search(r'^VERSION\s*=\s*"([^"]+)"', path.read_text("utf-8"), re.M)
    return m.group(1) if m else None


def write_server_py_version(path: Path, new_version: str) -> None:
    text = path.read_text("utf-8")
    m = re.search(r'(^VERSION\s*=\s*")([^"]+)(")', text, re.M)
    if not m:
        raise RuntimeError(f"Couldn't update VERSION in {path}")
    updated = text[:m.start(2)] + new_version + text[m.end(2):]
    path.write_text(updated, "utf-8", newline="")


def inspect(root: Path) -> dict[str, str | None]:
    return {
        "Rust/Cargo.toml": read_cargo_toml_version(root / "Rust" / "Cargo.toml"),
        "Rust/Cargo.lock": read_cargo_lock_version(root / "Rust" / "Cargo.lock"),
        "Rust/tauri.conf.json": read_tauri_conf_version(root / "Rust" / "tauri.conf.json"),
        "Source/server.py": read_server_py_version(root / "Source" / "server.py"),
    }


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description="Show, check, or bump Swiftlane's version.")
    ap.add_argument("version", nargs="?", help="New X.Y.Z version to write (omit to inspect/check).")
    ap.add_argument("--check", action="store_true", help="Exit non-zero if Rust version files disagree.")
    ap.add_argument("--check-all", action="store_true", help="Exit non-zero if Rust files or Source/server.py disagree.")
    ap.add_argument("--sync-all", action="store_true", help="Also update Source/server.py VERSION when setting a version.")
    ap.add_argument("--root", help="Override repository root directory (for testing).")
    args = ap.parse_args(argv)

    root = repo_root(args.root)

    if args.version:
        if not SEMVER_RE.match(args.version):
            sys.exit(f"Invalid version '{args.version}': expected X.Y.Z (digits only, e.g. 2.0.0).")
        write_cargo_toml_version(root / "Rust" / "Cargo.toml", args.version)
        write_cargo_lock_version(root / "Rust" / "Cargo.lock", args.version)
        write_tauri_conf_version(root / "Rust" / "tauri.conf.json", args.version)
        if args.sync_all:
            write_server_py_version(root / "Source" / "server.py", args.version)

    versions = inspect(root)
    for rel, ver in versions.items():
        print(f"{rel:22s} {ver or '(missing)'}")

    rust_vals = {versions["Rust/Cargo.toml"], versions["Rust/Cargo.lock"], versions["Rust/tauri.conf.json"]}
    if len(rust_vals) != 1:
        if args.check or args.check_all:
            print("ERROR: Rust version files do not match.", file=sys.stderr)
            return 1
        print("WARNING: Rust version files do not match.", file=sys.stderr)
    elif args.check_all and versions["Source/server.py"] not in (None, next(iter(rust_vals))):
        print("ERROR: Source/server.py VERSION does not match Rust version.", file=sys.stderr)
        return 1

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
