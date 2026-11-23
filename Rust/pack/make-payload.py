"""Packs a runnable Swiftlane (Rust) folder into payload.zip + version.txt for Swiftlane Setup.

Usage:
  python Rust/pack/make-payload.py <app_dir> <out_dir> [--version X.Y.Z]

Refuses to pack if any required executable, DLL, or helper script is missing or empty,
and skips checksum/scratch files so only runtime files are installed into Program Files.
"""
import argparse
import re
import sys
import zipfile
from pathlib import Path

REQUIRED_FILES = (
    "Swiftlane.exe",
    "ffmpeg.exe",
    "ffprobe.exe",
    "pdfium.dll",
    "yt-dlp.exe",
    "toolkit/calc.py",
    "uninstall.ps1",
    "airplay-install.ps1",
    "licenses/SOURCES.txt",
)

SKIP_NAMES = {"Swiftlane.exe.sha256", "SHA256SUMS.txt"}
NEVER_PACK = re.compile(r"(\.pem|\.key|signing|config\.json|shares\.json|setup-choices\.json|\.log|\.pdb)$", re.I)


def cargo_version(repo_root: Path) -> str:
    cargo = repo_root / "Rust" / "Cargo.toml"
    m = re.search(r'(?ms)^\[package\]\s.*?^version\s*=\s*"([^"]+)"', cargo.read_text("utf-8"))
    if not m:
        raise RuntimeError(f"Couldn't read [package] version from {cargo}")
    return m.group(1)


def pack(app_dir: Path, out_dir: Path, version: str) -> tuple[Path, Path]:
    app_dir = app_dir.resolve()
    out_dir = out_dir.resolve()
    if not app_dir.is_dir():
        raise RuntimeError(f"App folder doesn't exist: {app_dir}")

    missing = []
    for rel in REQUIRED_FILES:
        p = app_dir / rel
        if not p.is_file() or p.stat().st_size == 0:
            missing.append(rel)
    if missing:
        raise RuntimeError("Incomplete Rust app folder; missing or empty: " + ", ".join(missing))

    out_dir.mkdir(parents=True, exist_ok=True)
    ver_path = out_dir / "version.txt"
    zip_path = out_dir / "payload.zip"
    ver_path.write_text(version.strip(), "utf-8")

    files = sorted(f for f in app_dir.rglob("*") if f.is_file())
    count = 0
    with zipfile.ZipFile(zip_path, "w", zipfile.ZIP_LZMA) as z:
        for f in files:
            rel = f.relative_to(app_dir).as_posix()
            if f.name in SKIP_NAMES:
                continue
            if NEVER_PACK.search(rel):
                raise RuntimeError(f"Refusing to pack sensitive/debug file: {rel}")
            z.write(f, rel)
            count += 1

    mb = zip_path.stat().st_size / 1e6
    print(f"payload.zip: Swiftlane {version} (Rust), {count} files, {mb:.1f} MB")
    return zip_path, ver_path


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description="Pack a Rust Swiftlane folder into payload.zip + version.txt.")
    ap.add_argument("app_dir", help="Folder containing Swiftlane.exe and its bundled helpers.")
    ap.add_argument("out_dir", help="Output folder where payload.zip and version.txt will be written.")
    ap.add_argument("--version", help="Override version string (defaults to Rust/Cargo.toml).")
    args = ap.parse_args(argv)

    root = Path(__file__).resolve().parent.parent.parent
    ver = args.version or cargo_version(root)
    if not re.match(r"^\d+\.\d+\.\d+$", ver):
        sys.exit(f"Invalid version '{ver}': expected X.Y.Z.")

    try:
        pack(Path(args.app_dir), Path(args.out_dir), ver)
    except RuntimeError as e:
        sys.exit(str(e))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
