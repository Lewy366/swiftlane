"""Video trimmer: cut a clip (fast, without re-encoding, or exact), make it fit a size, or keep the sound.

FFmpeg runs with an argument list (never a shell), so file names can't inject commands.
"""
import base64
import re
import subprocess
from pathlib import Path

from .common import Cancelled
from .convert import ffmpeg_exe, no_window

PRESETS = {"discord": 10, "email": 20, "whatsapp": 64}  # MB


def _run(args, timeout=60):
    return subprocess.run([ffmpeg_exe(), "-hide_banner", *args], capture_output=True, timeout=timeout, **no_window())


def probe(path):
    err = _run(["-i", str(path)], timeout=30).stderr.decode("utf-8", "replace")
    m = re.search(r"Duration: (\d+):(\d+):(\d+(?:\.\d+)?)", err)
    if not m or "Video:" not in err:
        raise ValueError("Swiftlane can't read a video in this file.")
    duration = int(m.group(1)) * 3600 + int(m.group(2)) * 60 + float(m.group(3))
    size = re.search(r"Video:.*?(\d{2,5})x(\d{2,5})", err)
    return {"duration": duration, "width": int(size.group(1)) if size else 0, "height": int(size.group(2)) if size else 0,
            "audio": "Audio:" in err, "bytes": Path(path).stat().st_size, "name": Path(path).name}


def frame(path, t, height=360):
    """One picture from the video at second t, as a data: URL."""
    r = _run(["-ss", f"{max(0.0, float(t)):.3f}", "-i", str(path), "-frames:v", "1", "-vf", f"scale=-2:{int(height)}",
              "-f", "image2pipe", "-vcodec", "mjpeg", "-q:v", "5", "-"], timeout=30)
    if not r.stdout:
        return None
    return "data:image/jpeg;base64," + base64.b64encode(r.stdout).decode()


def strip(path, duration, count=10, height=64):
    return [frame(path, duration * (i + .5) / count, height) for i in range(count)]


def plan_size(duration, target_mb, height):
    """Video bitrate (bits/s) and output height so the clip lands just under target_mb."""
    total = target_mb * 8 * 1024 * 1024 * 0.95
    video = max(120_000, total / max(duration, 0.1) - 128_000)
    out_h = height
    if video < 700_000 and height > 480:
        out_h = 480
    elif video < 1_800_000 and height > 720:
        out_h = 720
    return int(video), out_h


def trim(path, start, end, output, mode="fast", target_mb=10, height=0, progress=None, cancelled=None):
    start, end = max(0.0, float(start)), float(end)
    length = end - start
    if length <= 0.05:
        raise ValueError("The cut is empty: move the end after the start.")
    base = ["-y", "-ss", f"{start:.3f}", "-i", str(path), "-t", f"{length:.3f}"]
    if mode == "fast":
        args = [*base, "-map", "0:v:0?", "-map", "0:a?", "-c", "copy", "-avoid_negative_ts", "make_zero"]
    elif mode == "exact":
        args = [*base, "-c:v", "libx264", "-preset", "medium", "-crf", "20", "-c:a", "aac", "-b:a", "160k", "-movflags", "+faststart"]
    elif mode == "size":
        rate, out_h = plan_size(length, float(target_mb), height)
        args = [*base, "-c:v", "libx264", "-preset", "medium", "-b:v", str(rate), "-maxrate", str(int(rate * 1.3)),
                "-bufsize", str(rate * 2), "-c:a", "aac", "-b:a", "128k", "-movflags", "+faststart"]
        if out_h and height and out_h < height:
            args += ["-vf", f"scale=-2:{out_h}"]
    elif mode == "audio":
        args = [*base, "-vn", "-c:a", "libmp3lame", "-b:a", "192k"]
    else:
        raise ValueError("Unknown mode.")
    proc = subprocess.Popen([ffmpeg_exe(), "-hide_banner", "-loglevel", "error", "-progress", "pipe:1", "-nostats", *args, str(output)],
                            stdout=subprocess.PIPE, stderr=subprocess.PIPE, **no_window())
    try:
        for line in proc.stdout:
            if cancelled and cancelled():
                proc.kill()
                raise Cancelled()
            m = re.match(rb"out_time_us=(\d+)", line)
            if m and progress:
                progress(min(1.0, int(m.group(1)) / 1e6 / length))
        proc.wait()
    finally:
        if proc.poll() is None:
            proc.kill()
    if proc.returncode != 0 or not Path(output).exists():
        Path(output).unlink(missing_ok=True)
        raise ValueError("FFmpeg couldn't cut this video: " + proc.stderr.read().decode("utf-8", "replace")[-200:])
    return [Path(output)]
