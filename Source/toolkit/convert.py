"""File converter: images (Pillow), video and audio (FFmpeg), PDF pages (pdfium).

convert_file(src, target, options, progress, cancelled) -> list of output paths.
Outputs never overwrite anything; by default they go next to the original.
"""
import re
import subprocess
import threading
from pathlib import Path

from .common import Cancelled, ffmpeg_exe, free_path, no_window

IMAGE_IN = ["jpg", "jpeg", "png", "webp", "gif", "bmp", "tif", "tiff", "ico", "avif", "heic", "heif"]
VIDEO_IN = ["mp4", "mov", "mkv", "avi", "webm", "m4v", "wmv", "flv", "3gp", "mpg", "mpeg", "ts"]
AUDIO_IN = ["mp3", "wav", "m4a", "aac", "flac", "ogg", "opus", "wma", "aiff", "aif"]
PDF_IN = ["pdf"]

IMAGE_OUT = ["jpg", "png", "webp", "gif", "bmp", "tiff", "ico", "avif", "pdf"]
VIDEO_OUT = ["mp4", "mov", "mkv", "avi", "webm", "gif"]
AUDIO_OUT = ["mp3", "wav", "m4a", "aac", "flac", "ogg", "opus"]
PDF_OUT = ["jpg", "png"]

# quality preset -> (video CRF for x264, audio bitrate, JPEG/WebP quality)
QUALITY = {"high": (18, "320k", 95), "balanced": (23, "192k", 88), "small": (28, "128k", 75)}


def kind_of(path):
    ext = Path(path).suffix.lower().lstrip(".")
    for kind, exts in (("image", IMAGE_IN), ("video", VIDEO_IN), ("audio", AUDIO_IN), ("pdf", PDF_IN)):
        if ext in exts:
            return kind
    return None


def targets_for(kind):
    return {"image": IMAGE_OUT, "video": VIDEO_OUT + AUDIO_OUT, "audio": AUDIO_OUT,
            "pdf": PDF_OUT}.get(kind, [])


def supported_formats():
    return {"image": {"in": IMAGE_IN, "out": IMAGE_OUT},
            "video": {"in": VIDEO_IN, "out": VIDEO_OUT + AUDIO_OUT},
            "audio": {"in": AUDIO_IN, "out": AUDIO_OUT},
            "pdf": {"in": PDF_IN, "out": PDF_OUT}}


def convert_file(src, target, options=None, progress=None, cancelled=None):
    options = options or {}
    progress = progress or (lambda fraction: None)
    cancelled = cancelled or (lambda: False)
    src = Path(src)
    target = target.lower().lstrip(".")
    kind = kind_of(src)
    if target not in targets_for(kind):
        raise ValueError(f"Can't convert {src.suffix or 'this file'} to .{target}")
    out_dir = Path(options.get("folder") or src.parent)
    quality = QUALITY.get(options.get("quality", "balanced"), QUALITY["balanced"])
    if kind == "pdf":
        return _pdf_pages(src, target, out_dir, progress, cancelled)
    # options["output"]: the exact file chosen in the Save dialog
    out = _exact_output(options.get("output"), src, target) if options.get("output") else None
    if kind == "image":
        return [_image(src, target, out_dir, quality[2], out)]
    return [_media(src, target, out_dir, quality, progress, cancelled, out)]


def _exact_output(path, src, target):
    out = Path(path)
    if out.suffix.lower() != "." + target:
        out = out.with_name(out.name + "." + target)
    if out.resolve() == Path(src).resolve():
        raise ValueError("Choose a different name: the converted file can't replace the original it's made from.")
    out.parent.mkdir(parents=True, exist_ok=True)
    return out


def images_to_pdf(paths, out_dir=None, output=None):
    """Combine several images into one PDF (one page each). `output`: exact file from the Save dialog."""
    from PIL import Image
    _register_heif()
    pages = []
    for p in paths:
        with Image.open(p) as img:
            pages.append(_flatten(_upright(img)).copy())
    first = Path(paths[0])
    if output:
        out = _exact_output(output, "", "pdf")
    else:
        out = free_path(out_dir or first.parent, f"{first.stem} + {len(paths) - 1} more.pdf"
                        if len(paths) > 1 else f"{first.stem}.pdf")
    pages[0].save(out, "PDF", save_all=True, append_images=pages[1:], resolution=150)
    return out


# ---------------------------------------------------------------- images

def _register_heif():
    try:
        import pillow_heif
        pillow_heif.register_heif_opener()
    except ImportError:
        pass


def _upright(img):
    from PIL import ImageOps
    return ImageOps.exif_transpose(img)


def _flatten(img):
    """RGB with transparency turned white (for JPG/PDF/BMP)."""
    from PIL import Image
    if img.mode in ("RGBA", "LA", "P"):
        img = img.convert("RGBA")
        bg = Image.new("RGB", img.size, (255, 255, 255))
        bg.paste(img, mask=img.split()[-1])
        return bg
    return img.convert("RGB")


def _image(src, target, out_dir, quality, out=None):
    from PIL import Image
    _register_heif()
    ext = {"jpeg": "jpg", "tif": "tiff"}.get(target, target)
    out = out or free_path(out_dir, f"{src.stem}.{ext}")
    with Image.open(src) as img:
        exif = img.info.get("exif")
        frames = getattr(img, "n_frames", 1)
        if frames > 1 and ext in ("gif", "webp"):  # keep animations
            img.save(out, save_all=True, loop=img.info.get("loop", 0),
                     duration=img.info.get("duration", 100), quality=quality)
            return out
        img = _upright(img)
        if ext in ("jpg", "bmp", "pdf"):
            img = _flatten(img)
        elif ext == "gif":
            img = img.convert("RGBA").convert("P", palette=Image.ADAPTIVE)
        elif img.mode not in ("RGB", "RGBA", "L", "LA"):
            img = img.convert("RGBA")
        opts = {}
        if ext in ("jpg", "webp", "avif"):
            opts["quality"] = quality
        if ext == "jpg":
            opts.update(optimize=True)
        if exif and ext in ("jpg", "webp", "png", "tiff", "avif"):
            from PIL import Image as _I
            e = _I.Exif()
            e.load(exif)
            e[0x0112] = 1  # pixels are already upright
            opts["exif"] = e.tobytes()
        if ext == "ico":
            side = max(img.size)
            square = Image.new("RGBA", (side, side), (0, 0, 0, 0))
            square.paste(img.convert("RGBA"), ((side - img.width) // 2, (side - img.height) // 2))
            img = square
            opts["sizes"] = [(s, s) for s in (256, 128, 64, 48, 32, 16) if s <= max(side, 16)]
        fmt = {"jpg": "JPEG", "tiff": "TIFF", "ico": "ICO", "pdf": "PDF"}.get(ext, ext.upper())
        img.save(out, fmt, **opts)
    return out


# ---------------------------------------------------------------- PDF pages

def _pdf_pages(src, target, out_dir, progress, cancelled):
    import pypdfium2 as pdfium
    pdf = pdfium.PdfDocument(str(src))
    outputs = []
    try:
        total = len(pdf)
        for i in range(total):
            if cancelled():
                raise Cancelled()
            page = pdf[i]
            image = page.render(scale=2).to_pil()  # 144 dpi
            out = free_path(out_dir, f"{src.stem} - page {i + 1}.{target}")
            if target == "jpg":
                image.convert("RGB").save(out, "JPEG", quality=92)
            else:
                image.save(out, "PNG")
            outputs.append(out)
            progress((i + 1) / total)
    finally:
        pdf.close()
    return outputs


# ---------------------------------------------------------------- video / audio

def _media_args(target, quality, has_video_in):
    crf, abr, _ = quality
    x264 = ["-c:v", "libx264", "-preset", "medium", "-crf", str(crf), "-pix_fmt", "yuv420p"]
    aac = ["-c:a", "aac", "-b:a", abr]
    return {
        "mp4": x264 + aac + ["-movflags", "+faststart"],
        "mov": x264 + aac + ["-movflags", "+faststart"],
        "mkv": x264 + aac,
        "avi": ["-c:v", "mpeg4", "-q:v", str(max(2, crf // 6)), "-c:a", "libmp3lame", "-b:a", abr],
        "webm": ["-c:v", "libvpx-vp9", "-crf", str(crf + 10), "-b:v", "0", "-row-mt", "1",
                 "-deadline", "good", "-cpu-used", "4", "-c:a", "libopus", "-b:a", "128k"],
        "gif": ["-vf", "fps=12,scale='min(540,iw)':-2:flags=lanczos,split[a][b];[a]palettegen[p];[b][p]paletteuse",
                "-an", "-loop", "0"],
        "mp3": ["-vn", "-c:a", "libmp3lame", "-b:a", abr],
        "wav": ["-vn", "-c:a", "pcm_s16le"],
        "m4a": ["-vn", "-c:a", "aac", "-b:a", abr],
        "aac": ["-vn", "-c:a", "aac", "-b:a", abr, "-f", "adts"],
        "flac": ["-vn", "-c:a", "flac"],
        "ogg": ["-vn", "-c:a", "libvorbis", "-q:a", "6" if abr == "320k" else "5" if abr == "192k" else "3"],
        "opus": ["-vn", "-c:a", "libopus", "-b:a", "160k" if abr != "128k" else "96k"],
    }[target]


def media_duration(src):
    """Length in seconds (0 if unknown), read from FFmpeg's header output."""
    result = subprocess.run([ffmpeg_exe(), "-hide_banner", "-i", str(src)], capture_output=True,
                            text=True, errors="replace", **no_window())
    m = re.search(r"Duration: (\d+):(\d+):(\d+(?:\.\d+)?)", result.stderr)
    return int(m[1]) * 3600 + int(m[2]) * 60 + float(m[3]) if m else 0.0


def _media(src, target, out_dir, quality, progress, cancelled, out=None):
    out = out or free_path(out_dir, f"{src.stem}.{target}")
    duration = media_duration(src)
    cmd = [ffmpeg_exe(), "-hide_banner", "-nostdin", "-y", "-i", str(src),
           *_media_args(target, quality, True), "-progress", "pipe:1", "-nostats", str(out)]
    proc = subprocess.Popen(cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
                            errors="replace", **no_window())
    errors = []
    threading.Thread(target=lambda: errors.extend(proc.stderr), daemon=True).start()
    try:
        for line in proc.stdout:
            if cancelled():
                proc.kill()
                raise Cancelled()
            if line.startswith("out_time_us=") and duration:
                try:
                    progress(min(int(line.split("=")[1]) / 1e6 / duration, 0.999))
                except ValueError:
                    pass
        proc.wait()
    except Cancelled:
        proc.wait()
        out.unlink(missing_ok=True)
        raise
    if proc.returncode != 0:
        out.unlink(missing_ok=True)
        detail = next((l.strip() for l in reversed(errors) if l.strip()), "unknown error")
        raise RuntimeError(f"FFmpeg couldn't convert this file ({detail[:160]})")
    progress(1.0)
    return out
