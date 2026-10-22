"""Small helpers shared by the tools."""
import os
import shutil
import subprocess
import sys
from pathlib import Path


class Cancelled(Exception):
    """Raised inside a job when the user presses Cancel."""


def free_path(folder, name):
    """folder/name, or folder/'name (1).ext' etc. if taken. Never overwrites."""
    folder = Path(folder)
    folder.mkdir(parents=True, exist_ok=True)
    stem, ext = os.path.splitext(name)
    n = 0
    while True:
        candidate = folder / (name if n == 0 else f"{stem} ({n}){ext}")
        if not candidate.exists():
            return candidate
        n += 1


def ffmpeg_exe():
    """The FFmpeg that ships with Swiftlane (via imageio-ffmpeg)."""
    import imageio_ffmpeg
    return imageio_ffmpeg.get_ffmpeg_exe()


def no_window():
    """subprocess flags so helper programs never flash a console window."""
    return {"creationflags": subprocess.CREATE_NO_WINDOW} if sys.platform == "win32" else {}


def find_program(name):
    return shutil.which(name)


def clipboard_text():
    """Text on the Windows clipboard ('' if none), for the downloader's Paste button."""
    if sys.platform != "win32":
        return ""
    import ctypes
    user32, kernel32 = ctypes.windll.user32, ctypes.windll.kernel32
    user32.GetClipboardData.restype = ctypes.c_void_p
    kernel32.GlobalLock.argtypes = [ctypes.c_void_p]
    kernel32.GlobalLock.restype = ctypes.c_void_p
    kernel32.GlobalUnlock.argtypes = [ctypes.c_void_p]
    if not user32.OpenClipboard(None):
        return ""
    try:
        handle = user32.GetClipboardData(13)  # CF_UNICODETEXT
        if not handle:
            return ""
        ptr = kernel32.GlobalLock(handle)
        try:
            return ctypes.wstring_at(ptr) if ptr else ""
        finally:
            kernel32.GlobalUnlock(handle)
    finally:
        user32.CloseClipboard()
