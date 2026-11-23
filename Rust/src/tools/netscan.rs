//! Network scanner: which devices are on your home network right now.
//! Port of Source/toolkit/netscan.py plus net_* methods in Source/swiftlane.py.
//!
//! Only scans the private /24 network this PC is on (e.g. 192.168.0.1-254). Sends one
//! ICMP echo to each address via Windows' IcmpSendEcho (no admin rights needed), reads
//! Windows' ARP table, resolves names via router DNS, and remembers seen devices by MAC
//! address in %APPDATA%\Swiftlane\network.json so new devices stand out.

use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::net::{Ipv4Addr, UdpSocket};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Once};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Manager};

use super::arg;
use crate::api::{push, AppState};
use crate::config;
use crate::jobs::{Ctx, JobError};

/// The local IPv4 address Windows uses to reach the internet (sends no packets).
pub fn primary_ip() -> Option<String> {
    let sock = UdpSocket::bind("0.0.0.0:0").ok()?;
    sock.connect("8.8.8.8:80").ok()?;
    match sock.local_addr().ok()?.ip() {
        std::net::IpAddr::V4(v4) => Some(v4.to_string()),
        _ => None,
    }
}

/// The private /24 network around `ip`: returns `(cidr_string, [a, b, c])` only for RFC1918 networks.
pub fn home_network(ip: &str) -> Option<(String, [u8; 3])> {
    let addr: Ipv4Addr = ip.parse().ok()?;
    if !addr.is_private() {
        return None;
    }
    let [a, b, c, _] = addr.octets();
    Some((format!("{a}.{b}.{c}.0/24"), [a, b, c]))
}

fn in_slash24(ip: &str, prefix: [u8; 3]) -> bool {
    ip.parse::<Ipv4Addr>()
        .map(|a| {
            let o = a.octets();
            o[0] == prefix[0] && o[1] == prefix[1] && o[2] == prefix[2]
        })
        .unwrap_or(false)
}

#[cfg(windows)]
mod win_net {
    use std::ffi::c_void;
    use std::net::Ipv4Addr;

    #[link(name = "iphlpapi")]
    extern "system" {
        fn GetBestRoute(dw_dest_addr: u32, dw_source_addr: u32, best_route: *mut [u32; 14]) -> u32;
        fn IcmpCreateFile() -> *mut c_void;
        fn IcmpSendEcho(
            icmp_handle: *mut c_void,
            destination_address: u32,
            request_data: *const u8,
            request_size: u16,
            request_options: *const c_void,
            reply_buffer: *mut u8,
            reply_size: u32,
            timeout: u32,
        ) -> u32;
        fn IcmpCloseHandle(icmp_handle: *mut c_void) -> i32;
    }

    #[repr(C)]
    struct SockAddrIn {
        sin_family: u16,
        sin_port: u16,
        sin_addr: [u8; 4],
        sin_zero: [u8; 8],
    }

    #[link(name = "ws2_32")]
    extern "system" {
        fn getnameinfo(
            sa: *const SockAddrIn,
            salen: i32,
            host: *mut u8,
            hostlen: u32,
            serv: *mut u8,
            servlen: u32,
            flags: i32,
        ) -> i32;
    }

    pub fn gateway(ip: &str) -> Option<String> {
        let src_ip: Ipv4Addr = ip.parse().ok()?;
        let dest = u32::from_ne_bytes([8, 8, 8, 8]);
        let src = u32::from_ne_bytes(src_ip.octets());
        let mut row = [0u32; 14];
        let rc = unsafe { GetBestRoute(dest, src, &mut row) };
        if rc != 0 {
            return None;
        }
        let hop = Ipv4Addr::from(row[3].to_ne_bytes());
        if hop.is_unspecified() {
            None
        } else {
            Some(hop.to_string())
        }
    }

    pub fn ping(ip: &str, timeout_ms: u32) -> bool {
        let Ok(addr) = ip.parse::<Ipv4Addr>() else {
            return false;
        };
        let dest = u32::from_ne_bytes(addr.octets());
        unsafe {
            let h = IcmpCreateFile();
            if h.is_null() || h as isize == -1 {
                return false;
            }
            let data = b"swiftlane";
            let mut reply = [0u8; 256];
            let n = IcmpSendEcho(
                h,
                dest,
                data.as_ptr(),
                data.len() as u16,
                std::ptr::null(),
                reply.as_mut_ptr(),
                reply.len() as u32,
                timeout_ms,
            );
            let _ = IcmpCloseHandle(h);
            let status = u32::from_ne_bytes([reply[4], reply[5], reply[6], reply[7]]);
            n > 0 && status == 0
        }
    }

    pub fn reverse_lookup(ip: &str) -> Option<String> {
        let addr: Ipv4Addr = ip.parse().ok()?;
        let sa = SockAddrIn {
            sin_family: 2, // AF_INET
            sin_port: 0,
            sin_addr: addr.octets(),
            sin_zero: [0; 8],
        };
        let mut host = [0u8; 1025];
        let rc = unsafe {
            getnameinfo(
                &sa,
                std::mem::size_of::<SockAddrIn>() as i32,
                host.as_mut_ptr(),
                host.len() as u32,
                std::ptr::null_mut(),
                0,
                0x04, // NI_NAMEREQD
            )
        };
        if rc != 0 {
            return None;
        }
        let end = host.iter().position(|&b| b == 0).unwrap_or(host.len());
        let s = String::from_utf8_lossy(&host[..end]).to_string();
        if s.is_empty() || s == ip { None } else { Some(s) }
    }
}

#[cfg(not(windows))]
mod win_net {
    pub fn gateway(_ip: &str) -> Option<String> {
        None
    }
    pub fn ping(_ip: &str, _timeout_ms: u32) -> bool {
        false
    }
    pub fn reverse_lookup(_ip: &str) -> Option<String> {
        None
    }
}

/// The router (next hop) for `ip`, or None.
pub fn gateway(ip: &str) -> Option<String> {
    win_net::gateway(ip)
}

fn is_mac_token(s: &str) -> bool {
    let bytes = s.as_bytes();
    if bytes.len() != 17 {
        return false;
    }
    let sep = bytes[2];
    if sep != b'-' && sep != b':' {
        return false;
    }
    for (i, &b) in bytes.iter().enumerate() {
        if i % 3 == 2 {
            if b != sep {
                return false;
            }
        } else if !b.is_ascii_hexdigit() {
            return false;
        }
    }
    true
}

fn normalize_mac(s: &str) -> String {
    s.to_ascii_lowercase().replace('-', ":")
}

fn parse_arp_table(out: &str) -> HashMap<String, String> {
    let mut table = HashMap::new();
    for line in out.lines() {
        let mut parts = line.split_whitespace();
        let (Some(ip), Some(raw_mac)) = (parts.next(), parts.next()) else {
            continue;
        };
        if ip.parse::<Ipv4Addr>().is_err() || !is_mac_token(raw_mac) {
            continue;
        }
        let mac = normalize_mac(raw_mac);
        if mac == "ff:ff:ff:ff:ff:ff"
            || mac == "00:00:00:00:00:00"
            || mac.starts_with("01:00:5e")
        {
            continue;
        }
        table.insert(ip.to_string(), mac);
    }
    table
}

fn arp_table() -> HashMap<String, String> {
    let Some(arp) = super::program("arp") else {
        return HashMap::new();
    };
    let Ok(out) = super::command(&arp).arg("-a").output() else {
        return HashMap::new();
    };
    parse_arp_table(&String::from_utf8_lossy(&out.stdout))
}

fn parse_getmac(out: &str) -> Option<String> {
    let mut macs = Vec::new();
    for field in out.split('"') {
        if is_mac_token(field) {
            macs.push(normalize_mac(field));
        }
    }
    if macs.len() == 1 {
        macs.into_iter().next()
    } else {
        None
    }
}

fn own_mac() -> Option<String> {
    if !cfg!(windows) {
        return None;
    }
    let gm = super::program("getmac")?;
    let out = super::command(&gm).args(["/v", "/fo", "csv"]).output().ok()?;
    parse_getmac(&String::from_utf8_lossy(&out.stdout))
}

pub fn randomized(mac: &str) -> bool {
    mac.split(':')
        .next()
        .and_then(|first| u8::from_str_radix(first, 16).ok())
        .is_some_and(|b| (b & 0x02) != 0)
}

fn sanitize_name(name: &str) -> Option<String> {
    let cleaned: String = name
        .chars()
        .filter(|c| c.is_alphanumeric() || matches!(c, '_' | '.' | '-' | ' '))
        .take(80)
        .collect();
    if cleaned.is_empty() { None } else { Some(cleaned) }
}

fn sanitize_label(text: &str) -> String {
    text.chars()
        .filter(|c| (*c as u32) >= 0x20)
        .take(60)
        .collect()
}

pub fn reverse_name(ip: &str) -> Option<String> {
    let ip_owned = ip.to_string();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(win_net::reverse_lookup(&ip_owned));
    });
    let raw = rx.recv_timeout(Duration::from_millis(1500)).ok()??;
    if raw == ip {
        return None;
    }
    sanitize_name(&raw)
}

fn local_hostname() -> Option<String> {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .ok()
        .and_then(|s| sanitize_name(&s))
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn scan(
    progress: impl Fn(f64, usize) + Send + Sync,
    cancelled: impl Fn() -> bool + Send + Sync,
) -> Result<Value, JobError> {
    let Some(ip) = primary_ip() else {
        return Err(JobError::Failed(
            "You don't seem to be on a home or office network right now.".into(),
        ));
    };
    let Some((cidr, prefix)) = home_network(&ip) else {
        return Err(JobError::Failed(
            "You don't seem to be on a home or office network right now.".into(),
        ));
    };
    let gw = gateway(&ip);
    let hosts: Vec<String> = (1u8..=254u8)
        .map(|last| format!("{}.{}.{}.{}", prefix[0], prefix[1], prefix[2], last))
        .collect();
    let total_hosts = hosts.len();
    let queue = Arc::new(Mutex::new(VecDeque::from(hosts)));
    let alive = Arc::new(Mutex::new(HashSet::<String>::new()));
    let done = Arc::new(AtomicUsize::new(0));
    let progress_ref = &progress;
    let cancelled_ref = &cancelled;

    std::thread::scope(|scope| {
        for _ in 0..64 {
            let queue = Arc::clone(&queue);
            let alive = Arc::clone(&alive);
            let done = Arc::clone(&done);
            scope.spawn(move || loop {
                if cancelled_ref() {
                    break;
                }
                let next = queue.lock().unwrap().pop_front();
                let Some(host) = next else { break };
                let ok = win_net::ping(&host, 700);
                let count_alive = {
                    let mut set = alive.lock().unwrap();
                    if ok {
                        set.insert(host);
                    }
                    set.len()
                };
                let finished = done.fetch_add(1, Ordering::Relaxed) + 1;
                if finished % 8 == 0 {
                    progress_ref(finished as f64 / total_hosts as f64 * 0.85, count_alive);
                }
            });
        }
    });

    if cancelled() {
        return Err(JobError::Cancelled);
    }

    let arp: HashMap<String, String> = arp_table()
        .into_iter()
        .filter(|(k, _)| in_slash24(k, prefix))
        .collect();
    let alive_set = alive.lock().unwrap().clone();

    let mut found_set: HashSet<String> = alive_set.clone();
    for k in arp.keys() {
        found_set.insert(k.clone());
    }
    found_set.insert(ip.clone());

    let mut found: Vec<String> = found_set.into_iter().collect();
    found.sort_by_key(|a| a.parse::<Ipv4Addr>().ok());

    let name_queue = Arc::new(Mutex::new(VecDeque::from(found.clone())));
    let names = Arc::new(Mutex::new(HashMap::<String, Option<String>>::new()));
    let workers = found.len().min(32).max(1);
    std::thread::scope(|scope| {
        for _ in 0..workers {
            let name_queue = Arc::clone(&name_queue);
            let names = Arc::clone(&names);
            scope.spawn(move || loop {
                let next = name_queue.lock().unwrap().pop_front();
                let Some(addr) = next else { break };
                let resolved = reverse_name(&addr);
                names.lock().unwrap().insert(addr, resolved);
            });
        }
    });

    progress(1.0, found.len());

    let names = names.lock().unwrap();
    let self_host = local_hostname();
    let self_mac = own_mac();
    let mut devices = Vec::with_capacity(found.len());
    for addr in &found {
        let is_self = addr == &ip;
        let is_router = gw.as_deref() == Some(addr.as_str());
        let mac = arp
            .get(addr)
            .cloned()
            .or_else(|| if is_self { self_mac.clone() } else { None });
        let name = names
            .get(addr)
            .and_then(|o| o.clone())
            .or_else(|| if is_self { self_host.clone() } else { None });
        let is_rand = mac.as_deref().is_some_and(randomized);
        devices.push(json!({
            "ip": addr,
            "mac": mac,
            "name": name,
            "self": is_self,
            "router": is_router,
            "randomized": is_rand,
            "pings": alive_set.contains(addr),
        }));
    }

    let gw_mac = gw.as_deref().and_then(|g| arp.get(g)).cloned();
    Ok(json!({
        "network": cidr,
        "ip": ip,
        "gateway": gw,
        "gateway_mac": gw_mac,
        "devices": devices,
        "time": now_unix(),
    }))
}

// ------------------------------------------------------------------ remembered devices

pub struct Memory {
    path: PathBuf,
    data: Value,
}

impl Memory {
    pub fn load(path: PathBuf) -> Self {
        let data = std::fs::read_to_string(&path)
            .ok()
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
            .filter(|v| v.get("networks").and_then(Value::as_object).is_some())
            .unwrap_or_else(|| json!({ "networks": {} }));
        Self { path, data }
    }

    fn net_key(result: &Value) -> String {
        result["gateway_mac"]
            .as_str()
            .filter(|s| !s.is_empty())
            .or_else(|| result["network"].as_str())
            .unwrap_or("default")
            .to_string()
    }

    fn device_key(d: &Value) -> String {
        if let Some(mac) = d["mac"].as_str().filter(|s| !s.is_empty()) {
            mac.to_string()
        } else {
            format!("ip:{}", d["ip"].as_str().unwrap_or(""))
        }
    }

    fn net_devices_mut(&mut self, net_key: &str) -> &mut Map<String, Value> {
        let networks = self.data["networks"]
            .as_object_mut()
            .expect("networks object");
        let entry = networks
            .entry(net_key.to_string())
            .or_insert_with(|| json!({ "devices": {} }));
        if !entry.get("devices").is_some_and(Value::is_object) {
            entry["devices"] = json!({});
        }
        entry["devices"].as_object_mut().expect("devices object")
    }

    /// Marks each device new/known and remembers it. Returns the newly seen devices.
    pub fn merge(&mut self, result: &mut Value) -> Vec<Value> {
        let net_key = Self::net_key(result);
        let now = result["time"].as_i64().unwrap_or_else(now_unix);
        let mut newly_seen = Vec::new();
        let mut present_keys = HashSet::new();

        {
            let known = self.net_devices_mut(&net_key);
            let first_scan = known.is_empty();

            if let Some(devs) = result["devices"].as_array_mut() {
                for d in devs.iter_mut() {
                    let k = Self::device_key(d);
                    present_keys.insert(k.clone());
                    let is_self = d["self"].as_bool().unwrap_or(false);

                    let mut is_brand_new = false;
                    let seen = match known.get_mut(&k) {
                        Some(existing) => existing,
                        None => {
                            if !first_scan && !is_self {
                                is_brand_new = true;
                            }
                            known.insert(k.clone(), json!({ "first": now, "label": "" }));
                            known.get_mut(&k).unwrap()
                        }
                    };
                    seen["last"] = json!(now);
                    seen["ip"] = d["ip"].clone();
                    seen["name"] = d["name"].clone();
                    if is_brand_new {
                        seen["new"] = json!(true);
                    }
                    let first = seen["first"].clone();
                    let label = seen["label"].as_str().unwrap_or("").to_string();
                    let flag_new = seen["new"].as_bool().unwrap_or(false) || is_brand_new;

                    d["key"] = json!(k);
                    d["first"] = first;
                    d["label"] = json!(label);
                    d["new"] = json!(flag_new);

                    if is_brand_new {
                        newly_seen.push(d.clone());
                    }
                }
            }

            let mut missing: Vec<Value> = Vec::new();
            let ordered: BTreeMap<&String, &Value> = known.iter().collect();
            for (k, v) in ordered {
                if !present_keys.contains(k.as_str()) {
                    missing.push(json!({
                        "key": k,
                        "ip": v.get("ip").cloned().unwrap_or(Value::Null),
                        "name": v.get("name").cloned().unwrap_or(Value::Null),
                        "label": v.get("label").cloned().unwrap_or(Value::Null),
                        "last": v.get("last").cloned().unwrap_or(Value::Null),
                    }));
                }
            }
            result["missing"] = json!(missing);
        }

        self.save();
        newly_seen
    }

    pub fn label(&mut self, result: &Value, key: &str, text: &str) -> Option<String> {
        let net_key = Self::net_key(result);
        let cleaned = sanitize_label(text);
        {
            let known = self.net_devices_mut(&net_key);
            let dev = known.get_mut(key)?;
            dev["label"] = json!(cleaned);
            dev["new"] = json!(false);
        }
        self.save();
        Some(cleaned)
    }

    pub fn acknowledge(&mut self, result: &Value) {
        let net_key = Self::net_key(result);
        {
            let known = self.net_devices_mut(&net_key);
            for dev in known.values_mut() {
                dev["new"] = json!(false);
            }
        }
        self.save();
    }

    pub fn forget(&mut self, result: &Value, key: &str) {
        let net_key = Self::net_key(result);
        {
            let known = self.net_devices_mut(&net_key);
            known.remove(key);
        }
        self.save();
    }

    fn save(&self) {
        if let Some(parent) = self.path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let Ok(text) = serde_json::to_string_pretty(&self.data) else {
            return;
        };
        let mut tmp = self.path.as_os_str().to_os_string();
        tmp.push(".tmp");
        let tmp = PathBuf::from(tmp);
        if std::fs::write(&tmp, text).is_ok() {
            if std::fs::rename(&tmp, &self.path).is_err() {
                let _ = std::fs::remove_file(&tmp);
            }
        }
    }
}

// ------------------------------------------------------------------ state & page calls

static LAST_NET: Mutex<Option<Value>> = Mutex::new(None);
static SCAN_LOCK: Mutex<()> = Mutex::new(());
static WATCHER: Once = Once::new();

fn memory_path() -> PathBuf {
    config::config_dir().join("network.json")
}

fn net_state(app: &AppHandle) -> Value {
    let watch = app
        .state::<AppState>()
        .cfg
        .lock()
        .unwrap()
        .get("net_watch")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let result = LAST_NET.lock().unwrap().clone().unwrap_or(Value::Null);
    json!({ "result": result, "watch": watch })
}

fn run_and_store_scan(app: &AppHandle, ctx: Option<&Ctx>) -> Result<Value, JobError> {
    let (result, new_devs) = {
        let _guard = SCAN_LOCK.lock().unwrap();
        let mut res = scan(
            |f, n| {
                if let Some(c) = ctx {
                    c.report(Some(f), Some(&format!("{n} found")));
                }
            },
            || ctx.is_some_and(Ctx::cancelled),
        )?;
        let mut mem = Memory::load(memory_path());
        let new_devs = mem.merge(&mut res);
        *LAST_NET.lock().unwrap() = Some(res.clone());
        (res, new_devs)
    };

    for d in &new_devs {
        config::log(&format!(
            "Network: new device {} {} {}",
            d["ip"].as_str().unwrap_or(""),
            d["mac"].as_str().unwrap_or(""),
            d["name"].as_str().unwrap_or("")
        ));
    }
    if !new_devs.is_empty() {
        let names: Vec<&str> = new_devs
            .iter()
            .take(3)
            .map(|d| {
                d["name"]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| d["ip"].as_str().unwrap_or("device"))
            })
            .collect();
        let suffix = if new_devs.len() > 3 {
            format!(" and {} more", new_devs.len() - 3)
        } else {
            String::new()
        };
        crate::notify::show(app, "New device on your network", &format!("{}{}", names.join(", "), suffix));
    }
    push(app, "net", net_state(app));
    Ok(result)
}

fn ensure_watcher(app: &AppHandle) {
    let app = app.clone();
    WATCHER.call_once(move || {
        std::thread::spawn(move || loop {
            std::thread::sleep(Duration::from_secs(600));
            let enabled = {
                let st = app.state::<AppState>();
                let cfg = st.cfg.lock().unwrap();
                cfg.get("net_watch").and_then(Value::as_bool).unwrap_or(false)
                    && cfg["features"]["netscan"].as_bool().unwrap_or(true)
            };
            if enabled {
                if let Err(JobError::Failed(e)) = run_and_store_scan(&app, None) {
                    config::log(&format!("Network check skipped: {e}"));
                }
            }
        });
    });
}

pub fn dispatch(app: &AppHandle, method: &str, args: &[Value]) -> Option<Value> {
    if !matches!(method, "net_state" | "net_scan" | "net_label" | "net_ack" | "net_forget") {
        return None;
    }
    ensure_watcher(app);

    Some(match method {
        "net_state" => net_state(app),
        "net_scan" => {
            let id = app.state::<AppState>().jobs.start(
                app,
                "net",
                "Scan the home network",
                false,
                move |ctx| {
                    run_and_store_scan(ctx.app(), Some(ctx))?;
                    Ok(Vec::new())
                },
            );
            json!(id)
        }
        "net_label" => {
            let key = arg(args, 0).as_str().unwrap_or("");
            let text = arg(args, 1).as_str().unwrap_or("");
            let mut guard = LAST_NET.lock().unwrap();
            let Some(res) = guard.as_mut() else {
                return Some(Value::Null);
            };
            let mut mem = Memory::load(memory_path());
            let Some(cleaned) = mem.label(res, key, text) else {
                return Some(Value::Null);
            };
            if let Some(devs) = res["devices"].as_array_mut() {
                for d in devs {
                    if d["key"].as_str() == Some(key) {
                        d["label"] = json!(cleaned);
                        d["new"] = json!(false);
                    }
                }
            }
            drop(guard);
            net_state(app)
        }
        "net_ack" => {
            let mut guard = LAST_NET.lock().unwrap();
            if let Some(res) = guard.as_mut() {
                let mut mem = Memory::load(memory_path());
                mem.acknowledge(res);
                if let Some(devs) = res["devices"].as_array_mut() {
                    for d in devs {
                        d["new"] = json!(false);
                    }
                }
            }
            drop(guard);
            net_state(app)
        }
        "net_forget" => {
            let key = arg(args, 0).as_str().unwrap_or("");
            let mut guard = LAST_NET.lock().unwrap();
            if let Some(res) = guard.as_mut() {
                let mut mem = Memory::load(memory_path());
                mem.forget(res, key);
                if let Some(missing) = res["missing"].as_array_mut() {
                    missing.retain(|m| m["key"].as_str() != Some(key));
                }
            }
            drop(guard);
            net_state(app)
        }
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh_dir() -> PathBuf {
        let mut b = [0u8; 6];
        getrandom::getrandom(&mut b).unwrap();
        let dir = std::env::temp_dir().join(format!("swl-net-test-{}", hex::encode(b)));
        std::fs::create_dir(&dir).unwrap();
        dir
    }

    #[test]
    fn parses_arp_getmac_and_networks() {
        let arp = "\
Interface: 192.168.0.10 --- 0x5
  Internet Address      Physical Address      Type
  192.168.0.1           aa-bb-cc-dd-ee-ff     dynamic
  192.168.0.255         ff-ff-ff-ff-ff-ff     static
  224.0.0.22            01-00-5e-00-00-16     static
";
        let tbl = parse_arp_table(arp);
        assert_eq!(tbl.get("192.168.0.1").map(String::as_str), Some("aa:bb:cc:dd:ee:ff"));
        assert!(!tbl.contains_key("192.168.0.255"));
        assert!(!tbl.contains_key("224.0.0.22"));

        assert_eq!(
            parse_getmac("\"Wi-Fi\",\"Intel\",\"12-34-56-78-9A-BC\",\"\\Device\\Tcpip\"").as_deref(),
            Some("12:34:56:78:9a:bc")
        );
        assert!(randomized("02:11:22:33:44:55"));
        assert!(!randomized("00:11:22:33:44:55"));

        assert_eq!(home_network("192.168.1.42"), Some(("192.168.1.0/24".into(), [192, 168, 1])));
        assert_eq!(home_network("8.8.8.8"), None);
    }

    #[test]
    fn memory_tracks_new_missing_labels_and_ack() {
        let dir = fresh_dir();
        let mem_path = dir.join("network.json");

        // First scan: nothing flagged as new
        let mut scan1 = json!({
            "network": "192.168.0.0/24",
            "gateway_mac": "aa:bb:cc:dd:ee:01",
            "time": 1000,
            "devices": [
                { "ip": "192.168.0.1", "mac": "aa:bb:cc:dd:ee:01", "name": "router", "self": false },
                { "ip": "192.168.0.10", "mac": "aa:bb:cc:dd:ee:10", "name": "pc", "self": true }
            ]
        });
        let mut mem = Memory::load(mem_path.clone());
        let new1 = mem.merge(&mut scan1);
        assert!(new1.is_empty());
        assert_eq!(scan1["missing"].as_array().unwrap().len(), 0);

        // Second scan: router gone, phone joined
        let mut scan2 = json!({
            "network": "192.168.0.0/24",
            "gateway_mac": "aa:bb:cc:dd:ee:01",
            "time": 2000,
            "devices": [
                { "ip": "192.168.0.10", "mac": "aa:bb:cc:dd:ee:10", "name": "pc", "self": true },
                { "ip": "192.168.0.25", "mac": "02:aa:bb:cc:dd:25", "name": "phone", "self": false }
            ]
        });
        let new2 = mem.merge(&mut scan2);
        assert_eq!(new2.len(), 1);
        assert_eq!(new2[0]["ip"], "192.168.0.25");
        assert_eq!(scan2["missing"].as_array().unwrap().len(), 1);
        assert_eq!(scan2["missing"][0]["key"], "aa:bb:cc:dd:ee:01");

        // Label strips control chars and clears new
        let lbl = mem.label(&scan2, "02:aa:bb:cc:dd:25", "My\nPhone").unwrap();
        assert_eq!(lbl, "MyPhone");

        // Forget missing device
        mem.forget(&scan2, "aa:bb:cc:dd:ee:01");
        let reloaded = Memory::load(mem_path);
        assert!(reloaded.data["networks"]["aa:bb:cc:dd:ee:01"]["devices"].get("aa:bb:cc:dd:ee:01").is_none());

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
