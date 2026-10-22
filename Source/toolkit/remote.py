"""PC remote: the fixed list of things a paired phone may ask this PC to do.

Nothing outside ACTIONS can ever run: the phone sends only an action name, never a command.
Sleep and shut down are POWER actions and need their own setting (off by default).
"""
import ctypes
import subprocess
import sys

KEYS = {  # Windows virtual-key codes: media keys and arrows (arrows turn slides)
    "volume_up": 0xAF, "volume_down": 0xAE, "mute": 0xAD,
    "play_pause": 0xB3, "next": 0xB0, "previous": 0xB1,
    "slide_next": 0x27, "slide_previous": 0x25,
}
POWER = ("sleep", "shutdown", "shutdown_cancel")
ACTIONS = (*KEYS, "lock", *POWER)
SHUTDOWN_SECONDS = 60
NO_WINDOW = {"creationflags": subprocess.CREATE_NO_WINDOW} if sys.platform == "win32" else {}


def _press(vk):
    user32 = ctypes.windll.user32
    user32.keybd_event(vk, 0, 0x1, 0)        # KEYEVENTF_EXTENDEDKEY
    user32.keybd_event(vk, 0, 0x1 | 0x2, 0)  # ... | KEYEVENTF_KEYUP


def perform(action):
    """Do one action. Raises ValueError for anything not in ACTIONS."""
    if action not in ACTIONS:
        raise ValueError("unknown action")
    if action in KEYS:
        _press(KEYS[action])
    elif action == "lock":
        ctypes.windll.user32.LockWorkStation()
    elif action == "sleep":
        ctypes.windll.powrprof.SetSuspendState(0, 0, 0)
    elif action == "shutdown":
        subprocess.run(["shutdown", "/s", "/t", str(SHUTDOWN_SECONDS), "/c",
                        "Swiftlane: your phone asked to shut down this PC. Cancel it on the phone, or run: shutdown /a"],
                       capture_output=True, **NO_WINDOW)
    elif action == "shutdown_cancel":
        subprocess.run(["shutdown", "/a"], capture_output=True, **NO_WINDOW)
