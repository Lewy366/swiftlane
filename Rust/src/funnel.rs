//! Tailscale Funnel: switched on (pointing at the share server) only while something is shared,
//! and off again afterwards. Only ever touches its own entry; if something else already uses
//! Funnel/Serve on port 443, it leaves it alone. Same behaviour as share.py's Funnel class.

use serde_json::{json, Value};
use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::config;
use crate::share::SHARE_PORT;

fn target() -> String {
    format!("http://127.0.0.1:{SHARE_PORT}")
}

fn exe() -> Option<PathBuf> {
    let default = PathBuf::from(std::env::var_os("ProgramFiles").unwrap_or_else(|| r"C:\Program Files".into())).join("Tailscale").join("tailscale.exe");
    if default.exists() {
        return Some(default);
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join(if cfg!(windows) { "tailscale.exe" } else { "tailscale" })).find(|p| p.exists())
}

/// Runs tailscale with these arguments (no window), giving up after `timeout`. (exit code, output)
fn call(args: &[&str], timeout: Duration) -> Result<(i32, String), String> {
    let exe = exe().ok_or("Tailscale isn't installed.")?;
    let mut cmd = Command::new(exe);
    cmd.args(args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let mut child = cmd.spawn().map_err(|e| format!("Couldn't start Tailscale: {e}"))?;
    let (mut out, mut err) = (child.stdout.take().unwrap(), child.stderr.take().unwrap());
    let reader = std::thread::spawn(move || {
        let (mut a, mut b) = (String::new(), String::new());
        let _ = out.read_to_string(&mut a);
        let _ = err.read_to_string(&mut b);
        a + &b
    });
    let started = Instant::now();
    let code = loop {
        if let Ok(Some(status)) = child.try_wait() {
            break status.code().unwrap_or(1);
        }
        if started.elapsed() > timeout {
            let _ = child.kill();
            let _ = child.wait();
            break 124;
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    Ok((code, reader.join().unwrap_or_default()))
}

/// (our entry is on, something else uses 443)
fn config() -> (bool, bool) {
    let Ok((_, out)) = call(&["funnel", "status", "--json"], Duration::from_secs(10)) else { return (false, false) };
    let cfg: Value = serde_json::from_str(if out.trim().is_empty() { "{}" } else { &out }).unwrap_or(json!({}));
    let (mut mine, mut other) = (false, false);
    if let Some(web) = cfg["Web"].as_object() {
        for (hostport, w) in web {
            if !hostport.ends_with(":443") {
                continue;
            }
            for h in w["Handlers"].as_object().into_iter().flat_map(|m| m.values()) {
                if h["Proxy"].as_str().unwrap_or("").trim_end_matches('/') == target() {
                    mine = cfg["AllowFunnel"][hostport].as_bool().unwrap_or(false);
                } else {
                    other = true;
                }
            }
        }
    }
    if cfg["TCP"].get("443").is_some() && cfg["Web"].as_object().map_or(true, |w| w.is_empty()) {
        other = true;
    }
    (mine, other)
}

/// What the Share page shows: is Tailscale ready, is Funnel allowed, is our link on?
pub fn status() -> Value {
    if exe().is_none() {
        return json!({ "state": "no-tailscale" });
    }
    let Ok((_, out)) = call(&["status", "--json"], Duration::from_secs(10)) else { return json!({ "state": "not-running" }) };
    let Ok(data) = serde_json::from_str::<Value>(&out) else { return json!({ "state": "not-running" }) };
    if data["BackendState"] != "Running" {
        return json!({ "state": "not-running" });
    }
    let me = &data["Self"];
    let host = me["DNSName"].as_str().unwrap_or("").trim_end_matches('.').to_string();
    let mut caps: Vec<String> = me["Capabilities"].as_array().into_iter().flatten().filter_map(|c| c.as_str().map(String::from)).collect();
    caps.extend(me["CapMap"].as_object().into_iter().flat_map(|m| m.keys().cloned()));
    let https = caps.iter().any(|c| c == "https" || c.ends_with("/https"));
    let funnel_ok = caps.iter().any(|c| c.contains("funnel"));
    let (mine, other) = config();
    let mut state = if mine { "on" } else if !https { "needs-https" } else if !funnel_ok { "needs-funnel" } else { "off" };
    if other && !mine {
        state = "in-use";
    }
    json!({ "state": state, "host": host, "url": (!host.is_empty()).then(|| format!("https://{host}")) })
}

/// Ok({"ok": true}) or Ok({"approve": url}) when Tailscale wants the owner's OK first.
pub fn on() -> Result<Value, String> {
    let (mine, other) = config();
    if mine {
        return Ok(json!({ "ok": true }));
    }
    if other {
        return Err("Tailscale Funnel or Serve is already used by something else on this PC, so Swiftlane won't change it.".into());
    }
    let (_, out) = call(&["funnel", "--bg", &target()], Duration::from_secs(25))?;
    if let Some(i) = out.find("https://login.tailscale.com/") {
        let url: String = out[i..].chars().take_while(|c| !c.is_whitespace()).collect();
        return Ok(json!({ "approve": url }));
    }
    if !config().0 {
        return Err(format!("Tailscale didn't switch Funnel on: {}", out.trim().chars().take(200).collect::<String>()));
    }
    config::log("Tailscale Funnel on (sharing)");
    Ok(json!({ "ok": true }))
}

pub fn off() {
    if !config().0 {
        return;
    }
    let _ = call(&["funnel", "--https=443", "off"], Duration::from_secs(20));
    config::log("Tailscale Funnel off (nothing shared)");
}

// ------------------------------------------------------------------ is the link really reachable from the internet?

const PUBLIC_DNS: &[&str] = &["1.1.1.1", "8.8.8.8", "9.9.9.9"];

/// IPv4 addresses for `host` as the rest of the internet sees them (asks public DNS, not this PC's
/// Tailscale DNS, which would give the private tailnet address).
fn public_addresses(host: &str) -> Vec<String> {
    let mut id = [0u8; 2];
    let _ = getrandom::getrandom(&mut id);
    let mut q = vec![id[0], id[1], 0x01, 0x00, 0, 1, 0, 0, 0, 0, 0, 0];
    for part in host.split('.') {
        if part.is_empty() || part.len() > 63 {
            return vec![];
        }
        q.push(part.len() as u8);
        q.extend_from_slice(part.as_bytes());
    }
    q.extend_from_slice(&[0, 0, 1, 0, 1]);
    for server in PUBLIC_DNS {
        let Ok(sock) = std::net::UdpSocket::bind("0.0.0.0:0") else { continue };
        let _ = sock.set_read_timeout(Some(Duration::from_secs(4)));
        if sock.send_to(&q, (*server, 53)).is_err() {
            continue;
        }
        let mut buf = [0u8; 1500];
        let Ok(n) = sock.recv(&mut buf) else { continue };
        if let Some(ips) = parse_answer(&buf[..n], q.len(), &id) {
            if !ips.is_empty() {
                return ips;
            }
        }
    }
    vec![]
}

fn parse_answer(d: &[u8], qlen: usize, id: &[u8; 2]) -> Option<Vec<String>> {
    if d.len() < 12 || d[..2] != id[..] {
        return None;
    }
    let count = u16::from_be_bytes([d[6], d[7]]);
    let (mut i, mut ips) = (qlen, Vec::new());
    for _ in 0..count {
        if *d.get(i)? & 0xC0 == 0xC0 {
            i += 2;
        } else {
            while *d.get(i)? != 0 {
                i += *d.get(i)? as usize + 1;
            }
            i += 1;
        }
        let rtype = u16::from_be_bytes([*d.get(i)?, *d.get(i + 1)?]);
        let len = u16::from_be_bytes([*d.get(i + 8)?, *d.get(i + 9)?]) as usize;
        i += 10;
        if rtype == 1 && len == 4 {
            ips.push(format!("{}.{}.{}.{}", d.get(i)?, d.get(i + 1)?, d.get(i + 2)?, d.get(i + 3)?));
        }
        i += len;
    }
    Some(ips)
}

/// One try: fetch https://<host>/.well-known/swiftlane-ready/<nonce> the way a stranger's browser
/// would (public DNS, Tailscale's internet entrance, Windows' own certificate checking through its
/// built-in curl). Some(true) only if our own share server answered; None if this PC can't check.
pub fn reachable(host: &str, nonce: &str) -> (Option<bool>, String) {
    let curl = PathBuf::from(std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into())).join("System32").join("curl.exe");
    if !curl.exists() {
        return (None, "can't check on this Windows".into());
    }
    let ips = public_addresses(host);
    if ips.is_empty() {
        return (Some(false), "not in public DNS yet".into());
    }
    let mut why = String::new();
    for ip in ips.iter().take(2) {
        let mut cmd = Command::new(&curl);
        cmd.args(["-sS", "-o", "NUL", "--max-time", "12", "--resolve", &format!("{host}:443:{ip}"), "-w", "%{http_code} %{ssl_verify_result}",
            &format!("https://{host}{}{nonce}", crate::share_server::READY_PATH)]);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000);
        }
        match cmd.output() {
            Ok(o) if String::from_utf8_lossy(&o.stdout).trim() == "204 0" => return (Some(true), "ok".into()),
            Ok(o) => why = String::from_utf8_lossy(if o.stderr.is_empty() { &o.stdout } else { &o.stderr }).trim().chars().take(120).collect(),
            Err(e) => why = e.to_string(),
        }
    }
    (Some(false), why)
}

/// Tell Windows the PC is in use (called regularly while links are active).
pub fn keep_awake() {
    #[cfg(windows)]
    unsafe {
        windows_sys::Win32::System::Power::SetThreadExecutionState(windows_sys::Win32::System::Power::ES_SYSTEM_REQUIRED);
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn dns_answer() {
        // a real-shaped answer: 1 question (example.com A), 1 answer 93.184.216.34
        let id = [0x12, 0x34];
        let mut q = vec![0x12, 0x34, 0x81, 0x80, 0, 1, 0, 1, 0, 0, 0, 0];
        q.extend_from_slice(b"\x07example\x03com\x00\x00\x01\x00\x01");
        let qlen = q.len();
        q.extend_from_slice(&[0xC0, 0x0C, 0, 1, 0, 1, 0, 0, 0, 60, 0, 4, 93, 184, 216, 34]);
        assert_eq!(super::parse_answer(&q, qlen, &id), Some(vec!["93.184.216.34".to_string()]));
        assert_eq!(super::parse_answer(&q, qlen, &[0, 0]), None, "answers with another id are ignored");
    }
}
