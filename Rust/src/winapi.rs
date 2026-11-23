//! The few Windows things Swiftlane needs directly: start with Windows (the Run key, same entry
//! as the Python version), opening folders in Explorer, and starting a program as administrator.

#[cfg(windows)]
mod imp {
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;
    use winreg::enums::{HKEY_CURRENT_USER, KEY_READ, KEY_SET_VALUE};
    use winreg::RegKey;

    const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

    pub fn autostart_enabled() -> bool {
        RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey_with_flags(RUN_KEY, KEY_READ)
            .and_then(|k| k.get_value::<String, _>("Swiftlane"))
            .is_ok()
    }

    pub fn set_autostart(on: bool) -> bool {
        let Ok(key) = RegKey::predef(HKEY_CURRENT_USER).open_subkey_with_flags(RUN_KEY, KEY_SET_VALUE) else { return false };
        if on {
            let Ok(exe) = std::env::current_exe() else { return false };
            key.set_value("Swiftlane", &format!("\"{}\" --minimized", exe.display())).is_ok()
        } else {
            let _ = key.delete_value("Swiftlane");
            true
        }
    }

    fn wide(s: &std::ffi::OsStr) -> Vec<u16> {
        s.encode_wide().chain(std::iter::once(0)).collect()
    }

    /// Starts `exe` as administrator (Windows asks). False if the user said no.
    pub fn run_elevated(exe: &Path, params: &str, directory: &Path) -> bool {
        use windows_sys::Win32::UI::Shell::{ShellExecuteExW, SHELLEXECUTEINFOW};
        use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
        let verb = wide("runas".as_ref());
        let file = wide(exe.as_os_str());
        let args = wide(params.as_ref());
        let dir = wide(directory.as_os_str());
        unsafe {
            let mut info: SHELLEXECUTEINFOW = std::mem::zeroed();
            info.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
            info.lpVerb = verb.as_ptr();
            info.lpFile = file.as_ptr();
            info.lpParameters = args.as_ptr();
            info.lpDirectory = dir.as_ptr();
            info.nShow = SW_SHOWNORMAL;
            ShellExecuteExW(&mut info) != 0
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use std::path::Path;
    pub fn autostart_enabled() -> bool {
        false
    }
    pub fn set_autostart(_on: bool) -> bool {
        false
    }
    pub fn run_elevated(_exe: &Path, _params: &str, _directory: &Path) -> bool {
        false
    }
}

pub use imp::*;

/// Opens a folder in File Explorer (only folders that exist).
pub fn open_folder(path: &std::path::Path) -> bool {
    if !path.is_dir() {
        return false;
    }
    std::process::Command::new(if cfg!(windows) { "explorer" } else { "xdg-open" }).arg(path).spawn().is_ok()
}
