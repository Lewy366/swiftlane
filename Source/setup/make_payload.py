"""Packs the built app (Source/dist/Swiftlane) plus uninstall.ps1 into payload.zip for the setup.
Usage: python make_payload.py <source_dir> <out_dir>"""
import re
import sys
import zipfile
from pathlib import Path

src, out = Path(sys.argv[1]), Path(sys.argv[2])
app = src / "dist" / "Swiftlane"
if not (app / "Swiftlane.exe").exists():
    sys.exit("Build the app first (dist/Swiftlane/Swiftlane.exe is missing).")
out.mkdir(parents=True, exist_ok=True)
version = re.search(r'^VERSION = "(.+)"', (src / "server.py").read_text("utf-8"), re.M).group(1)
(out / "version.txt").write_text(version, "utf-8")
with zipfile.ZipFile(out / "payload.zip", "w", zipfile.ZIP_LZMA) as z:
    for f in app.rglob("*"):
        if f.is_file():
            z.write(f, f.relative_to(app))
    z.write(src / "uninstall.ps1", "uninstall.ps1")
size = (out / "payload.zip").stat().st_size
print(f"payload.zip: Swiftlane {version}, {size / 1e6:.0f} MB")
