"""Where Windows keeps the user's desktop (it can be moved, e.g. into OneDrive)."""
import ctypes
from pathlib import Path


def desktop_shortcut(app="Swiftlane"):
    buf = ctypes.create_unicode_buffer(260)
    ctypes.windll.shell32.SHGetFolderPathW(None, 0x10, None, 0, buf)  # CSIDL_DESKTOPDIRECTORY
    return Path(buf.value) / f"{app}.lnk"
