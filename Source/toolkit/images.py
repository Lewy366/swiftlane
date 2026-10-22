"""Image tools: crop, straighten, rotate/flip, resize, adjust colours, compress,
strip metadata, change format, with a fast preview for the live editor.

Edits are applied in this order (the editor shows them the same way):
  upright (EXIF) -> rotate 90° steps -> flip -> straighten -> crop -> resize -> adjust -> save

edit_image(src, ops)    -> {"output", "before", "after", "size"}   full-size result, original untouched
preview_source(src)     -> small upright copy for the editor + the full size
render_preview(src, ops)-> what the result will look like (incl. compression) + estimated file size
"""
import base64
import io
import math
from pathlib import Path

from .common import free_path

FORMATS = {"keep": None, "jpg": "JPEG", "png": "PNG", "webp": "WEBP"}
RATIOS = {"1:1": 1, "4:3": 4 / 3, "3:2": 3 / 2, "16:9": 16 / 9, "9:16": 9 / 16, "3:4": 3 / 4, "4:5": 4 / 5}
PREVIEW_MAX = 1400
_cache = {}  # path -> (mtime, upright preview image, full size, exif, icc, format)


def _open(src):
    from PIL import Image
    try:
        import pillow_heif
        pillow_heif.register_heif_opener()
    except ImportError:
        pass
    return Image.open(src)


def _load_upright(src):
    from PIL import ImageOps
    with _open(src) as original:
        info = (original.info.get("exif"), original.info.get("icc_profile"), original.format)
        img = ImageOps.exif_transpose(original)
        img.load()
    return img, info


def _preview_base(src):
    """Cached small upright copy, so dragging sliders doesn't re-decode a 12 MP photo each time."""
    src = Path(src)
    mtime = src.stat().st_mtime
    hit = _cache.get(str(src))
    if hit and hit[0] == mtime:
        return hit[1:]
    img, (exif, icc, fmt) = _load_upright(src)
    full = img.size
    small = img.copy()
    small.thumbnail((PREVIEW_MAX, PREVIEW_MAX))
    if len(_cache) > 8:
        _cache.clear()
    _cache[str(src)] = (mtime, small, full, exif, icc, fmt)
    return small, full, exif, icc, fmt


def _data_url(img, fmt="JPEG", quality=88):
    buf = io.BytesIO()
    if fmt == "JPEG" and img.mode not in ("RGB", "L"):
        img = _on_white(img)
    img.save(buf, fmt, quality=quality) if fmt in ("JPEG", "WEBP") else img.save(buf, fmt)
    mime = {"JPEG": "jpeg", "PNG": "png", "WEBP": "webp"}[fmt]
    return f"data:image/{mime};base64," + base64.b64encode(buf.getvalue()).decode(), buf.tell()


def preview_source(src):
    """The editor's starting picture: upright, at most 1400 px, plus the real size."""
    small, full, _, _, fmt = _preview_base(src)
    has_alpha = small.mode in ("RGBA", "LA", "P")
    data, _ = _data_url(small, "PNG" if has_alpha else "JPEG", 90)
    return {"data": data, "width": full[0], "height": full[1], "preview": list(small.size),
            "bytes": Path(src).stat().st_size, "format": fmt or Path(src).suffix.lstrip(".").upper()}


# ---------------------------------------------------------------- the edit pipeline

def straighten_scale(angle_deg, w, h):
    """How much to zoom in after rotating by `angle_deg` so no empty corners show."""
    a = math.radians(abs(angle_deg))
    if a == 0:
        return 1.0
    return max((w * math.cos(a) + h * math.sin(a)) / w, (w * math.sin(a) + h * math.cos(a)) / h)


def output_size(full_w, full_h, ops):
    """Final pixel size for these ops, without touching any pixels."""
    rot = int(ops.get("rotate") or 0) % 360
    w, h = (full_h, full_w) if rot in (90, 270) else (full_w, full_h)
    s = straighten_scale(float(ops.get("straighten") or 0), w, h)
    w, h = w / s, h / s
    box = _crop_fractions(ops, w, h)
    if box:
        w, h = w * box[2], h * box[3]
    w, h = max(1, round(w)), max(1, round(h))
    return _resize_target(w, h, ops.get("resize"))


def _crop_fractions(ops, w, h):
    """[x, y, w, h] as fractions of the (rotated, straightened) image, or None."""
    box = ops.get("crop_box")
    if box and len(box) == 4:
        x, y, bw, bh = (min(max(float(v), 0.0), 1.0) for v in box)
        bw, bh = min(bw, 1 - x), min(bh, 1 - y)
        if bw > 0.001 and bh > 0.001 and (bw, bh) != (1, 1):
            return x, y, bw, bh
        return None
    ratio = RATIOS.get(ops.get("crop") or "")
    if not ratio:
        return None
    if w / h > ratio:
        bw = ratio * h / w
        return (1 - bw) / 2, 0.0, bw, 1.0
    bh = (w / ratio) / h
    return 0.0, (1 - bh) / 2, 1.0, bh


def _resize_target(w, h, spec):
    if not spec:
        return w, h
    if spec.get("width") or spec.get("height"):  # explicit size from the editor (aspect already kept by the UI)
        tw, th = int(spec.get("width") or 0), int(spec.get("height") or 0)
        if tw and not th:
            th = round(h * tw / w)
        if th and not tw:
            tw = round(w * th / h)
        return max(1, tw), max(1, th)
    mode, value = spec.get("mode"), float(spec.get("value") or 0)
    if value <= 0:
        return w, h
    scale = {"percent": value / 100, "longest": value / max(w, h), "width": value / w, "height": value / h}.get(mode)
    if not scale:
        return w, h
    return max(1, round(w * scale)), max(1, round(h * scale))


def _transform(img, ops, final_size=None):
    """Apply geometry + colour edits to `img` (any scale). final_size: resize to this at the end."""
    from PIL import Image, ImageEnhance, ImageOps
    rot = int(ops.get("rotate") or 0) % 360
    if rot:
        img = img.rotate(-rot, expand=True)  # PIL turns counter-clockwise; ours is clockwise
    flip = ops.get("flip")
    if ops.get("flip_h") or flip == "horizontal":
        img = ImageOps.mirror(img)
    if ops.get("flip_v") or flip == "vertical":
        img = ImageOps.flip(img)
    angle = float(ops.get("straighten") or 0)
    if angle:
        w, h = img.size
        s = straighten_scale(angle, w, h)
        fill = (0, 0, 0, 0) if img.mode == "RGBA" else None
        rotated = img.rotate(-angle, resample=Image.BICUBIC, expand=False, fillcolor=fill)
        cw, ch = w / s, h / s
        img = rotated.crop((round((w - cw) / 2), round((h - ch) / 2), round((w + cw) / 2), round((h + ch) / 2)))
    box = _crop_fractions(ops, *img.size)
    if box:
        w, h = img.size
        x, y, bw, bh = box
        img = img.crop((round(x * w), round(y * h), round((x + bw) * w), round((y + bh) * h)))
    if final_size and tuple(final_size) != img.size:
        img = img.resize(tuple(final_size), Image.LANCZOS)
    adjust = ops.get("adjust") or {}
    if any(abs(float(adjust.get(k, 1)) - 1) > 1e-3 for k in ("brightness", "contrast", "saturation", "sharpness")):
        alpha = img.getchannel("A") if img.mode == "RGBA" else None
        base = img.convert("RGB")
        for key, enhancer in (("brightness", ImageEnhance.Brightness), ("contrast", ImageEnhance.Contrast),
                              ("saturation", ImageEnhance.Color), ("sharpness", ImageEnhance.Sharpness)):
            factor = float(adjust.get(key, 1))
            if abs(factor - 1) > 1e-3:
                base = enhancer(base).enhance(factor)
        if alpha is not None:
            base.putalpha(alpha)
        img = base
    return img


def _target_format(ops, src_format, src):
    # A name typed in the Save dialog ("photo.png") decides the format.
    chosen = Path(ops["output"]).suffix.lower().lstrip(".") if ops.get("output") else ""
    by_ext = {"jpg": "JPEG", "jpeg": "JPEG", "png": "PNG", "webp": "WEBP"}
    fmt = by_ext.get(chosen) or FORMATS.get(ops.get("format") or "keep") or _same_format(src_format, src)
    ext = {"JPEG": "jpg", "PNG": "png", "WEBP": "webp"}[fmt]
    return fmt, ext


def _output_path(ops, src, ext):
    """The exact file chosen in the Save dialog, or '<name> (edited).<ext>' next to the original."""
    if ops.get("output"):
        out = Path(ops["output"])
        if out.suffix.lower().lstrip(".") not in ("jpg", "jpeg", "png", "webp"):
            out = out.with_name(out.name + "." + ext)
        out.parent.mkdir(parents=True, exist_ok=True)
        return out
    return free_path(ops.get("folder") or src.parent, f"{src.stem} (edited).{ext}")


def edit_image(src, ops):
    """Full-size result saved as '<name> (edited).<ext>'. See the module docstring for ops."""
    from PIL import Image
    src = Path(src)
    img, (exif, icc, src_format) = _load_upright(src)
    final = output_size(*img.size, ops)
    img = _transform(img, ops, final)
    fmt, ext = _target_format(ops, src_format, src)
    if fmt == "JPEG" and img.mode not in ("RGB", "L"):
        img = _on_white(img)
    out = _output_path(ops, src, ext)  # the picture is already in memory, so saving over the original is safe
    save = _save_options(fmt, ops, exif, icc)
    img.save(out, fmt, **save)
    return {"output": str(out), "before": src.stat().st_size, "after": out.stat().st_size, "size": list(img.size)}


def _save_options(fmt, ops, exif, icc):
    from PIL import Image
    save = {}
    if fmt in ("JPEG", "WEBP"):
        save["quality"] = max(1, min(100, int(ops.get("quality") or 90)))
    if fmt in ("JPEG", "PNG"):
        save["optimize"] = True
    if not ops.get("strip_metadata"):
        if exif:
            e = Image.Exif()
            e.load(exif)
            e[0x0112] = 1  # pixels are already upright
            save["exif"] = e.tobytes()
        if icc:
            save["icc_profile"] = icc
    return save


def render_preview(src, ops):
    """The result as it will look (incl. compression), at preview size, plus size estimates."""
    small, full, exif, icc, src_format = _preview_base(src)
    final = output_size(*full, ops)
    preview_scale = small.size[0] / full[0]
    preview_final = (max(1, round(final[0] * preview_scale)), max(1, round(final[1] * preview_scale)))
    img = _transform(small, ops, preview_final)
    fmt, ext = _target_format(ops, src_format, Path(src))
    if fmt == "JPEG" and img.mode not in ("RGB", "L"):
        img = _on_white(img)
    buf = io.BytesIO()
    img.save(buf, fmt, **_save_options(fmt, ops, None, None))
    encoded = buf.getvalue()
    # File size grows roughly with the number of pixels; scale the preview's size up.
    est = len(encoded) * (final[0] * final[1]) / max(1, img.size[0] * img.size[1])
    if not ops.get("strip_metadata") and exif:
        est += len(exif)
    mime = {"JPEG": "jpeg", "PNG": "png", "WEBP": "webp"}[fmt]
    return {"data": f"data:image/{mime};base64," + base64.b64encode(encoded).decode(),
            "width": final[0], "height": final[1], "estimate": int(est), "format": ext.upper()}


def _same_format(src_format, src):
    if src_format in ("JPEG", "PNG", "WEBP"):
        return src_format
    return "JPEG" if Path(src).suffix.lower() in (".heic", ".heif", ".jpg", ".jpeg") else "PNG"


def _on_white(img):
    from PIL import Image
    img = img.convert("RGBA")
    bg = Image.new("RGB", img.size, (255, 255, 255))
    bg.paste(img, mask=img.split()[-1])
    return bg
