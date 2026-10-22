"""Video/audio downloader for YouTube and the many other sites yt-dlp supports.

Uses the bundled FFmpeg to merge video+audio and to make MP3/M4A, and Node.js
(if installed) which YouTube now needs for some formats. Only download things
you have the right to.
"""
from pathlib import Path

from .common import Cancelled, ffmpeg_exe, find_program

CHOICES = {
    "best": {"label": "Best quality video", "args": ["-f", "bv*+ba/b", "-S", "res,fps,vcodec:h264,acodec:m4a",
                                                      "--merge-output-format", "mp4"]},
    "1080": {"label": "1080p video", "args": ["-f", "bv*+ba/b", "-S", "res:1080,fps,vcodec:h264,acodec:m4a",
                                               "--merge-output-format", "mp4"]},
    "720": {"label": "720p video", "args": ["-f", "bv*+ba/b", "-S", "res:720,fps,vcodec:h264,acodec:m4a",
                                             "--merge-output-format", "mp4"]},
    "480": {"label": "480p video", "args": ["-f", "bv*+ba/b", "-S", "res:480,fps,vcodec:h264,acodec:m4a",
                                             "--merge-output-format", "mp4"]},
    "mp3": {"label": "Audio only (MP3)", "args": ["-f", "ba/b", "-x", "--audio-format", "mp3", "--audio-quality", "0"]},
    "m4a": {"label": "Audio only (M4A)", "args": ["-f", "ba[ext=m4a]/ba/b", "-x", "--audio-format", "m4a"]},
}


def _base_args():
    # --no-cache-dir: don't leave a yt-dlp cache folder in the user profile
    args = ["--ffmpeg-location", ffmpeg_exe(), "--no-mtime", "--windows-filenames", "--no-colors", "--no-cache-dir"]
    if find_program("node"):
        args += ["--js-runtimes", "node"]
    return args


def _options(extra, hooks=None):
    import yt_dlp
    parsed = yt_dlp.parse_options(_base_args() + extra)
    opts = dict(parsed.ydl_opts)
    opts.update(quiet=True, no_warnings=True, noprogress=True)
    if hooks:
        opts["progress_hooks"] = [hooks[0]]
        opts["postprocessor_hooks"] = [hooks[1]]
    return opts


def get_info(url):
    """Title, channel, length and thumbnail, without downloading."""
    import yt_dlp
    with yt_dlp.YoutubeDL(_options(["--flat-playlist", "--skip-download"])) as ydl:
        info = ydl.extract_info(url, download=False)
    if info.get("_type") == "playlist":
        return {"title": info.get("title") or "Playlist", "uploader": info.get("uploader") or "",
                "duration": None, "thumbnail": _thumb(info), "playlist": True,
                "count": len(info.get("entries") or []), "site": info.get("extractor_key", "")}
    return {"title": info.get("title"), "uploader": info.get("uploader") or info.get("channel") or "",
            "duration": info.get("duration"), "thumbnail": _thumb(info), "playlist": False,
            "count": 1, "site": info.get("extractor_key", "")}


def _thumb(info):
    if info.get("thumbnail"):
        return info["thumbnail"]
    thumbs = info.get("thumbnails") or []
    return thumbs[-1]["url"] if thumbs else None


def extension(choice):
    """The file type a choice produces (video is always merged into MP4)."""
    return choice if choice in ("mp3", "m4a") else "mp4"


def download(url, choice, folder, playlist=False, progress=None, cancelled=None, name=None):
    """Download into `folder` (as `name` + extension if given). Returns the saved file paths."""
    import yt_dlp
    progress = progress or (lambda fraction, text: None)
    cancelled = cancelled or (lambda: False)
    spec = CHOICES[choice]
    folder = Path(folder)
    folder.mkdir(parents=True, exist_ok=True)
    if playlist:
        template = "%(playlist_title)s/%(playlist_index)03d %(title)s.%(ext)s"
    elif name:
        template = name.replace("%", "%%") + ".%(ext)s"  # a name chosen in the Save dialog
    else:
        template = "%(title)s.%(ext)s"
    args = spec["args"] + ["-P", str(folder), "-o", template, "--yes-playlist" if playlist else "--no-playlist"]
    saved = []
    state = {"item": 0}

    def on_progress(d):
        if cancelled():
            raise Cancelled()
        if d["status"] == "downloading":
            total = d.get("total_bytes") or d.get("total_bytes_estimate") or 0
            done = d.get("downloaded_bytes") or 0
            speed = d.get("speed") or 0
            text = f"{_mb(done)} of {_mb(total)}" if total else _mb(done)
            if speed:
                text += f" · {_mb(speed)}/s"
            progress(done / total if total else None, text)
        elif d["status"] == "finished":
            progress(None, "Processing…")

    def on_postprocess(d):
        if cancelled():
            raise Cancelled()
        if d["status"] == "started":
            progress(None, {"Merger": "Merging video and audio…", "ExtractAudio": "Making audio file…"}
                     .get(d.get("postprocessor"), "Processing…"))
        elif d["status"] == "finished" and d.get("postprocessor") in ("MoveFiles", "MoveFilesAfterDownload"):
            path = d.get("info_dict", {}).get("filepath")
            if path:
                saved.append(path)

    with yt_dlp.YoutubeDL(_options(args, (on_progress, on_postprocess))) as ydl:
        info = ydl.extract_info(url, download=True)
    if not saved:  # fall back to what yt-dlp reports
        entries = info.get("entries") or [info]
        for entry in entries:
            for req in (entry or {}).get("requested_downloads") or []:
                if req.get("filepath"):
                    saved.append(req["filepath"])
    return [p for p in dict.fromkeys(saved) if Path(p).exists()]


def _mb(n):
    return f"{n / 1024 / 1024:.1f} MB"
