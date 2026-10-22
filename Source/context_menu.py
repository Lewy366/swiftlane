"""Swiftlane in File Explorer's right-click menu (this Windows account only, no admin needed).

On Windows 11 these appear under "Show more options" (Shift+F10), like most classic apps.
"""
import sys
import winreg

ENTRIES = [  # (key under HKCU\Software\Classes, menu text, command-line flag)
    (r"*\shell\SwiftlaneShare", "Share with Swiftlane", "--share"),
    (r"Directory\shell\SwiftlaneShare", "Share with Swiftlane", "--share"),
    (r"*\shell\SwiftlaneSend", "Send to phone with Swiftlane", "--send"),
    (r"*\shell\SwiftlaneConvert", "Convert with Swiftlane", "--convert"),
]
ROOT = "Software\\Classes\\"


def _command(flag):
    if getattr(sys, "frozen", False):
        return f'"{sys.executable}" {flag} "%1"'
    from pathlib import Path
    return f'"{sys.executable}" "{Path(__file__).resolve().parent / "swiftlane.py"}" {flag} "%1"'


def enable(icon=None):
    for key, label, flag in ENTRIES:
        with winreg.CreateKey(winreg.HKEY_CURRENT_USER, ROOT + key) as k:
            winreg.SetValueEx(k, "MUIVerb", 0, winreg.REG_SZ, label)
            winreg.SetValueEx(k, "Icon", 0, winreg.REG_SZ, icon or sys.executable)
            winreg.SetValueEx(k, "MultiSelectModel", 0, winreg.REG_SZ, "Player")
        with winreg.CreateKey(winreg.HKEY_CURRENT_USER, ROOT + key + r"\command") as k:
            winreg.SetValueEx(k, "", 0, winreg.REG_SZ, _command(flag))


def disable():
    for key, _, _ in ENTRIES:
        for sub in (key + r"\command", key):
            try:
                winreg.DeleteKey(winreg.HKEY_CURRENT_USER, ROOT + sub)
            except OSError:
                pass


def is_enabled():
    try:
        winreg.CloseKey(winreg.OpenKey(winreg.HKEY_CURRENT_USER, ROOT + ENTRIES[0][0] + r"\command"))
        return True
    except OSError:
        return False
