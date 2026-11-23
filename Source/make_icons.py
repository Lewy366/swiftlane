"""Generates the Swiftlane 2.0 Interlocked SL (Dark Emblem) app icons.

Run after changing the design:
    python Source/make_icons.py

Requires Pillow (no external SVG/font dependencies). Renders exact 256x256
vector geometry at 4x supersampling (4096x4096) and downsamples with LANCZOS.

Outputs:
    Source/web/icon-180.png  (180x180 square, iPhone Home Screen)
    Source/web/icon-512.png  (512x512 square, web manifest)
    Source/web/icon.ico      (multi-size 256/64/48/32/16 rounded, Windows)
    Rust/icons/icon.ico      (copy of rounded icon.ico, Tauri Windows)
    Rust/icons/icon.png      (512x512 rounded PNG, Tauri)
"""
from pathlib import Path
import re
import shutil

from PIL import Image, ImageDraw

SOURCE = Path(__file__).resolve().parent
WEB = SOURCE / "web"
RUST_ICONS = SOURCE.parent / "Rust" / "icons"

VIEWBOX = 256
SIZE = 1024
SUPERSAMPLE = 4
CANVAS = SIZE * SUPERSAMPLE
SCALE = CANVAS / VIEWBOX

BG = (14, 18, 28, 255)                       # #0E121C
RIM = (255, 255, 255, round(255 * 0.14))     # rgba(255, 255, 255, 0.14)
CYAN = (20, 184, 224, 255)                   # #14B8E0
WHITE = (243, 245, 248, 255)                 # #F3F5F8

TILE_RX = 54
RIM_WIDTH = 1

# Exact 256x256 Interlocked SL (Dark Emblem) paths
L_PATH = "M 152 60 H 178 V 166 H 218 L 205 194 H 152 Z"
CUTOUT_RECT = (142, 122, 44, 44, 4)          # x, y, width, height, rx
S_PATH = (
    "M 136 60 H 82 "
    "C 52 60 38 76 38 98 "
    "C 38 120 52 134 82 138 "
    "L 120 143 "
    "C 130 145 134 150 134 157 "
    "C 134 165 126 168 112 168 "
    "H 50 L 38 194 H 114 "
    "C 146 194 162 178 162 155 "
    "C 162 131 146 119 114 115 "
    "L 80 110 "
    "C 70 108 66 104 66 97 "
    "C 66 90 72 86 86 86 "
    "H 126 Z"
)

ICO_SIZES = [(256, 256), (64, 64), (48, 48), (32, 32), (16, 16)]

_TOKEN_RE = re.compile(r"[MHVLCZ]|[-+]?(?:\d+\.?\d*|\.\d+)")


def sample_svg_path(d: str, scale: float, curve_steps: int = 96) -> list[tuple[float, float]]:
    """Sample an absolute SVG path (M, H, V, L, C, Z) into a dense polygon."""
    tokens = _TOKEN_RE.findall(d)
    pts: list[tuple[float, float]] = []
    i = 0
    x = y = 0.0
    cmd = ""
    while i < len(tokens):
        tok = tokens[i]
        if tok.isalpha():
            cmd = tok
            i += 1
            if cmd == "Z":
                continue
        if cmd == "M":
            x, y = float(tokens[i]), float(tokens[i + 1])
            i += 2
            pts.append((x * scale, y * scale))
            cmd = "L"
        elif cmd == "H":
            x = float(tokens[i])
            i += 1
            pts.append((x * scale, y * scale))
        elif cmd == "V":
            y = float(tokens[i])
            i += 1
            pts.append((x * scale, y * scale))
        elif cmd == "L":
            x, y = float(tokens[i]), float(tokens[i + 1])
            i += 2
            pts.append((x * scale, y * scale))
        elif cmd == "C":
            x1, y1 = float(tokens[i]), float(tokens[i + 1])
            x2, y2 = float(tokens[i + 2]), float(tokens[i + 3])
            x3, y3 = float(tokens[i + 4]), float(tokens[i + 5])
            i += 6
            for step in range(1, curve_steps + 1):
                t = step / curve_steps
                u = 1.0 - t
                bx = u**3 * x + 3 * u**2 * t * x1 + 3 * u * t**2 * x2 + t**3 * x3
                by = u**3 * y + 3 * u**2 * t * y1 + 3 * u * t**2 * y2 + t**3 * y3
                pts.append((bx * scale, by * scale))
            x, y = x3, y3
        else:
            raise ValueError(f"Unsupported SVG path command: {cmd!r}")
    return pts


def master(rounded: bool) -> Image.Image:
    """Render the Interlocked SL icon at 1024x1024 via 4x supersampling."""
    img = Image.new("RGBA", (CANVAS, CANVAS), (0, 0, 0, 0))
    draw = ImageDraw.Draw(img)

    # 1. Background tile
    radius = round(TILE_RX * SCALE) if rounded else 0
    if rounded:
        draw.rounded_rectangle((0, 0, CANVAS - 1, CANVAS - 1), radius=radius, fill=BG)
        rim_layer = Image.new("RGBA", (CANVAS, CANVAS), (0, 0, 0, 0))
        ImageDraw.Draw(rim_layer).rounded_rectangle(
            (0, 0, CANVAS - 1, CANVAS - 1),
            radius=radius,
            outline=RIM,
            width=round(RIM_WIDTH * SCALE),
        )
        img = Image.alpha_composite(img, rim_layer)
        draw = ImageDraw.Draw(img)
    else:
        draw.rectangle((0, 0, CANVAS - 1, CANVAS - 1), fill=BG)

    # 2. Cyan L polygon
    draw.polygon(sample_svg_path(L_PATH, SCALE), fill=CYAN)

    # 3. Negative-space weave cutout over L stem
    cx, cy, cw, ch, crx = CUTOUT_RECT
    draw.rounded_rectangle(
        (
            round(cx * SCALE),
            round(cy * SCALE),
            round((cx + cw) * SCALE) - 1,
            round((cy + ch) * SCALE) - 1,
        ),
        radius=round(crx * SCALE),
        fill=BG,
    )

    # 4. Crisp off-white S Bezier path
    draw.polygon(sample_svg_path(S_PATH, SCALE), fill=WHITE)

    return img.resize((SIZE, SIZE), Image.Resampling.LANCZOS)


def main() -> None:
    WEB.mkdir(parents=True, exist_ok=True)
    RUST_ICONS.mkdir(parents=True, exist_ok=True)

    square = master(rounded=False)
    rounded = master(rounded=True)

    square.resize((180, 180), Image.Resampling.LANCZOS).save(WEB / "icon-180.png")
    square.resize((512, 512), Image.Resampling.LANCZOS).save(WEB / "icon-512.png")

    web_ico = WEB / "icon.ico"
    rounded.save(web_ico, format="ICO", sizes=ICO_SIZES)

    shutil.copy2(web_ico, RUST_ICONS / "icon.ico")
    rounded.resize((512, 512), Image.Resampling.LANCZOS).save(RUST_ICONS / "icon.png")

    print(f"Icons written to {WEB} and {RUST_ICONS}")


if __name__ == "__main__":
    main()
