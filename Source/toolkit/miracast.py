"""Android to this PC (Miracast), through Windows' own receiver: the "Wireless Display" app, a free
optional part of Windows 11. Swiftlane only installs it (Windows asks for admin and downloads it
from Windows Update), opens it and opens Windows' own projection settings (PIN, who may connect).
All receiving is done by Microsoft's code; Swiftlane adds nothing that listens on the network.
"""
import subprocess

CAPABILITY = "App.WirelessDisplay.Connect~~~~0.0.1.0"
APP_ID = "Microsoft.PPIProjection_cw5n1h2txyewy!Microsoft.PPIProjection"   # Wireless Display's Start menu ID


def _ps(command, timeout=30):
    try:
        return subprocess.run(["powershell.exe", "-NoProfile", "-Command", command], capture_output=True, text=True,
                              timeout=timeout, creationflags=subprocess.CREATE_NO_WINDOW).stdout
    except (OSError, subprocess.SubprocessError):
        return ""


def supported():
    """True/False if Windows says this PC's Wi-Fi and graphics can receive Miracast, None if unknown."""
    out = _ps("netsh wlan show drivers")
    for line in out.splitlines():
        if "Wireless Display Supported" in line:
            return line.split(":", 1)[1].strip().lower().startswith("yes")
    return None


def app_id():
    """The Wireless Display app's Start menu ID, or None if it isn't installed."""
    out = _ps(f"Get-StartApps | Where-Object {{ $_.AppID -eq '{APP_ID}' }} | Select-Object -First 1 -ExpandProperty AppID")
    return out.strip() or None


def install_command():
    """(program, parameters) to run as administrator."""
    return ("powershell.exe", f"-NoProfile -WindowStyle Hidden -Command \"Add-WindowsCapability -Online -Name '{CAPABILITY}' | Out-Null\"")


def start():
    aid = app_id()
    if not aid:
        raise RuntimeError("Wireless Display isn't installed yet.")
    subprocess.Popen(["explorer.exe", f"shell:AppsFolder\\{aid}"])


def open_settings():
    import os
    os.startfile("ms-settings:project")
