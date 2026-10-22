"""Draws the Swiftlane icon into web/: the logo's italic cyan "S" in motion (two fading
copies behind it), driving between the two lane lines, on a dark tile.

Run once after changing the design: python make_icons.py   (needs Pillow and Windows' Bahnschrift font)
Writes icon-180.png (iPhone Home Screen), icon-512.png (web manifest) and icon.ico (Windows app).
"""
import os
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont

WEB = Path(__file__).resolve().parent / "web"
FONT = Path(os.environ.get("SystemRoot", r"C:\Windows")) / "Fonts" / "bahnschrift.ttf"
TILE = (20, 22, 31)
CYAN = (20, 184, 224)
LINE = (236, 240, 246)
SLANT = 0.176  # same lean as the logo's italic (tan 10 degrees)
BIG = 1024


def master(rounded):
    """The icon at 1024 px. rounded=False for iPhone, which rounds icons itself."""
    img = Image.new("RGBA", (BIG, BIG), (0, 0, 0, 0))
    mask = Image.new("L", (BIG, BIG), 0)
    ImageDraw.Draw(mask).rounded_rectangle((0, 0, BIG - 1, BIG - 1), radius=230 if rounded else 0, fill=255)
    tile = Image.new("RGBA", (BIG, BIG), TILE + (255,))
    sheen = Image.new("L", (BIG, BIG), 0)  # a soft light from the top, like the app's glass
    sd = ImageDraw.Draw(sheen)
    for yy in range(BIG // 2):
        sd.line((0, yy, BIG, yy), fill=round(24 * (1 - yy / (BIG / 2)) ** 1.5))
    tile = Image.composite(Image.new("RGBA", (BIG, BIG), (255, 255, 255, 255)), tile, sheen)
    img.paste(tile, (0, 0), mask)

    # the S, upright first, then slanted like the logo
    font = ImageFont.truetype(str(FONT), 760)
    try:
        font.set_variation_by_name("Bold")
    except Exception:
        pass
    layer = Image.new("RGBA", (BIG, BIG), (0, 0, 0, 0))
    d = ImageDraw.Draw(layer)
    box = d.textbbox((0, 0), "S", font=font)
    w, h = box[2] - box[0], box[3] - box[1]
    x, y = (BIG - w) / 2 - box[0] + 40, (BIG - h) / 2 - box[1]
    for dx, alpha in ((-120, 40), (-62, 90), (0, 255)):  # motion: two fading copies trail behind
        d.text((x + dx, y), "S", font=font, fill=CYAN + (alpha,))
    layer = layer.transform(layer.size, Image.AFFINE, (1, SLANT, -SLANT * BIG / 2, 0, 1, 0), Image.BICUBIC)
    img.alpha_composite(layer)

    # the lane: two lines above and below, leaning the same way
    lines = Image.new("RGBA", (BIG, BIG), (0, 0, 0, 0))
    ld = ImageDraw.Draw(lines)
    for yy in (150, BIG - 150):
        shift = (yy - BIG / 2) * SLANT
        ld.line((150 - shift, yy, BIG - 150 - shift, yy), fill=LINE + (235,), width=34)
        for end in (150 - shift, BIG - 150 - shift):
            ld.ellipse((end - 17, yy - 17, end + 17, yy + 17), fill=LINE + (235,))
    img.alpha_composite(lines)
    out = Image.new("RGBA", (BIG, BIG), (0, 0, 0, 0))
    out.paste(img, (0, 0), mask)
    return out


if __name__ == "__main__":
    square, rounded = master(False), master(True)
    square.resize((180, 180), Image.LANCZOS).save(WEB / "icon-180.png")
    square.resize((512, 512), Image.LANCZOS).save(WEB / "icon-512.png")
    rounded.save(WEB / "icon.ico", sizes=[(256, 256), (64, 64), (48, 48), (32, 32), (16, 16)])
    print("Icons written to", WEB)
