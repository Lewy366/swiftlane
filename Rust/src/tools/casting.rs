//! Screen mirroring to this PC (the Python version's toolkit/airplay.py, toolkit/miracast.py,
//! cast.py and the airplay_* / miracast_* / cast_* methods in swiftlane.py):
//! - AirPlay (iPhone, iPad, Mac) through UxPlay (open source, GPL-3.0), a one-time download that's
//!   refused unless it matches the SHA-256 below, installed as administrator into its own Program
//!   Files folder by airplay-install.ps1 (which checks the zip again, in memory, before unpacking).
//!   It only runs while AirPlay is switched on and Swiftlane is open: a Windows job ends it when
//!   Swiftlane ends, even after a crash. A new phone has to type a random 4-digit PIN.
//! - Android (Miracast) through Windows' own Wireless Display app: Swiftlane only installs and
//!   opens it; nothing of Swiftlane's listens on the network for it.
//! - Laptops casting from a browser (hub.rs): every cast waits for Allow on this PC, and the
//!   picture shows in its own window (cast-view.html; double-click or F11 for full screen).

pub mod hub;

use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager};

use super::arg;
use crate::api::{push, AppState};
use crate::config;
use crate::jobs::JobError;

// ------------------------------------------------------------------ AirPlay receiver (UxPlay)

const RELEASE: &str = "airplay-receiver-1";
const URL: &str = "https://github.com/Lewy366/swiftlane/releases/download/airplay-receiver-1/airplay-receiver.zip";
// from GitHub run 44834256264 (UxPlay 48f5ce4); an empty value would mean "not available yet"
const SHA256: &str = "f6a983285750a6f17c3b55578ba269800bd344231dd360ed8f395be934e16696";
const SIZE: u64 = 77378682;
/// The firewall rule airplay-install.ps1 makes (only uxplay.exe, only Private networks).
#[allow(dead_code)]
pub const RULE: &str = "Swiftlane AirPlay";
const INSTALL_FOLDER: &str = "Swiftlane AirPlay";
const CHUNK: usize = 256 * 1024;
const SOURCE_URL: &str = "https://github.com/FDH2/UxPlay";

/// Program Files\Swiftlane AirPlay (only administrators can change it).
fn install_dir() -> PathBuf {
    win::program_files().join(INSTALL_FOLDER)
}

fn exe() -> PathBuf {
    install_dir().join("uxplay.exe")
}

pub fn installed() -> bool {
    exe().is_file()
}

fn available() -> bool {
    !SHA256.is_empty() && SIZE > 0
}

fn data_dir() -> PathBuf {
    config::config_dir().join("airplay")
}

fn register() -> PathBuf {
    data_dir().join("phones.txt")
}

fn pc_name() -> String {
    let n = std::env::var("COMPUTERNAME").or_else(|_| std::env::var("HOSTNAME")).unwrap_or_default();
    let n: String = n.chars().take(40).collect();
    if n.is_empty() { "PC".into() } else { n }
}

/// A random 4-digit PIN (1000-9999), without bias.
fn new_pin() -> String {
    loop {
        let mut b = [0u8; 4];
        getrandom::getrandom(&mut b).expect("Windows' random number source failed");
        let n = u32::from_le_bytes(b);
        if n < u32::MAX - u32::MAX % 9000 {
            return (n % 9000 + 1000).to_string();
        }
    }
}

#[derive(Default)]
struct Receiver {
    child: Option<Child>,
    run: u64, // which start this child belongs to (so the reader knows if it was stopped on purpose)
    pin: Option<String>,
    status: &'static str, // off | ready | showing | error
    client: Option<String>,
    error: Option<String>,
    log: VecDeque<String>,
}

static RECEIVER: LazyLock<Mutex<Receiver>> = LazyLock::new(|| Mutex::new(Receiver { status: "off", ..Default::default() }));
static RUNS: AtomicU64 = AtomicU64::new(0);
static INSTALLING: AtomicBool = AtomicBool::new(false);
static MIRACAST_INSTALLING: AtomicBool = AtomicBool::new(false);

fn running(r: &mut Receiver) -> bool {
    r.child.as_mut().is_some_and(|c| matches!(c.try_wait(), Ok(None)))
}

/// Phones that typed the PIN once and may connect again without it.
fn phones() -> Vec<Value> {
    let Ok(text) = std::fs::read(register()) else { return Vec::new() };
    parse_phones(&String::from_utf8_lossy(&text))
}

fn parse_phones(text: &str) -> Vec<Value> {
    let cut = |s: &str, n: usize| s.chars().take(n).collect::<String>();
    text.lines()
        .filter_map(|line| {
            // UxPlay writes one line per phone: public key, date added, device id, name
            let parts: Vec<&str> = line.splitn(4, ',').collect();
            (parts.len() == 4).then(|| {
                let name = cut(parts[3].trim(), 60);
                json!({ "name": if name.is_empty() { "Phone".into() } else { name },
                        "added": cut(parts[1].trim(), 20), "id": cut(&parts[2].trim().to_uppercase(), 17) })
            })
        })
        .collect()
}

pub fn airplay_state(app: &AppHandle) -> Value {
    let on = app.state::<AppState>().cfg.lock().unwrap()["airplay_on"].as_bool().unwrap_or(false);
    let r = RECEIVER.lock().unwrap();
    json!({
        "installed": installed(), "available": available(), "on": on, "status": r.status, "pin": r.pin,
        "client": r.client, "error": r.error, "name": format!("{} (Swiftlane)", pc_name()),
        "phones": phones(), "installing": INSTALLING.load(Ordering::SeqCst), "size": SIZE,
    })
}

/// What UxPlay reported, for the window (and a notification for the PIN).
fn airplay_event(app: &AppHandle, kind: &str, name: Option<&str>, pin: Option<&str>) {
    match kind {
        "pin" => crate::notify::show(app, "AirPlay PIN", &format!("Type {} on your iPhone", pin.unwrap_or(""))),
        "connect" => config::log(&format!("AirPlay: {} connected", name.unwrap_or(""))),
        "paired" => config::log(&format!("AirPlay: new phone allowed: {}", name.unwrap_or("None"))),
        _ => {}
    }
    push(app, "airplay", airplay_state(app));
}

fn uxplay_args(name: &str, pin: &str, reg: &Path, key: &Path) -> Vec<std::ffi::OsString> {
    let os = |s: &str| std::ffi::OsString::from(s);
    vec![
        os("-n"), os(name),
        os("-nh"),
        os("-pin"), os(pin),
        os("-reg"), reg.as_os_str().to_os_string(),
        os("-key"), key.as_os_str().to_os_string(),
        os("-vc"), os("d3d11upload ! d3d11convert"),
        os("-vs"), os("d3d11videosink"),
        os("-as"), os("wasapi2sink"),
        os("-fps"), os("60"),
        os("-al"), os("0.06"),
        os("-nohold"),
    ]
}

fn start_receiver(app: &AppHandle) -> Result<(), String> {
    {
        let mut r = RECEIVER.lock().unwrap();
        if running(&mut r) {
            return Ok(());
        }
        if !installed() {
            return Err("The AirPlay receiver isn't installed yet.".into());
        }
        let dir = data_dir();
        std::fs::create_dir_all(&dir).map_err(|e| format!("Couldn't start the AirPlay receiver ({e})."))?;
        let pin = new_pin();
        let install = install_dir();
        let mut cmd = super::command(&exe());
        // only the receiver's own parts, never anything from a user-writable folder
        for (k, _) in std::env::vars_os() {
            if k.to_string_lossy().to_uppercase().starts_with("GST_") {
                cmd.env_remove(&k);
            }
        }
        let mut path = std::ffi::OsString::from(install.as_os_str());
        path.push(";");
        path.push(win::system32().as_os_str());
        cmd.env("PATH", path)
            .env("GST_PLUGIN_SYSTEM_PATH_1_0", install.join("lib").join("gstreamer-1.0"))
            .env("GST_PLUGIN_PATH_1_0", "")
            .env("GST_PLUGIN_SCANNER_1_0", install.join("gst-plugin-scanner.exe"))
            .env("GST_REGISTRY_1_0", dir.join("plugins.bin"))
            .env("HOME", &dir)
            .current_dir(&dir)
            .args(uxplay_args(&format!("{} (Swiftlane)", pc_name()), &pin, &register(), &dir.join("key.pem")));
        let (reader, writer) = std::io::pipe().map_err(|e| format!("Couldn't start the AirPlay receiver ({e})."))?;
        let writer2 = writer.try_clone().map_err(|e| format!("Couldn't start the AirPlay receiver ({e})."))?;
        cmd.stdin(Stdio::null()).stdout(writer).stderr(writer2);
        let spawned = cmd.spawn();
        drop(cmd); // closes our copies of the pipe's writing end, so the reader sees when uxplay ends
        let mut child = spawned.map_err(|e| format!("Couldn't start the AirPlay receiver ({e})."))?;
        if !win::end_with_swiftlane(&child) {
            let _ = child.kill();
            let _ = child.wait();
            return Err("Windows wouldn't tie the AirPlay receiver to Swiftlane, so it wasn't started.".into());
        }
        let run = RUNS.fetch_add(1, Ordering::SeqCst) + 1;
        r.child = Some(child);
        r.run = run;
        r.pin = Some(pin);
        r.status = "ready";
        r.client = None;
        r.error = None;
        r.log.clear();
        let app2 = app.clone();
        std::thread::spawn(move || read_output(app2, reader, run));
    }
    airplay_event(app, "status", None, None);
    Ok(())
}

fn stop_receiver(app: &AppHandle) {
    let child = {
        let mut r = RECEIVER.lock().unwrap();
        r.status = "off";
        r.client = None;
        r.pin = None;
        r.child.take()
    };
    if let Some(mut c) = child {
        let _ = c.kill(); // Windows has no gentle "terminate" for a windowless program
        let _ = c.wait();
    }
    airplay_event(app, "status", None, None);
}

fn read_output(app: AppHandle, reader: std::io::PipeReader, run: u64) {
    let mut lines = std::io::BufReader::new(reader);
    let mut raw = Vec::new();
    loop {
        raw.clear();
        match lines.read_until(b'\n', &mut raw) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        let line = String::from_utf8_lossy(&raw).trim_end().to_string();
        if line.is_empty() {
            continue;
        }
        {
            let mut r = RECEIVER.lock().unwrap();
            if r.run != run {
                break;
            }
            r.log.push_back(line.clone());
            while r.log.len() > 60 {
                r.log.pop_front();
            }
        }
        handle_line(&app, &line, run);
    }
    // it stopped by itself (stop_receiver takes the child first, so this only finds it then)
    let child = {
        let mut r = RECEIVER.lock().unwrap();
        if r.run != run { None } else { r.child.take() }
    };
    let Some(mut child) = child else { return };
    let code = child.wait().ok().and_then(|s| s.code()).unwrap_or(-1);
    {
        let mut r = RECEIVER.lock().unwrap();
        if r.run == run {
            r.status = "error";
            r.client = None;
            let tail = r.log.iter().rev().find(|l| l.contains("ERROR")).or(r.log.back()).cloned();
            r.error = Some(format!("The AirPlay receiver stopped (code {code}).{}", tail.map(|t| format!(" {}", t.chars().take(200).collect::<String>())).unwrap_or_default()));
        }
    }
    airplay_event(&app, "status", None, None);
}

/// "connection request from NAME (MODEL) with deviceID = ID": the phone's name.
fn connecting_name(line: &str) -> Option<String> {
    const START: &str = "connection request from ";
    let rest = &line[line.find(START)? + START.len()..];
    let end = rest.find(") with deviceID = ")?;
    let after = &rest[end + ") with deviceID = ".len()..];
    if after.is_empty() || after.starts_with(char::is_whitespace) {
        return None;
    }
    let name_end = rest.get(1..)?.find(" (").map(|i| i + 1).filter(|i| *i < end)?;
    Some(rest[..name_end].trim().chars().take(60).collect())
}

fn handle_line(app: &AppHandle, line: &str, run: u64) {
    if line.contains("CLIENT MUST NOW ENTER PIN") {
        // a new phone is showing the PIN prompt right now
        let pin = RECEIVER.lock().unwrap().pin.clone();
        airplay_event(app, "pin", None, pin.as_deref());
        return;
    }
    if let Some(name) = connecting_name(line) {
        {
            let mut r = RECEIVER.lock().unwrap();
            if r.run != run {
                return;
            }
            r.client = Some(name.clone());
            r.status = "showing";
        }
        airplay_event(app, "connect", Some(&name), None);
        return;
    }
    if line.contains("registered new client") {
        let client = RECEIVER.lock().unwrap().client.clone();
        airplay_event(app, "paired", client.as_deref(), None);
        return;
    }
    if line.contains("on_video_stop") || line.contains("lost connection with client") || line.contains("Connection closed") {
        let changed = {
            let mut r = RECEIVER.lock().unwrap();
            if r.run == run && r.status == "showing" {
                r.status = "ready";
                r.client = None;
                true
            } else {
                false
            }
        };
        if changed {
            airplay_event(app, "status", None, None);
        }
    }
}

fn airplay_switch(app: &AppHandle, on: bool) -> Value {
    if on {
        if let Err(e) = start_receiver(app) {
            return json!({ "error": e });
        }
        config::log("AirPlay receiver on");
    } else {
        stop_receiver(app);
        config::log("AirPlay receiver off");
    }
    {
        let st = app.state::<AppState>();
        let mut cfg = st.cfg.lock().unwrap();
        cfg["airplay_on"] = json!(on);
        config::save(&cfg);
    }
    airplay_state(app)
}

fn airplay_forget(app: &AppHandle) -> Value {
    let was_running = running(&mut RECEIVER.lock().unwrap());
    if was_running {
        stop_receiver(app);
    }
    let _ = std::fs::remove_file(register());
    if was_running {
        if let Err(e) = start_receiver(app) {
            config::log(&format!("AirPlay receiver didn't start again: {e}"));
        }
    }
    config::log("AirPlay: forgot all phones (each needs the PIN again)");
    airplay_state(app)
}

/// Is this a Private (home) network? The firewall rule only covers those. Null if Windows won't say.
fn network_is_private() -> Value {
    match win::network_is_private() {
        Some(priv_net) => json!(priv_net),
        None => Value::Null,
    }
}

/// Fetch the receiver zip into `folder` (a fresh temp folder) and check its size and SHA-256 on
/// the way in. Returns the zip's path.
fn download(ctx: &crate::jobs::Ctx, folder: &Path) -> Result<PathBuf, JobError> {
    use sha2::{Digest, Sha256};
    if !available() {
        return Err(JobError::Failed("The AirPlay receiver isn't ready for download yet.".into()));
    }
    let interrupted = || JobError::Failed("The download was interrupted. Try again.".into());
    let path = folder.join("airplay-receiver.zip");
    let client = reqwest::blocking::Client::builder()
        .user_agent("Swiftlane")
        .https_only(true)
        .connect_timeout(Duration::from_secs(60))
        .timeout(None::<Duration>)
        .build()
        .map_err(|_| interrupted())?;
    let mut resp = client.get(URL).send().map_err(|_| interrupted())?;
    if !resp.status().is_success() {
        return Err(interrupted());
    }
    let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(&path).map_err(|_| interrupted())?;
    let mut hash = Sha256::new();
    let mut buf = vec![0u8; CHUNK];
    let (mut done, mut last) = (0u64, Instant::now());
    loop {
        ctx.check()?;
        let n = resp.read(&mut buf).map_err(|_| interrupted())?;
        if n == 0 {
            break;
        }
        done += n as u64;
        if done > SIZE {
            return Err(JobError::Failed("The download is bigger than expected, so it was rejected.".into()));
        }
        hash.update(&buf[..n]);
        file.write_all(&buf[..n]).map_err(|_| interrupted())?;
        if last.elapsed() > Duration::from_millis(250) {
            last = Instant::now();
            ctx.report(Some(done as f64 / SIZE as f64 * 0.9), Some("Downloading"));
        }
    }
    file.flush().map_err(|_| interrupted())?;
    if done != SIZE || hex::encode(hash.finalize()) != SHA256 {
        return Err(JobError::Failed("The download doesn't match the AirPlay receiver Swiftlane expects, so it was rejected.".into()));
    }
    Ok(path)
}

/// Download the receiver, check it, and install it as administrator (Windows asks).
fn airplay_install(app: &AppHandle) -> Value {
    if INSTALLING.swap(true, Ordering::SeqCst) {
        return Value::Null;
    }
    let app2 = app.clone();
    let id = app.state::<AppState>().jobs.start(app, "airplay", "AirPlay receiver", true, move |ctx| {
        let mut name = [0u8; 8];
        getrandom::getrandom(&mut name).expect("Windows' random number source failed");
        let folder = std::env::temp_dir().join(format!("Swiftlane-airplay-{}", hex::encode(name)));
        let result = (|| {
            std::fs::create_dir(&folder).map_err(|_| JobError::Failed("The download was interrupted. Try again.".into()))?;
            let zip = download(ctx, &folder)?;
            ctx.check()?;
            ctx.report(Some(0.92), Some("Installing: Windows asks for permission"));
            // next to Swiftlane.exe (Program Files once installed: only administrators can change it)
            let script = std::env::current_exe().ok().and_then(|e| e.parent().map(|d| d.join("airplay-install.ps1")))
                .filter(|p| p.is_file())
                .ok_or_else(|| JobError::Failed("Installing didn't work (the installer script is missing).".into()))?;
            let params = format!("-NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File \"{}\" -Zip \"{}\" -Sha256 {SHA256}", script.display(), zip.display());
            match win::run_elevated_wait(&win::powershell_exe(), &params) {
                None => Err(JobError::Failed("Windows didn't get permission, so nothing was installed.".into())),
                Some(code) if code != 0 || !installed() => Err(JobError::Failed(format!("Installing didn't work (code {code})."))),
                Some(_) => {
                    config::log(&format!("AirPlay receiver installed ({RELEASE})"));
                    Ok(Vec::new())
                }
            }
        })();
        let _ = std::fs::remove_dir_all(&folder);
        INSTALLING.store(false, Ordering::SeqCst);
        push(&app2, "airplay", airplay_state(&app2));
        result
    });
    json!(id)
}

// ------------------------------------------------------------------ Android (Miracast, Windows' own receiver)

const CAPABILITY: &str = "App.WirelessDisplay.Connect~~~~0.0.1.0";
const APP_ID: &str = "Microsoft.PPIProjection_cw5n1h2txyewy!Microsoft.PPIProjection"; // Wireless Display's Start menu ID

/// True/False if Windows says this PC's Wi-Fi and graphics can receive Miracast, Null if unknown.
/// Cached for the session because WDDM/Wi-Fi hardware capability doesn't change while Swiftlane runs.
fn miracast_supported() -> Value {
    static SUPPORTED: std::sync::OnceLock<Value> = std::sync::OnceLock::new();
    SUPPORTED
        .get_or_init(|| {
            let out = win::run_capture(&win::system32().join("netsh.exe"), &["wlan", "show", "drivers"], 30).unwrap_or_default();
            for line in out.lines() {
                if line.contains("Wireless Display Supported") {
                    return json!(line.split_once(':').map(|(_, v)| v.trim().to_lowercase().starts_with("yes")).unwrap_or(false));
                }
            }
            Value::Null
        })
        .clone()
}

/// Is the Wireless Display app installed? (Direct SystemApps check avoids spawning PowerShell.)
fn miracast_app() -> bool {
    let dir = win::windows_dir().join("SystemApps").join("Microsoft.PPIProjection_cw5n1h2txyewy");
    dir.join("Receiver.exe").is_file() || dir.join("AppxManifest.xml").is_file()
}

fn miracast_state() -> Value {
    json!({ "supported": miracast_supported(), "installed": miracast_app(), "installing": MIRACAST_INSTALLING.load(Ordering::SeqCst) })
}

/// Windows asks for admin, then downloads Wireless Display from Windows Update (a minute or two).
fn miracast_install() -> Value {
    if MIRACAST_INSTALLING.swap(true, Ordering::SeqCst) {
        return json!({ "error": "Already setting it up." });
    }
    let params = format!("-NoProfile -WindowStyle Hidden -Command \"Add-WindowsCapability -Online -Name '{CAPABILITY}' | Out-Null\"");
    let code = win::run_elevated_wait(&win::powershell_exe(), &params);
    MIRACAST_INSTALLING.store(false, Ordering::SeqCst);
    if code.is_none() {
        return json!({ "error": "Windows didn't get permission, so nothing was installed." });
    }
    if !miracast_app() {
        return json!({ "error": "Windows couldn't install Wireless Display. Try Settings > System > Optional features." });
    }
    config::log("Wireless Display (Miracast receiver) installed");
    miracast_state()
}

fn miracast_start() -> Value {
    if !miracast_app() {
        return json!({ "error": "Wireless Display isn't installed yet." });
    }
    let explorer = win::windows_dir().join("explorer.exe");
    match std::process::Command::new(explorer).arg(format!("shell:AppsFolder\\{APP_ID}")).spawn() {
        Ok(_) => json!(true),
        Err(e) => json!({ "error": e.to_string() }),
    }
}

// ------------------------------------------------------------------ cast from a browser (laptops)

static HUB: LazyLock<hub::CastHub> = LazyLock::new(hub::CastHub::default);
/// viewer window label -> request id
static VIEWERS: LazyLock<Mutex<HashMap<String, String>>> = LazyLock::new(|| Mutex::new(HashMap::new()));
static WATCHING_MAIN: AtomicBool = AtomicBool::new(false);
static VIEWER_IDS: AtomicU64 = AtomicU64::new(0);

/// The cast hub the phone server passes offers and answers through.
pub fn cast_hub() -> &'static hub::CastHub {
    &HUB
}

/// The window that shows a laptop's screen talks to Swiftlane through this instead of bridge.js:
/// it can only call the four viewer methods below, and gets none of the main window's events.
const VIEWER_BRIDGE: &str = r#"(() => {
  if (window.pywebview) return;
  const call = (method, args) => window.__TAURI_INTERNALS__.invoke('call', { method, args });
  window.pywebview = { api: new Proxy({}, { get: (_, m) => typeof m === 'string' ? (...a) => call(m, a) : undefined }) };
  window.addEventListener('DOMContentLoaded', () => window.dispatchEvent(new Event('pywebviewready')));
})();"#;

fn server_running(app: &AppHandle) -> bool {
    app.state::<AppState>().transfer.running.lock().unwrap().is_some()
}

pub fn cast_state(app: &AppHandle) -> Value {
    let st = app.state::<AppState>();
    let ca = st.transfer.ca.lock().unwrap().clone();
    let server = server_running(app);
    let link = match (HUB.token(), &ca, server) {
        (Some(token), Some(_), true) => {
            let port = st.cfg.lock().unwrap()["port"].as_u64().unwrap_or(8765);
            Some(format!("https://{}:{port}/cast#{token}", crate::transfer::local_addresses()[0].1))
        }
        _ => None,
    };
    json!({
        "on": HUB.on(), "link": link, "qr": link.as_deref().and_then(crate::transfer::qr_svg),
        "fingerprint": ca.as_ref().map(|c| c.fingerprint()).unwrap_or_default(), "server": server, "tls": ca.is_some(),
    })
}

fn cast_switch(app: &AppHandle, on: bool) -> Value {
    if on {
        if !server_running(app) {
            return json!({ "error": "Casting uses Phone transfer's connection. Switch Phone transfer on in Settings first." });
        }
        if app.state::<AppState>().transfer.ca.lock().unwrap().is_none() {
            return json!({ "error": "Encrypted connections aren't ready on this PC, and browsers only share screens over them." });
        }
        HUB.enable();
        watch_main_window(app);
        config::log("Cast from a browser on");
    } else {
        close_cast_windows(app);
        config::log("Cast from a browser off");
    }
    cast_state(app)
}

/// Switches casting off and closes every viewer window.
fn close_cast_windows(app: &AppHandle) {
    HUB.disable();
    let labels: Vec<String> = VIEWERS.lock().unwrap().drain().map(|(l, _)| l).collect();
    for label in labels {
        if let Some(w) = app.get_webview_window(&label) {
            let _ = w.destroy();
        }
    }
}

/// When the main window closes, casting switches off and the viewers close too. Otherwise
/// Swiftlane would keep running behind them, with share links and the vault still open.
fn watch_main_window(app: &AppHandle) {
    if WATCHING_MAIN.swap(true, Ordering::SeqCst) {
        return;
    }
    if let Some(main) = app.get_webview_window("main") {
        let app2 = app.clone();
        main.on_window_event(move |e| {
            if matches!(e, tauri::WindowEvent::Destroyed) {
                close_cast_windows(&app2);
            }
        });
    } else {
        WATCHING_MAIN.store(false, Ordering::SeqCst);
    }
}

/// A laptop wants to show its screen: ask on the PC (notification + a prompt in the window).
/// Called by the phone server after a valid offer.
pub fn announce(app: &AppHandle, o: &hub::Offer) {
    crate::notify::show(app, "Show a screen on this PC?", &format!("{} ({}) · Allow or deny it in Swiftlane", o.name, o.ip));
    push(app, "cast-offer", json!({ "id": o.id, "name": o.name, "ip": o.ip.to_string() }));
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
    }
}

fn close_soon(app: &AppHandle, label: String) {
    let app = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(2500));
        if let Some(w) = app.get_webview_window(&label) {
            let _ = w.destroy();
        }
    });
}

fn label_for(id: &str) -> Option<String> {
    VIEWERS.lock().unwrap().iter().find(|(_, oid)| *oid == id).map(|(l, _)| l.clone())
}

/// The laptop stopped sharing: say so in its window, then close it. Called by the phone server.
pub fn laptop_ended(app: &AppHandle, id: &str) {
    let Some(label) = label_for(id) else { return };
    if let Some(w) = app.get_webview_window(&label) {
        let _ = w.eval("window.castEnded && window.castEnded()");
    }
    close_soon(app, label);
}

fn cast_decide(app: &AppHandle, id: &str, allow: bool) -> Value {
    let Some(o) = HUB.decide(id, allow) else {
        return json!({ "error": "That request has expired." });
    };
    config::log(&format!("Cast from {} ({}) {}", o.name, o.ip, if allow { "allowed" } else { "denied" }));
    if allow {
        let label = format!("cast-{}", VIEWER_IDS.fetch_add(1, Ordering::SeqCst) + 1);
        VIEWERS.lock().unwrap().insert(label.clone(), o.id.clone());
        let built = tauri::WebviewWindowBuilder::new(app, &label, tauri::WebviewUrl::App("cast-view.html".into()))
            .title(format!("{} · Swiftlane", o.name))
            .inner_size(1280.0, 760.0)
            .min_inner_size(320.0, 200.0)
            .background_color(tauri::window::Color(0, 0, 0, 255))
            .initialization_script(VIEWER_BRIDGE)
            .build();
        match built {
            Ok(w) => {
                watch_main_window(app);
                let (oid, label2) = (o.id.clone(), label.clone());
                w.on_window_event(move |e| {
                    if matches!(e, tauri::WindowEvent::Destroyed) {
                        HUB.end(&oid, None); // the laptop's page sees it ended
                        VIEWERS.lock().unwrap().remove(&label2);
                    }
                });
            }
            Err(e) => {
                VIEWERS.lock().unwrap().remove(&label);
                HUB.end(&o.id, None);
                config::log(&format!("Couldn't open the cast window: {e}"));
                return json!({ "error": "Couldn't open a window for the picture." });
            }
        }
    }
    json!(true)
}

/// Calls from a viewer window (any window other than the main one). It may only fetch its own
/// request and answer it. None: not a viewer window, or not one of its methods.
pub fn viewer_call(app: &AppHandle, label: &str, method: &str, args: &[Value]) -> Option<Value> {
    let id = VIEWERS.lock().unwrap().get(label).cloned()?;
    Some(match method {
        "offer" => {
            let o = HUB.get(&id);
            let lang = app.state::<AppState>().cfg.lock().unwrap()["language"].as_str().unwrap_or("en").to_string();
            json!({ "sdp": o.filter(|o| o.status == hub::Status::Allowed).map(|o| o.sdp), "lang": lang })
        }
        "answer" => json!(arg(args, 0).as_str().is_some_and(|sdp| HUB.answer(&id, sdp))),
        "toggle_fullscreen" => {
            let w = app.get_webview_window(label)?;
            let full = !w.is_fullscreen().unwrap_or(false);
            let _ = w.set_fullscreen(full);
            json!(w.is_fullscreen().unwrap_or(full))
        }
        "ended" => {
            // the laptop stopped: show that for a moment, then close the window
            HUB.end(&id, None);
            close_soon(app, label.to_string());
            Value::Null
        }
        _ => return None,
    })
}

// ------------------------------------------------------------------ hooks for the rest of the app

/// Settings > a page switched off: nothing on it may keep listening (same as the Python version).
pub fn feature_off(app: &AppHandle, name: &str) {
    if name == "airplay" {
        stop_receiver(app);
    }
    if (name == "airplay" || name == "transfer") && HUB.on() {
        cast_switch(app, false); // casting lives on the same page and uses Phone transfer's connection
    }
}

/// When Swiftlane starts: AirPlay comes back on if it was on.
#[allow(dead_code)] // called from main.rs once it's wired up
pub fn start_with_swiftlane(app: &AppHandle) {
    let (feature, on) = {
        let st = app.state::<AppState>();
        let cfg = st.cfg.lock().unwrap();
        (cfg["features"]["airplay"].as_bool().unwrap_or(true), cfg["airplay_on"].as_bool().unwrap_or(false))
    };
    if feature && on && installed() {
        if let Err(e) = start_receiver(app) {
            config::log(&format!("AirPlay receiver didn't start: {e}"));
        }
    }
}

/// When Swiftlane closes: phones can no longer find this PC (the Windows job would end it anyway).
#[allow(dead_code)] // called from main.rs once it's wired up
pub fn shutdown(app: &AppHandle) {
    HUB.disable();
    let child = RECEIVER.lock().unwrap().child.take();
    if let Some(mut c) = child {
        let _ = c.kill();
        let _ = c.wait();
    }
    let _ = app;
}

pub fn dispatch(app: &AppHandle, method: &str, args: &[Value]) -> Option<Value> {
    let on = || arg(args, 0).as_bool().unwrap_or(false);
    Some(match method {
        "airplay_state" => airplay_state(app),
        "airplay_network" => network_is_private(),
        "airplay_switch" => airplay_switch(app, on()),
        "airplay_install" => airplay_install(app),
        "airplay_forget" => airplay_forget(app),
        "airplay_open_source" => {
            win::open_uri(SOURCE_URL);
            Value::Null
        }
        "miracast_state" => miracast_state(),
        "miracast_install" => miracast_install(),
        "miracast_start" => miracast_start(),
        "miracast_settings" => {
            win::open_uri("ms-settings:project");
            Value::Null
        }
        "cast_state" => cast_state(app),
        "cast_switch" => cast_switch(app, on()),
        "cast_decide" => {
            let id = match arg(args, 0) {
                Value::String(s) => s.clone(),
                Value::Null => String::new(),
                v => v.to_string(),
            };
            cast_decide(app, &id, arg(args, 1).as_bool().unwrap_or(false))
        }
        _ => return None,
    })
}

// ------------------------------------------------------------------ Windows helpers

#[cfg(windows)]
mod win {
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::AsRawHandle;
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;

    // kernel32 functions windows-sys only has behind features Swiftlane doesn't switch on
    #[repr(C)]
    #[derive(Default)]
    struct BasicLimits {
        per_process_user_time_limit: i64,
        per_job_user_time_limit: i64,
        limit_flags: u32,
        minimum_working_set_size: usize,
        maximum_working_set_size: usize,
        active_process_limit: u32,
        affinity: usize,
        priority_class: u32,
        scheduling_class: u32,
    }
    #[repr(C)]
    #[derive(Default)]
    struct ExtendedLimits {
        basic: BasicLimits,
        io: [u64; 6],
        process_memory_limit: usize,
        job_memory_limit: usize,
        peak_process_memory_used: usize,
        peak_job_memory_used: usize,
    }
    const JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE: u32 = 0x2000;
    const JOB_OBJECT_EXTENDED_LIMIT_INFORMATION: i32 = 9;

    #[link(name = "kernel32")]
    extern "system" {
        fn CreateJobObjectW(attributes: *const c_void, name: *const u16) -> *mut c_void;
        fn SetInformationJobObject(job: *mut c_void, class: i32, info: *const c_void, length: u32) -> i32;
        fn AssignProcessToJobObject(job: *mut c_void, process: *mut c_void) -> i32;
        fn WaitForSingleObject(handle: *mut c_void, milliseconds: u32) -> u32;
        fn GetExitCodeProcess(process: *mut c_void, code: *mut u32) -> i32;
        fn CloseHandle(handle: *mut c_void) -> i32;
    }

    /// The job handle, kept open for as long as Swiftlane runs; Windows closes it when Swiftlane
    /// ends (even after a crash), and that ends every program in it.
    static JOB: Mutex<usize> = Mutex::new(0);

    /// Put the program in a Windows job that ends it when Swiftlane ends.
    pub fn end_with_swiftlane(child: &std::process::Child) -> bool {
        let mut job = JOB.lock().unwrap();
        unsafe {
            if *job == 0 {
                let h = CreateJobObjectW(std::ptr::null(), std::ptr::null());
                if h.is_null() {
                    return false;
                }
                let mut info = ExtendedLimits::default();
                info.basic.limit_flags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                if SetInformationJobObject(h, JOB_OBJECT_EXTENDED_LIMIT_INFORMATION, &info as *const _ as *const c_void, std::mem::size_of::<ExtendedLimits>() as u32) == 0 {
                    CloseHandle(h);
                    return false;
                }
                *job = h as usize;
            }
            AssignProcessToJobObject(*job as *mut c_void, child.as_raw_handle() as *mut c_void) != 0
        }
    }

    fn wide(s: &std::ffi::OsStr) -> Vec<u16> {
        s.encode_wide().chain(std::iter::once(0)).collect()
    }

    fn known_folder(id: &windows::core::GUID) -> Option<PathBuf> {
        use windows::Win32::System::Com::CoTaskMemFree;
        use windows::Win32::UI::Shell::{SHGetKnownFolderPath, KNOWN_FOLDER_FLAG};
        unsafe {
            let p = SHGetKnownFolderPath(id, KNOWN_FOLDER_FLAG(0), None).ok()?;
            let s = p.to_string().ok();
            CoTaskMemFree(Some(p.0 as *const c_void));
            s.filter(|s| Path::new(s).is_absolute()).map(PathBuf::from)
        }
    }

    /// Program Files as Windows itself knows it (not from an environment variable).
    pub fn program_files() -> PathBuf {
        known_folder(&windows::Win32::UI::Shell::FOLDERID_ProgramFiles).unwrap_or_else(|| PathBuf::from(r"C:\Program Files"))
    }

    pub fn windows_dir() -> PathBuf {
        known_folder(&windows::Win32::UI::Shell::FOLDERID_Windows).unwrap_or_else(|| PathBuf::from(r"C:\Windows"))
    }

    pub fn system32() -> PathBuf {
        known_folder(&windows::Win32::UI::Shell::FOLDERID_System).unwrap_or_else(|| windows_dir().join("System32"))
    }

    pub fn powershell_exe() -> PathBuf {
        system32().join("WindowsPowerShell").join("v1.0").join("powershell.exe")
    }

    /// Starts `exe` as administrator (Windows asks), hidden, and waits up to 10 minutes for it.
    /// Its exit code, or None if the user said no.
    pub fn run_elevated_wait(exe: &Path, params: &str) -> Option<u32> {
        use windows_sys::Win32::UI::Shell::{ShellExecuteExW, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW};
        let verb = wide("runas".as_ref());
        let file = wide(exe.as_os_str());
        let args = wide(params.as_ref());
        // never Swiftlane's own folder, or Setup couldn't replace it while this runs
        let dir = wide(std::env::temp_dir().as_os_str());
        unsafe {
            let mut info: SHELLEXECUTEINFOW = std::mem::zeroed();
            info.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
            info.fMask = SEE_MASK_NOCLOSEPROCESS;
            info.lpVerb = verb.as_ptr();
            info.lpFile = file.as_ptr();
            info.lpParameters = args.as_ptr();
            info.lpDirectory = dir.as_ptr();
            info.nShow = 0; // SW_HIDE
            if ShellExecuteExW(&mut info) == 0 || info.hProcess.is_null() {
                return None;
            }
            let h = info.hProcess as *mut c_void;
            WaitForSingleObject(h, 10 * 60 * 1000);
            let mut code = 0u32;
            GetExitCodeProcess(h, &mut code);
            CloseHandle(h);
            Some(code)
        }
    }

    /// Opens a fixed address (a web page or a Settings page) with Windows.
    pub fn open_uri(uri: &str) {
        let verb = wide("open".as_ref());
        let target = wide(uri.as_ref());
        unsafe {
            windows_sys::Win32::UI::Shell::ShellExecuteW(std::ptr::null_mut(), verb.as_ptr(), target.as_ptr(), std::ptr::null(), std::ptr::null(), 1);
        }
    }

    /// Runs a Windows program (by full path) and returns what it printed, or None if it failed
    /// or took longer than `timeout` seconds.
    pub fn run_capture(exe: &Path, args: &[&str], timeout: u64) -> Option<String> {
        use std::io::Read;
        let mut child = super::super::command(exe).args(args).stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::null()).spawn().ok()?;
        let mut out = child.stdout.take()?;
        let reader = std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = out.read_to_end(&mut buf);
            buf
        });
        let started = std::time::Instant::now();
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if started.elapsed().as_secs() < timeout => std::thread::sleep(std::time::Duration::from_millis(100)),
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
            }
        }
        reader.join().ok().map(|b| String::from_utf8_lossy(&b).to_string())
    }

    /// Checks whether every connected network profile is Private or DomainAuthenticated via COM
    /// INetworkListManager (avoids spawning PowerShell).
    pub fn network_is_private() -> Option<bool> {
        use windows::Win32::Foundation::RPC_E_CHANGED_MODE;
        use windows::Win32::Networking::NetworkListManager::{
            INetworkListManager, NetworkListManager, NLM_ENUM_NETWORK_CONNECTED,
            NLM_NETWORK_CATEGORY_DOMAIN_AUTHENTICATED, NLM_NETWORK_CATEGORY_PRIVATE,
        };
        use windows::Win32::System::Com::{
            CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_ALL, COINIT_MULTITHREADED,
        };

        struct ComGuard(bool);
        impl Drop for ComGuard {
            fn drop(&mut self) {
                if self.0 {
                    unsafe {
                        CoUninitialize();
                    }
                }
            }
        }

        unsafe {
            let init = CoInitializeEx(None, COINIT_MULTITHREADED);
            if init.is_err() && init != RPC_E_CHANGED_MODE {
                return None;
            }
            let _guard = ComGuard(init.is_ok());
            let mgr: INetworkListManager = CoCreateInstance(&NetworkListManager, None, CLSCTX_ALL).ok()?;
            let enums = mgr.GetNetworks(NLM_ENUM_NETWORK_CONNECTED).ok()?;
            let (mut saw_any, mut all_private) = (false, true);
            loop {
                let mut buf = [None];
                let mut fetched = 0u32;
                if enums.Next(&mut buf, Some(&mut fetched)).is_err() || fetched == 0 {
                    break;
                }
                if let Some(net) = buf[0].take() {
                    if let Ok(cat) = net.GetCategory() {
                        saw_any = true;
                        if cat != NLM_NETWORK_CATEGORY_PRIVATE && cat != NLM_NETWORK_CATEGORY_DOMAIN_AUTHENTICATED {
                            all_private = false;
                        }
                    }
                }
            }
            saw_any.then_some(all_private)
        }
    }
}

#[cfg(not(windows))]
mod win {
    use std::path::{Path, PathBuf};
    pub fn end_with_swiftlane(_child: &std::process::Child) -> bool {
        false
    }
    pub fn program_files() -> PathBuf {
        PathBuf::from("/nonexistent")
    }
    pub fn windows_dir() -> PathBuf {
        PathBuf::from("/nonexistent")
    }
    pub fn system32() -> PathBuf {
        PathBuf::from("/nonexistent")
    }
    pub fn powershell_exe() -> PathBuf {
        PathBuf::from("/nonexistent/powershell")
    }
    pub fn run_elevated_wait(_exe: &Path, _params: &str) -> Option<u32> {
        None
    }
    pub fn open_uri(_uri: &str) {}
    pub fn run_capture(_exe: &Path, _args: &[&str], _timeout: u64) -> Option<String> {
        None
    }
    pub fn network_is_private() -> Option<bool> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pins_are_four_digits() {
        for _ in 0..200 {
            let p: u32 = new_pin().parse().unwrap();
            assert!((1000..=9999).contains(&p));
        }
    }

    #[test]
    fn reads_uxplay_lines() {
        let line = "raop_rtp_mirror connection request from Lewy's iPhone (iPhone15,2) with deviceID = AA:BB:CC:DD:EE:FF";
        assert_eq!(connecting_name(line).as_deref(), Some("Lewy's iPhone"));
        assert_eq!(connecting_name("connection request from X (Y) with deviceID = ").as_deref(), None);
        assert_eq!(connecting_name("nothing here"), None);
    }

    #[test]
    fn reads_the_phone_list() {
        let text = "KEY1,2026-01-02,aa:bb:cc:dd:ee:ff,My iPhone\nbad line\nKEY2,2026-02-03,11:22, Pad, with comma \n";
        let p = parse_phones(text);
        assert_eq!(p.len(), 2);
        assert_eq!(p[0]["name"], "My iPhone");
        assert_eq!(p[0]["id"], "AA:BB:CC:DD:EE:FF");
        assert_eq!(p[0]["added"], "2026-01-02");
        assert_eq!(p[1]["name"], "Pad, with comma");
    }

    #[test]
    fn constants_unchanged() {
        assert_eq!(SHA256.len(), 64);
        assert!(URL.contains(RELEASE));
        assert_eq!(INSTALL_FOLDER, "Swiftlane AirPlay");
        assert_eq!(RULE, "Swiftlane AirPlay");
    }

    #[test]
    fn uxplay_launch_args_enable_fast_pipeline() {
        let args = uxplay_args("MyPC (Swiftlane)", "1234", Path::new("phones.txt"), Path::new("key.pem"));
        let strs: Vec<String> = args.iter().map(|s| s.to_string_lossy().into_owned()).collect();
        for pair in [
            ("-vc", "d3d11upload ! d3d11convert"),
            ("-vs", "d3d11videosink"),
            ("-as", "wasapi2sink"),
            ("-fps", "60"),
            ("-al", "0.06"),
        ] {
            let idx = strs.iter().position(|s| s == pair.0).expect(pair.0);
            assert_eq!(strs.get(idx + 1).map(String::as_str), Some(pair.1));
        }
        assert!(strs.iter().any(|s| s == "-nohold"));
    }

    #[cfg(windows)]
    #[test]
    fn helper_paths_are_absolute() {
        assert!(win::powershell_exe().is_absolute());
        assert!(win::system32().is_absolute());
        assert!(install_dir().is_absolute() && install_dir().ends_with("Swiftlane AirPlay"));
    }

    #[cfg(windows)]
    #[test]
    fn native_network_and_miracast_checks_do_not_panic() {
        let _ = win::network_is_private();
        let _ = miracast_app();
    }
}
