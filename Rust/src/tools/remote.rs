//! PC remote: the fixed list of things a paired phone may ask this PC to do.
//! Port of Source/toolkit/remote.py plus remote_state / remote_try / _remote in Source/swiftlane.py.
//!
//! Security rules (matching Python):
//! - Nothing outside ACTIONS can ever run: the phone sends only an action name, never a command.
//! - Sleep and shut down are POWER actions and require their own setting (`remote_power`, off by default).
//! - `remote_try` from the PC page only allows sound/music keys.
//! - Rate limited to at most 10 presses per 2 seconds.

use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Manager};

use super::{arg, netscan};
use crate::api::{push, AppState};
use crate::config;

const SHUTDOWN_SECONDS: &str = "60";
const MAX_LOG: usize = 30;
const RATE_LIMIT_MAX: usize = 10;
const RATE_LIMIT_WINDOW: Duration = Duration::from_secs(2);

pub fn key_vk(action: &str) -> Option<u8> {
    Some(match action {
        "volume_up" => 0xAF,
        "volume_down" => 0xAE,
        "mute" => 0xAD,
        "play_pause" => 0xB3,
        "next" => 0xB0,
        "previous" => 0xB1,
        "slide_next" => 0x27,
        "slide_previous" => 0x25,
        _ => return None,
    })
}

pub fn is_power_action(action: &str) -> bool {
    matches!(action, "sleep" | "shutdown" | "shutdown_cancel")
}

pub fn is_action(action: &str) -> bool {
    key_vk(action).is_some() || action == "lock" || is_power_action(action)
}

pub fn is_try_action(action: &str) -> bool {
    matches!(
        action,
        "play_pause" | "previous" | "next" | "volume_down" | "volume_up" | "mute"
    )
}

#[cfg(windows)]
mod win_remote {
    #[link(name = "user32")]
    extern "system" {
        fn keybd_event(b_vk: u8, b_scan: u8, dw_flags: u32, dw_extra_info: usize);
        fn LockWorkStation() -> i32;
    }

    #[link(name = "powrprof")]
    extern "system" {
        fn SetSuspendState(b_hibernate: u8, b_force: u8, b_wakeup_events_disabled: u8) -> u8;
    }

    pub fn press(vk: u8) {
        unsafe {
            keybd_event(vk, 0, 0x1, 0); // KEYEVENTF_EXTENDEDKEY
            keybd_event(vk, 0, 0x1 | 0x2, 0); // KEYEVENTF_EXTENDEDKEY | KEYEVENTF_KEYUP
        }
    }

    pub fn lock() -> Result<(), String> {
        if unsafe { LockWorkStation() } == 0 {
            Err("LockWorkStation failed".into())
        } else {
            Ok(())
        }
    }

    pub fn sleep() -> Result<(), String> {
        if unsafe { SetSuspendState(0, 0, 0) } == 0 {
            Err("SetSuspendState failed".into())
        } else {
            Ok(())
        }
    }
}

#[cfg(not(windows))]
mod win_remote {
    pub fn press(_vk: u8) {}
    pub fn lock() -> Result<(), String> {
        Err("Only supported on Windows".into())
    }
    pub fn sleep() -> Result<(), String> {
        Err("Only supported on Windows".into())
    }
}

/// Do one allowlisted action. Refuses anything outside ACTIONS.
pub fn perform(action: &str) -> Result<(), String> {
    if let Some(vk) = key_vk(action) {
        win_remote::press(vk);
        return Ok(());
    }
    match action {
        "lock" => win_remote::lock(),
        "sleep" => win_remote::sleep(),
        "shutdown" => {
            let exe = super::program("shutdown")
                .ok_or_else(|| "shutdown helper not found".to_string())?;
            let status = super::command(&exe)
                .args([
                    "/s",
                    "/t",
                    SHUTDOWN_SECONDS,
                    "/c",
                    "Swiftlane: your phone asked to shut down this PC. Cancel it on the phone, or run: shutdown /a",
                ])
                .output()
                .map_err(|e| e.to_string())?;
            if status.status.success() {
                Ok(())
            } else {
                Err("shutdown returned an error".into())
            }
        }
        "shutdown_cancel" => {
            let exe = super::program("shutdown")
                .ok_or_else(|| "shutdown helper not found".to_string())?;
            let _ = super::command(&exe).arg("/a").output().map_err(|e| e.to_string())?;
            Ok(())
        }
        _ => Err("unknown action".into()),
    }
}

// ------------------------------------------------------------------ rate limit & log

struct RateWindow {
    times: VecDeque<Instant>,
}

impl RateWindow {
    const fn new() -> Self {
        Self {
            times: VecDeque::new(),
        }
    }

    /// Records a press at `now`. Returns `false` if >= 10 presses occurred within 2 seconds.
    fn allow(&mut self, now: Instant) -> bool {
        if self.times.len() == RATE_LIMIT_MAX {
            if let Some(&oldest) = self.times.front() {
                if now.duration_since(oldest) < RATE_LIMIT_WINDOW {
                    return false;
                }
            }
            self.times.pop_front();
        }
        self.times.push_back(now);
        true
    }
}

struct RemoteState {
    log: VecDeque<Value>,
    phone_names: HashMap<String, Option<String>>,
    rate: RateWindow,
}

static STATE: Mutex<Option<RemoteState>> = Mutex::new(None);

fn with_state<R>(f: impl FnOnce(&mut RemoteState) -> R) -> R {
    let mut guard = STATE.lock().unwrap();
    let st = guard.get_or_insert_with(|| RemoteState {
        log: VecDeque::with_capacity(MAX_LOG),
        phone_names: HashMap::new(),
        rate: RateWindow::new(),
    });
    f(st)
}

fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// Checks and records the phone remote rate limit (max 10 presses / 2 seconds).
#[allow(dead_code)] // called from transfer.rs once /api/remote is wired
pub fn allow_press() -> bool {
    with_state(|s| s.rate.allow(Instant::now()))
}

pub fn remote_state(app: &AppHandle) -> Value {
    let st = app.state::<AppState>();
    let cfg = st.cfg.lock().unwrap();
    let on = cfg["features"]["remote"].as_bool().unwrap_or(false);
    let power = cfg["remote_power"].as_bool().unwrap_or(false);
    let transfer_on = cfg["features"]["transfer"].as_bool().unwrap_or(true)
        && st.transfer.running.lock().unwrap().is_some();
    drop(cfg);
    let log: Vec<Value> = with_state(|s| s.log.iter().cloned().collect());
    json!({
        "on": on,
        "power": power,
        "transfer": transfer_on,
        "log": log,
    })
}

#[allow(dead_code)] // called from transfer.rs once /api/remote is wired
fn record_seen(app: &AppHandle, action: &str, ip: &str, problem: Option<&str>) {
    let need_lookup = with_state(|s| {
        let name = s.phone_names.get(ip).cloned().flatten();
        if s.log.len() >= MAX_LOG {
            s.log.pop_back();
        }
        s.log.push_front(json!({
            "action": action,
            "ip": ip,
            "name": name,
            "time": now_secs(),
            "error": problem,
        }));
        !s.phone_names.contains_key(ip)
    });
    push(app, "remote-press", remote_state(app));

    if need_lookup {
        let app = app.clone();
        let ip_owned = ip.to_string();
        std::thread::spawn(move || {
            let resolved = if ip_owned == "127.0.0.1" || ip_owned == "::1" {
                None
            } else {
                netscan::reverse_name(&ip_owned)
            };
            let has_name = resolved.is_some();
            with_state(|s| {
                s.phone_names.insert(ip_owned.clone(), resolved.clone());
                for entry in &mut s.log {
                    if entry["ip"].as_str() == Some(ip_owned.as_str()) {
                        entry["name"] = json!(resolved);
                    }
                }
            });
            if has_name {
                push(&app, "remote-press", remote_state(&app));
            }
        });
    }
}

/// Called by the phone transfer server for a paired phone's `/api/remote` press.
/// Returns `Ok(())` on success, or `Err(message)` to return to the phone with HTTP 403.
#[allow(dead_code)] // called from transfer.rs once /api/remote is wired
pub fn handle_phone_action(app: &AppHandle, action: &str, ip: &str) -> Result<(), String> {
    if !is_action(action) {
        return Err("Unknown action.".into());
    }
    let power_allowed = app
        .state::<AppState>()
        .cfg
        .lock()
        .unwrap()
        .get("remote_power")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if is_power_action(action) && !power_allowed {
        return Err(
            "Sleep and shut down from the phone are switched off in Swiftlane's settings.".into(),
        );
    }
    if is_power_action(action) || action == "lock" {
        config::log(&format!("PC remote from {ip}: {action}"));
    }
    if action == "shutdown" {
        crate::notify::show(
            app,
            "Shutting down in 60 seconds",
            "Your phone asked to shut down this PC. Cancel it on the phone, or run: shutdown /a",
        );
    }
    let problem = match perform(action) {
        Ok(()) => None,
        Err(e) => {
            config::log(&format!("PC remote {action} failed: {e}"));
            Some("That didn't work on the PC.".to_string())
        }
    };
    record_seen(app, action, ip, problem.as_deref());
    match problem {
        None => Ok(()),
        Some(msg) => Err(msg),
    }
}

pub fn dispatch(app: &AppHandle, method: &str, args: &[Value]) -> Option<Value> {
    Some(match method {
        "remote_state" => remote_state(app),
        "remote_try" => {
            let action = arg(args, 0).as_str().unwrap_or("");
            if !is_try_action(action) {
                return Some(json!({ "error": "Unknown action." }));
            }
            match perform(action) {
                Ok(()) => json!(true),
                Err(_) => json!({ "error": "That didn't work on this PC." }),
            }
        }
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_action_allowlists() {
        for a in [
            "volume_up",
            "volume_down",
            "mute",
            "play_pause",
            "next",
            "previous",
            "slide_next",
            "slide_previous",
            "lock",
            "sleep",
            "shutdown",
            "shutdown_cancel",
        ] {
            assert!(is_action(a), "{a} should be allowed");
        }
        assert!(is_power_action("sleep"));
        assert!(is_power_action("shutdown"));
        assert!(is_power_action("shutdown_cancel"));
        assert!(!is_power_action("lock"));
        assert!(!is_power_action("mute"));

        // remote_try never allows lock, slides, or power actions
        assert!(is_try_action("play_pause"));
        assert!(is_try_action("volume_up"));
        assert!(!is_try_action("lock"));
        assert!(!is_try_action("shutdown"));
        assert!(!is_try_action("slide_next"));

        // unknown commands are always refused
        assert!(!is_action("calc.exe"));
        assert!(!is_action(""));
        assert!(perform("calc.exe").is_err());
    }

    #[test]
    fn enforces_rate_limit_10_per_2_seconds() {
        let mut rw = RateWindow::new();
        let t0 = Instant::now();
        for i in 0..10 {
            assert!(rw.allow(t0 + Duration::from_millis(i * 100)));
        }
        assert!(!rw.allow(t0 + Duration::from_millis(1500)), "11th press within 2s must be blocked");
        assert!(rw.allow(t0 + Duration::from_millis(2100)), "allowed again once oldest ages past 2s");
    }
}
