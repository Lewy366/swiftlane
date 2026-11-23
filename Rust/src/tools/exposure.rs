//! "Is my PC exposed?": a read-only look at the ways in.
//! Port of Source/toolkit/exposure.py plus exposure_* methods in Source/swiftlane.py.
//!
//! Checks:
//! - TCP listeners from Windows' own table (GetExtendedTcpTable + tasklist /svc)
//! - Windows Firewall profiles and Public/Private network category
//! - Remote Desktop and legacy SMB1 file sharing (registry)
//! - Router UPnP port forwards (strictly limited to private HTTP URLs on the default gateway)
//!
//! Nothing is changed during the check. Fixes and UPnP unforwards only act on items
//! actually returned by the latest check, and system fixes ask Windows for admin via UAC.

use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Read;
use std::net::{Ipv4Addr, SocketAddrV4, UdpSocket};
use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;
use tauri::{AppHandle, Manager};

use super::{arg, netscan};
use crate::api::{push, AppState};
use crate::config;

const MAX_HTTP_BYTES: u64 = 256 * 1024;

fn known_port(port: u16) -> Option<(&'static str, &'static str)> {
    Some(match port {
        135 => ("Windows (RPC)", "Lets Windows programs talk to each other. Normal; the firewall keeps it inside your network."),
        139 => ("File and printer sharing", "Older part of Windows file sharing."),
        445 => ("File and printer sharing (SMB)", "Shared folders and printers. Normal at home; never forward it on your router."),
        3389 => ("Remote Desktop", "Someone with your password can control this PC. Turn off if you don't use it."),
        5040 => ("Windows (device connect)", "Used by Windows for nearby sharing and phone link."),
        5357 => ("Network discovery", "Lets other PCs see this one in the Network folder."),
        7680 => ("Windows Update sharing", "Delivery Optimization: shares update pieces with PCs on your network."),
        2869 => ("UPnP / media sharing", "Windows media and device sharing."),
        5985 => ("Remote management (WinRM)", "Lets administrators run commands remotely. Rarely needed at home."),
        22 => ("SSH server", "Remote command line. Only keep if you set it up yourself."),
        21 => ("FTP server", "Unencrypted file transfer server."),
        23 => ("Telnet server", "Unencrypted remote access. Should never be on."),
        5900 => ("VNC remote control", "Screen sharing / remote control."),
        8765 => ("Swiftlane phone transfer", "Your phones send and receive files here (pairing code needed)."),
        8766 => ("Swiftlane share links", "Only runs while a share link is active; reached through Tailscale Funnel."),
        _ => return None,
    })
}

fn level_rank(level: &str) -> u8 {
    match level {
        "ok" => 0,
        "info" => 1,
        "warn" => 2,
        "bad" => 3,
        _ => 0,
    }
}

// ------------------------------------------------------------------ listening ports

fn parse_quoted_csv_line(line: &str) -> Vec<String> {
    let mut cols = Vec::new();
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        if c == '"' {
            let mut val = String::new();
            for ch in chars.by_ref() {
                if ch == '"' {
                    break;
                }
                val.push(ch);
            }
            cols.push(val);
        }
    }
    cols
}

fn parse_tasklist_csv(out: &str) -> HashMap<u32, String> {
    let mut names = HashMap::new();
    names.insert(0, "System Idle".to_string());
    names.insert(4, "Windows (System)".to_string());
    for line in out.lines() {
        let cols = parse_quoted_csv_line(line);
        if cols.len() >= 3 {
            if let Ok(pid) = cols[1].parse::<u32>() {
                let exe = &cols[0];
                let services = &cols[2];
                let label = if exe.eq_ignore_ascii_case("svchost.exe")
                    && !services.is_empty()
                    && services != "N/A"
                {
                    format!("{exe} ({services})")
                } else {
                    exe.clone()
                };
                names.insert(pid, label);
            }
        }
    }
    names
}

fn process_names() -> HashMap<u32, String> {
    if !cfg!(windows) {
        return parse_tasklist_csv("");
    }
    let Some(tl) = super::program("tasklist") else {
        return parse_tasklist_csv("");
    };
    match super::command(&tl).args(["/svc", "/fo", "csv", "/nh"]).output() {
        Ok(o) => parse_tasklist_csv(&String::from_utf8_lossy(&o.stdout)),
        Err(_) => parse_tasklist_csv(""),
    }
}

#[cfg(windows)]
mod win_exp {
    use std::net::{Ipv4Addr, Ipv6Addr};
    use winreg::enums::{HKEY_LOCAL_MACHINE, KEY_READ};
    use winreg::RegKey;

    #[link(name = "iphlpapi")]
    extern "system" {
        fn GetExtendedTcpTable(
            p_tcp_table: *mut u8,
            pdw_size: *mut u32,
            b_order: i32,
            ul_af: u32,
            table_class: u32,
            reserved: u32,
        ) -> u32;
    }

    pub fn tcp_listeners(ipv6: bool) -> Vec<(String, u16, u32)> {
        let af: u32 = if ipv6 { 23 } else { 2 }; // AF_INET6 / AF_INET
        let mut size: u32 = 0;
        unsafe {
            GetExtendedTcpTable(std::ptr::null_mut(), &mut size, 0, af, 3, 0);
        }
        let mut buf = vec![0u8; size as usize + 4096];
        let mut buf_size = buf.len() as u32;
        let rc = unsafe { GetExtendedTcpTable(buf.as_mut_ptr(), &mut buf_size, 0, af, 3, 0) };
        if rc != 0 || buf.len() < 4 {
            return Vec::new();
        }
        let count = u32::from_ne_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
        let mut out = Vec::with_capacity(count);
        if !ipv6 {
            for i in 0..count {
                let off = 4 + i * 24;
                if off + 24 > buf.len() {
                    break;
                }
                let addr = Ipv4Addr::new(buf[off + 4], buf[off + 5], buf[off + 6], buf[off + 7]);
                let port = u16::from_be_bytes([buf[off + 8], buf[off + 9]]);
                let pid = u32::from_ne_bytes([buf[off + 20], buf[off + 21], buf[off + 22], buf[off + 23]]);
                out.push((addr.to_string(), port, pid));
            }
        } else {
            for i in 0..count {
                let off = 4 + i * 56;
                if off + 56 > buf.len() {
                    break;
                }
                let mut octets = [0u8; 16];
                octets.copy_from_slice(&buf[off..off + 16]);
                let addr = Ipv6Addr::from(octets);
                let port = u16::from_be_bytes([buf[off + 20], buf[off + 21]]);
                let pid = u32::from_ne_bytes([buf[off + 52], buf[off + 53], buf[off + 54], buf[off + 55]]);
                out.push((addr.to_string(), port, pid));
            }
        }
        out
    }

    pub fn remote_desktop_on() -> bool {
        RegKey::predef(HKEY_LOCAL_MACHINE)
            .open_subkey_with_flags(r"SYSTEM\CurrentControlSet\Control\Terminal Server", KEY_READ)
            .and_then(|k| k.get_value::<u32, _>("fDenyTSConnections"))
            .is_ok_and(|v| v == 0)
    }

    pub fn smb1_installed() -> bool {
        RegKey::predef(HKEY_LOCAL_MACHINE)
            .open_subkey_with_flags(r"SYSTEM\CurrentControlSet\Services\mrxsmb10", KEY_READ)
            .is_ok()
    }
}

#[cfg(not(windows))]
mod win_exp {
    pub fn tcp_listeners(_ipv6: bool) -> Vec<(String, u16, u32)> {
        Vec::new()
    }
    pub fn remote_desktop_on() -> bool {
        false
    }
    pub fn smb1_installed() -> bool {
        false
    }
}

fn is_windows_dynamic_prog(prog: &str) -> bool {
    let lower = prog.to_ascii_lowercase();
    ["lsass", "wininit", "services", "spoolsv", "svchost"]
        .iter()
        .any(|p| lower.starts_with(p))
}

fn listeners() -> Vec<Value> {
    let names = process_names();
    struct Entry {
        port: u16,
        pid: u32,
        program: String,
        addresses: Vec<String>,
        network: bool,
    }
    let mut seen: BTreeMap<(u16, u32), Entry> = BTreeMap::new();
    for ipv6 in [false, true] {
        for (addr, port, pid) in win_exp::tcp_listeners(ipv6) {
            let local_only = addr == "127.0.0.1" || addr == "::1" || addr.starts_with("127.");
            let entry = seen.entry((port, pid)).or_insert_with(|| Entry {
                port,
                pid,
                program: names
                    .get(&pid)
                    .cloned()
                    .unwrap_or_else(|| format!("PID {pid}")),
                addresses: Vec::new(),
                network: false,
            });
            entry.addresses.push(addr);
            entry.network = entry.network || !local_only;
        }
    }

    let mut items: Vec<Entry> = seen.into_values().collect();
    items.sort_by_key(|e| (!e.network, e.port));
    items
        .into_iter()
        .map(|e| {
            let known = known_port(e.port);
            let dynamic = e.port >= 49152 && is_windows_dynamic_prog(&e.program);
            let (what, why) = match known {
                Some((w, y)) => (Some(w), Some(y)),
                None if dynamic => (
                    Some("Windows (internal)"),
                    Some("Windows services that only answer other Windows parts."),
                ),
                None => (None, None),
            };
            json!({
                "port": e.port,
                "pid": e.pid,
                "program": e.program,
                "addresses": e.addresses,
                "network": e.network,
                "what": what,
                "why": why,
            })
        })
        .collect()
}

// ------------------------------------------------------------------ firewall & network type

struct FwProfile {
    name: String,
    enabled: bool,
}

struct NetProfile {
    name: String,
    adapter: String,
    category: String,
}

fn is_virtual_adapter(adapter: &str) -> bool {
    let lower = adapter.to_ascii_lowercase();
    ["tailscale", "vethernet", "vmware", "virtualbox", "wsl"]
        .iter()
        .any(|p| lower.starts_with(p))
}

fn parse_firewall_json(raw: &str) -> Option<(Vec<FwProfile>, Vec<NetProfile>)> {
    let data: Value = serde_json::from_str(raw).ok()?;
    let to_arr = |v: Option<&Value>| -> Vec<Value> {
        match v {
            Some(Value::Array(a)) => a.clone(),
            Some(o @ Value::Object(_)) => vec![o.clone()],
            _ => Vec::new(),
        }
    };
    let profiles = to_arr(data.get("profiles"))
        .into_iter()
        .filter_map(|p| {
            let name = p.get("Name")?.as_str()?.to_string();
            let enabled_str = match p.get("Enabled") {
                Some(Value::Bool(b)) => b.to_string(),
                Some(Value::String(s)) => s.clone(),
                Some(Value::Number(n)) => n.to_string(),
                _ => String::new(),
            };
            let enabled = matches!(enabled_str.to_ascii_lowercase().as_str(), "true" | "1");
            Some(FwProfile { name, enabled })
        })
        .collect();

    let networks = to_arr(data.get("networks"))
        .into_iter()
        .map(|n| NetProfile {
            name: n.get("Name").and_then(Value::as_str).unwrap_or("").to_string(),
            adapter: n
                .get("InterfaceAlias")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            category: n
                .get("Category")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
        })
        .collect();

    Some((profiles, networks))
}

fn firewall() -> Option<(Vec<FwProfile>, Vec<NetProfile>)> {
    if !cfg!(windows) {
        return None;
    }
    let ps = super::program("powershell")?;
    let script = concat!(
        "$p = Get-NetFirewallProfile | Select-Object Name,@{n='Enabled';e={[string]$_.Enabled}},@{n='Inbound';e={[string]$_.DefaultInboundAction}};",
        "$c = Get-NetConnectionProfile | Select-Object Name,InterfaceAlias,@{n='Category';e={[string]$_.NetworkCategory}};",
        "@{profiles=@($p); networks=@($c)} | ConvertTo-Json -Depth 4 -Compress"
    );
    let out = super::command(&ps)
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .output()
        .ok()?;
    parse_firewall_json(&String::from_utf8_lossy(&out.stdout))
}

// ------------------------------------------------------------------ UPnP on the router

fn split_http_url(url: &str) -> Option<(String, Option<u16>, String)> {
    let rest = url.strip_prefix("http://")?;
    let (authority, path) = match rest.find('/') {
        Some(idx) => (&rest[..idx], &rest[idx..]),
        None => (rest, "/"),
    };
    if authority.contains('@') || authority.is_empty() {
        return None;
    }
    let (host, port) = if let Some(colon) = authority.rfind(':') {
        let p = authority[colon + 1..].parse::<u16>().ok()?;
        (&authority[..colon], Some(p))
    } else {
        (authority, None)
    };
    Some((host.to_string(), port, path.to_string()))
}

/// Only talk to the router itself: an http URL whose host is a private IPv4 address matching gateway.
pub fn is_lan_url(url: &str, gateway: Option<&str>) -> bool {
    let Some((host, _, _)) = split_http_url(url) else {
        return false;
    };
    let Ok(ip) = host.parse::<Ipv4Addr>() else {
        return false;
    };
    if !ip.is_private() {
        return false;
    }
    match gateway {
        Some(gw) => host == gw,
        None => true,
    }
}

fn resolve_url(base: &str, rel: &str) -> Option<String> {
    let rel = rel.trim();
    if rel.starts_with("http://") || rel.starts_with("https://") {
        return Some(rel.to_string());
    }
    let (host, port, base_path) = split_http_url(base)?;
    let authority = match port {
        Some(p) => format!("{host}:{p}"),
        None => host,
    };
    if rel.starts_with('/') {
        Some(format!("http://{authority}{rel}"))
    } else {
        let dir = match base_path.rfind('/') {
            Some(i) => &base_path[..=i],
            None => "/",
        };
        Some(format!("http://{authority}{dir}{rel}"))
    }
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn xml_unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// Extracts inner text of the first tag whose local name (ignoring any namespace prefix) matches `tag`.
fn xml_tag_text(xml: &str, tag: &str) -> Option<String> {
    let mut pos = 0;
    while let Some(lt) = xml[pos..].find('<') {
        let start = pos + lt;
        let Some(gt_rel) = xml[start..].find('>') else { break };
        let open_end = start + gt_rel;
        let header = xml[start + 1..open_end].trim();
        if !header.starts_with('/') && !header.starts_with('?') && !header.starts_with('!') {
            let self_closing = header.ends_with('/');
            let full_name = header.trim_end_matches('/').split_whitespace().next().unwrap_or("");
            let local = full_name.rsplit(':').next().unwrap_or(full_name);
            if local == tag {
                if self_closing {
                    return Some(String::new());
                }
                let close_tag = format!("</{full_name}>");
                if let Some(end_rel) = xml[open_end + 1..].find(&close_tag) {
                    return Some(xml_unescape(&xml[open_end + 1..open_end + 1 + end_rel]));
                }
            }
        }
        pos = open_end + 1;
    }
    None
}

/// Extracts all `<service>...</service>` (ignoring namespace prefix) blocks from UPnP XML.
fn xml_blocks(xml: &str, tag: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut pos = 0;
    while let Some(lt) = xml[pos..].find('<') {
        let start = pos + lt;
        let Some(gt_rel) = xml[start..].find('>') else { break };
        let open_end = start + gt_rel;
        let header = xml[start + 1..open_end].trim();
        if !header.starts_with('/') && !header.starts_with('?') && !header.starts_with('!') && !header.ends_with('/') {
            let full_name = header.split_whitespace().next().unwrap_or("");
            let local = full_name.rsplit(':').next().unwrap_or(full_name);
            if local == tag {
                let close_tag = format!("</{full_name}>");
                if let Some(end_rel) = xml[open_end + 1..].find(&close_tag) {
                    let body_start = open_end + 1;
                    let body_end = body_start + end_rel;
                    out.push(xml[body_start..body_end].to_string());
                    pos = body_end + close_tag.len();
                    continue;
                }
            }
        }
        pos = open_end + 1;
    }
    out
}

fn upnp_client() -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(4))
        .build()
        .map_err(|e| e.to_string())
}

fn upnp_get(url: &str) -> Result<String, String> {
    let c = upnp_client()?;
    let resp = c.get(url).send().map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status().as_u16()));
    }
    let mut buf = Vec::new();
    resp.take(MAX_HTTP_BYTES + 1)
        .read_to_end(&mut buf)
        .map_err(|e| e.to_string())?;
    if buf.len() as u64 > MAX_HTTP_BYTES {
        return Err("answer too big".into());
    }
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

fn soap_call(control: &str, service: &str, action: &str, args: &[(&str, String)]) -> Result<String, String> {
    let mut body = String::new();
    for (k, v) in args {
        body.push('<');
        body.push_str(k);
        body.push('>');
        body.push_str(&xml_escape(v));
        body.push_str("</");
        body.push_str(k);
        body.push('>');
    }
    let envelope = format!(
        "<?xml version=\"1.0\"?><s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\" \
         s:encodingStyle=\"http://schemas.xmlsoap.org/soap/encoding/\"><s:Body>\
         <u:{action} xmlns:u=\"{service}\">{body}</u:{action}></s:Body></s:Envelope>"
    );
    let c = upnp_client()?;
    let resp = c
        .post(control)
        .header("Content-Type", "text/xml; charset=\"utf-8\"")
        .header("SOAPAction", format!("\"{service}#{action}\""))
        .body(envelope)
        .send()
        .map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status().as_u16()));
    }
    let mut buf = Vec::new();
    resp.take(MAX_HTTP_BYTES + 1)
        .read_to_end(&mut buf)
        .map_err(|e| e.to_string())?;
    if buf.len() as u64 > MAX_HTTP_BYTES {
        return Err("answer too big".into());
    }
    let text = String::from_utf8_lossy(&buf).into_owned();
    let expected = format!("{action}Response");
    let block = xml_blocks(&text, &expected)
        .into_iter()
        .next()
        .ok_or_else(|| format!("Missing {expected}"))?;
    Ok(block)
}

fn parse_ssdp_location(packet: &str) -> Option<String> {
    for line in packet.lines() {
        if let Some((k, v)) = line.split_once(':') {
            if k.trim().eq_ignore_ascii_case("location") {
                let loc = v.trim();
                if !loc.is_empty() {
                    return Some(loc.to_string());
                }
            }
        }
    }
    None
}

fn find_router_upnp(gateway: Option<&str>) -> Option<(String, String)> {
    let msg = concat!(
        "M-SEARCH * HTTP/1.1\r\n",
        "HOST: 239.255.255.250:1900\r\n",
        "MAN: \"ssdp:discover\"\r\n",
        "MX: 2\r\n",
        "ST: urn:schemas-upnp-org:device:InternetGatewayDevice:1\r\n\r\n"
    );
    let sock = UdpSocket::bind("0.0.0.0:0").ok()?;
    let _ = sock.set_read_timeout(Some(Duration::from_millis(2500)));
    let _ = sock.send_to(msg.as_bytes(), SocketAddrV4::new(Ipv4Addr::new(239, 255, 255, 250), 1900));

    let mut locations = Vec::new();
    let mut seen = HashSet::new();
    let mut buf = [0u8; 4096];
    let deadline = std::time::Instant::now() + Duration::from_millis(2500);
    while std::time::Instant::now() < deadline {
        match sock.recv_from(&mut buf) {
            Ok((n, src)) => {
                let src_ip = src.ip().to_string();
                if gateway.is_some_and(|gw| gw != src_ip) {
                    continue;
                }
                let text = String::from_utf8_lossy(&buf[..n]);
                if let Some(loc) = parse_ssdp_location(&text) {
                    if seen.insert(loc.clone()) {
                        locations.push(loc);
                    }
                }
            }
            Err(_) => break,
        }
    }

    for loc in locations {
        if !is_lan_url(&loc, gateway) {
            continue;
        }
        let Ok(xml) = upnp_get(&loc) else { continue };
        for svc in xml_blocks(&xml, "service") {
            let stype = xml_tag_text(&svc, "serviceType").unwrap_or_default();
            if stype.contains("WANIPConnection") || stype.contains("WANPPPConnection") {
                let ctrl_rel = xml_tag_text(&svc, "controlURL").unwrap_or_default();
                if let Some(control) = resolve_url(&loc, &ctrl_rel) {
                    if is_lan_url(&control, gateway) {
                        return Some((control, stype.trim().to_string()));
                    }
                }
            }
        }
    }
    None
}

fn clean_desc(s: &str) -> String {
    s.chars().filter(|c| (*c as u32) >= 0x20).take(80).collect()
}

fn port_forwards(gateway: Option<&str>) -> Option<Value> {
    let (control, stype) = find_router_upnp(gateway)?;
    let mut out = Vec::new();
    for i in 0..128usize {
        let Ok(resp) = soap_call(
            &control,
            &stype,
            "GetGenericPortMappingEntry",
            &[("NewPortMappingIndex", i.to_string())],
        ) else {
            break;
        };
        let external: u16 = xml_tag_text(&resp, "NewExternalPort")
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(0);
        let protocol = xml_tag_text(&resp, "NewProtocol").unwrap_or_default().trim().to_string();
        let internal: u16 = xml_tag_text(&resp, "NewInternalPort")
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(0);
        let client = xml_tag_text(&resp, "NewInternalClient").unwrap_or_default().trim().to_string();
        let enabled_raw = xml_tag_text(&resp, "NewEnabled").unwrap_or_else(|| "1".into());
        let enabled = matches!(enabled_raw.trim().to_ascii_lowercase().as_str(), "1" | "true");
        let name = clean_desc(&xml_tag_text(&resp, "NewPortMappingDescription").unwrap_or_default());
        let remote = xml_tag_text(&resp, "NewRemoteHost").unwrap_or_default().trim().to_string();
        let lease: u64 = xml_tag_text(&resp, "NewLeaseDuration")
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(0);
        out.push(json!({
            "external": external,
            "protocol": protocol,
            "internal": internal,
            "client": client,
            "enabled": enabled,
            "name": name,
            "remote": remote,
            "lease": lease,
        }));
    }
    Some(json!({
        "control": control,
        "service": stype,
        "forwards": out,
    }))
}

fn remove_forward(
    control: &str,
    service: &str,
    gateway: Option<&str>,
    external: u16,
    protocol: &str,
    remote: &str,
) -> Result<(), String> {
    if !is_lan_url(control, gateway) || !matches!(protocol, "TCP" | "UDP") {
        return Err("Not the router.".into());
    }
    soap_call(
        control,
        service,
        "DeletePortMapping",
        &[
            ("NewRemoteHost", remote.to_string()),
            ("NewExternalPort", external.to_string()),
            ("NewProtocol", protocol.to_string()),
        ],
    )?;
    Ok(())
}

// ------------------------------------------------------------------ full check & fixes

struct AppInfo {
    server_port: Option<u16>,
    https_only: bool,
    remote: bool,
    remote_power: bool,
    share_links: usize,
}

fn run_check(app_info: &AppInfo) -> Value {
    let ip = netscan::primary_ip();
    let gw = ip.as_deref().and_then(netscan::gateway);
    let mut found: Vec<Value> = Vec::new();

    let mut add = |id: &str, level: &str, title: String, detail: String, fix: Option<Value>| {
        found.push(json!({
            "id": id,
            "level": level,
            "title": title,
            "detail": detail,
            "fix": fix,
        }));
    };

    match firewall() {
        None => add(
            "firewall",
            "warn",
            "Couldn't read the firewall settings".into(),
            "Check Windows Security > Firewall & network protection.".into(),
            None,
        ),
        Some((profiles, networks)) => {
            let off: Vec<String> = profiles
                .iter()
                .filter(|p| !p.enabled)
                .map(|p| p.name.clone())
                .collect();
            if !off.is_empty() {
                let args: Vec<String> = off.iter().map(|p| p.to_ascii_lowercase()).collect();
                add(
                    "firewall",
                    "bad",
                    format!("Firewall is off for {} networks", off.join(", ")),
                    "The firewall is what keeps others on the network out of the programs below.".into(),
                    Some(json!({
                        "action": "firewall_on",
                        "args": args,
                        "label": "Turn the firewall on",
                        "admin": true,
                    })),
                );
            } else {
                add(
                    "firewall",
                    "ok",
                    "Firewall is on".into(),
                    "On for Domain, Private and Public networks.".into(),
                    None,
                );
            }

            for n in networks {
                if is_virtual_adapter(&n.adapter) {
                    continue;
                }
                if n.category == "Private" {
                    add(
                        "profile",
                        "info",
                        format!("\"{}\" is set as a Private network", n.name),
                        "Good at home: your PC can be seen by your own devices. On public Wi-Fi (café, hotel) choose Public instead in Windows settings.".into(),
                        None,
                    );
                } else if n.category == "Public" {
                    add(
                        "profile",
                        "ok",
                        format!("\"{}\" is set as a Public network", n.name),
                        "Windows keeps this PC hidden from others on the network.".into(),
                        None,
                    );
                }
            }
        }
    }

    if win_exp::remote_desktop_on() {
        add(
            "rdp",
            "warn",
            "Remote Desktop is on".into(),
            "Anyone who reaches this PC and knows your password can take control of it.".into(),
            Some(json!({
                "action": "rdp_off",
                "args": [],
                "label": "Turn Remote Desktop off",
                "admin": true,
            })),
        );
    } else {
        add(
            "rdp",
            "ok",
            "Remote Desktop is off".into(),
            "Nobody can take over this PC's screen remotely.".into(),
            None,
        );
    }

    if win_exp::smb1_installed() {
        add(
            "smb1",
            "warn",
            "Old SMB1 file sharing is installed".into(),
            "SMB1 was used by the WannaCry attack. Windows 11 doesn't need it.".into(),
            Some(json!({
                "action": "smb1_off",
                "args": [],
                "label": "Remove SMB1",
                "admin": true,
            })),
        );
    }

    let ports = listeners();
    let risky = ports.iter().any(|p| {
        p["network"].as_bool().unwrap_or(false)
            && matches!(p["port"].as_u64(), Some(21 | 23 | 3389 | 5900 | 5985))
    });
    let open_net = ports
        .iter()
        .filter(|p| p["network"].as_bool().unwrap_or(false))
        .count();
    add(
        "ports",
        if risky { "warn" } else { "info" },
        format!("{open_net} programs accept connections from the network"),
        "Listed below. Most are part of Windows. Whether others actually get through is decided by the firewall.".into(),
        None,
    );

    let upnp = port_forwards(gw.as_deref());
    match &upnp {
        None => add(
            "upnp",
            "ok",
            "Your router doesn't open doors by itself".into(),
            "UPnP is off on the router (or it doesn't answer), so programs can't open ports to the internet on their own.".into(),
            None,
        ),
        Some(u) => {
            let forwards = u["forwards"].as_array().map(Vec::as_slice).unwrap_or(&[]);
            if !forwards.is_empty() {
                let mine = ip
                    .as_deref()
                    .map(|my| forwards.iter().filter(|f| f["client"].as_str() == Some(my)).count())
                    .unwrap_or(0);
                let plural = if forwards.len() == 1 { "" } else { "s" };
                let mine_note = if mine > 0 {
                    format!(" {mine} go to this PC.")
                } else {
                    String::new()
                };
                add(
                    "upnp",
                    "warn",
                    format!("Your router forwards {} port{plural} from the internet", forwards.len()),
                    format!(
                        "These reach devices at home directly from the internet.{mine_note} Remove the ones you don't recognise. To stop programs doing this, turn UPnP off in your router's settings."
                    ),
                    None,
                );
            } else {
                add(
                    "upnp",
                    "info",
                    "UPnP is on, but no doors are open".into(),
                    "Programs could open ports on your router by themselves. Turning UPnP off in the router's settings is safer.".into(),
                    None,
                );
            }
        }
    }

    if app_info.share_links > 0 {
        let s = if app_info.share_links == 1 { " is" } else { "s are" };
        add(
            "swiftlane-share",
            "info",
            format!("{} Swiftlane share link{s} reachable from the internet", app_info.share_links),
            "Only through Tailscale Funnel, only the files you shared, until the links expire.".into(),
            Some(json!({ "action": "page", "args": ["share"], "label": "Open Share links" })),
        );
    }
    if app_info.server_port.is_some() {
        let level = if app_info.https_only { "ok" } else { "info" };
        let detail = if app_info.https_only {
            "Phones need the pairing code. Only encrypted connections are allowed."
        } else {
            "Phones need the pairing code. Unencrypted connections are still allowed; you can turn them off in Settings once phones are set up."
        };
        let fix = if app_info.https_only {
            None
        } else {
            Some(json!({ "action": "page", "args": ["settings"], "label": "Open Settings" }))
        };
        add(
            "swiftlane-transfer",
            level,
            "Swiftlane phone transfer is listening on your network".into(),
            detail.into(),
            fix,
        );
    }
    if app_info.remote && app_info.remote_power {
        add(
            "swiftlane-remote",
            "info",
            "Paired phones can sleep or shut down this PC".into(),
            "From the PC remote. You can turn that off in Settings.".into(),
            Some(json!({ "action": "page", "args": ["settings"], "label": "Open Settings" })),
        );
    }

    let worst = found
        .iter()
        .map(|f| level_rank(f["level"].as_str().unwrap_or("ok")))
        .max()
        .unwrap_or(0);
    found.sort_by_key(|f| std::cmp::Reverse(level_rank(f["level"].as_str().unwrap_or("ok"))));
    let verdict = match worst {
        0 | 1 => "ok",
        2 => "warn",
        _ => "bad",
    };

    json!({
        "findings": found,
        "ports": ports,
        "upnp": upnp,
        "ip": ip,
        "gateway": gw,
        "verdict": verdict,
    })
}

pub fn fix_command(action: &str, args: &[String]) -> Result<(&'static str, String), String> {
    match action {
        "firewall_on" => {
            let valid: Vec<&str> = args
                .iter()
                .map(String::as_str)
                .filter(|a| matches!(*a, "domain" | "private" | "public"))
                .collect();
            if valid.is_empty() {
                return Err("Unknown firewall profile.".into());
            }
            let target = if valid.len() > 1 {
                "allprofiles".to_string()
            } else {
                format!("{}profile", valid[0])
            };
            Ok(("netsh", format!("advfirewall set {target} state on")))
        }
        "rdp_off" => Ok((
            "reg",
            r#"add "HKLM\SYSTEM\CurrentControlSet\Control\Terminal Server" /v fDenyTSConnections /t REG_DWORD /d 1 /f"#.into(),
        )),
        "smb1_off" => Ok((
            "dism",
            "/online /disable-feature /featurename:SMB1Protocol /norestart".into(),
        )),
        _ => Err("Unknown fix.".into()),
    }
}

/// Strips the router's internal control URL and service type before sending data to the page.
fn sanitize_view(exp: &Value) -> Value {
    let mut view = exp.clone();
    if let Some(upnp) = view.get("upnp").and_then(Value::as_object) {
        let forwards = upnp.get("forwards").cloned().unwrap_or_else(|| json!([]));
        view["upnp"] = json!({ "forwards": forwards });
    }
    view
}

static LAST_EXPOSURE: Mutex<Option<Value>> = Mutex::new(None);

fn current_view() -> Value {
    LAST_EXPOSURE
        .lock()
        .unwrap()
        .as_ref()
        .map(sanitize_view)
        .unwrap_or(Value::Null)
}

pub fn dispatch(app: &AppHandle, method: &str, args: &[Value]) -> Option<Value> {
    Some(match method {
        "exposure_state" => current_view(),
        "exposure_check" => {
            let st = app.state::<AppState>();
            let app_info = {
                let cfg = st.cfg.lock().unwrap();
                let transfer_on = cfg["features"]["transfer"].as_bool().unwrap_or(true);
                let port = cfg["port"].as_u64().map(|p| p as u16);
                let share_links = st
                    .shares
                    .summary(None)
                    .as_array()
                    .map(|arr| {
                        arr.iter()
                            .filter(|s| s["active"].as_bool().unwrap_or(false))
                            .count()
                    })
                    .unwrap_or(0);
                AppInfo {
                    server_port: if transfer_on { port } else { None },
                    https_only: cfg["https_only"].as_bool().unwrap_or(false),
                    remote: cfg["features"]["remote"].as_bool().unwrap_or(false),
                    remote_power: cfg["remote_power"].as_bool().unwrap_or(false),
                    share_links,
                }
            };

            let id = st.jobs.start(
                app,
                "exposure",
                "Check how exposed this PC is",
                false,
                move |ctx| {
                    let result = run_check(&app_info);
                    ctx.check()?;
                    let view = sanitize_view(&result);
                    *LAST_EXPOSURE.lock().unwrap() = Some(result);
                    push(ctx.app(), "exposure", view);
                    Ok(Vec::new())
                },
            );
            json!(id)
        }
        "exposure_fix" => {
            let action = arg(args, 0).as_str().unwrap_or("");
            if action.is_empty() || action == "page" {
                return Some(json!({ "error": "Run the check again first." }));
            }
            let offered_args: Option<Vec<String>> = LAST_EXPOSURE
                .lock()
                .unwrap()
                .as_ref()
                .and_then(|e| e["findings"].as_array())
                .and_then(|findings| {
                    findings.iter().find_map(|f| {
                        let fix = f.get("fix")?.as_object()?;
                        if fix.get("action")?.as_str()? == action {
                            let arr = fix
                                .get("args")
                                .and_then(Value::as_array)
                                .map(|a| {
                                    a.iter()
                                        .filter_map(|v| v.as_str().map(String::from))
                                        .collect()
                                })
                                .unwrap_or_default();
                            Some(arr)
                        } else {
                            None
                        }
                    })
                });

            let Some(fix_args) = offered_args else {
                return Some(json!({ "error": "Run the check again first." }));
            };
            let (exe_name, params) = match fix_command(action, &fix_args) {
                Ok(cmd) => cmd,
                Err(e) => return Some(json!({ "error": e })),
            };
            config::log(&format!("Exposure fix requested: {action}"));
            let exe_path = super::program(exe_name).unwrap_or_else(|| Path::new(exe_name).to_path_buf());
            let cwd = std::env::current_dir().unwrap_or_else(|_| Path::new(".").to_path_buf());
            if !crate::winapi::run_elevated(&exe_path, &params, &cwd) {
                return Some(json!({ "error": "Windows didn't get permission, so nothing was changed." }));
            }
            json!(true)
        }
        "exposure_unforward" => {
            let external = arg(args, 0)
                .as_u64()
                .or_else(|| arg(args, 0).as_str().and_then(|s| s.parse::<u64>().ok()))
                .and_then(|n| u16::try_from(n).ok());
            let protocol = arg(args, 1).as_str().unwrap_or("");
            let Some(external) = external else {
                return Some(json!({ "error": "Run the check again first." }));
            };

            let target = {
                let guard = LAST_EXPOSURE.lock().unwrap();
                let Some(exp) = guard.as_ref() else {
                    return Some(json!({ "error": "Run the check again first." }));
                };
                let upnp = exp.get("upnp").and_then(Value::as_object);
                let control = upnp
                    .and_then(|u| u.get("control"))
                    .and_then(Value::as_str)
                    .map(String::from);
                let service = upnp
                    .and_then(|u| u.get("service"))
                    .and_then(Value::as_str)
                    .map(String::from);
                let gateway = exp.get("gateway").and_then(Value::as_str).map(String::from);
                let matched = upnp
                    .and_then(|u| u.get("forwards"))
                    .and_then(Value::as_array)
                    .and_then(|arr| {
                        arr.iter().find(|f| {
                            f["external"].as_u64() == Some(external as u64)
                                && f["protocol"].as_str() == Some(protocol)
                        })
                    })
                    .cloned();
                match (control, service, matched) {
                    (Some(c), Some(s), Some(m)) => Some((c, s, gateway, m)),
                    _ => None,
                }
            };

            let Some((control, service, gateway, matched)) = target else {
                return Some(json!({ "error": "Run the check again first." }));
            };
            let remote = matched["remote"].as_str().unwrap_or("");
            if let Err(e) = remove_forward(&control, &service, gateway.as_deref(), external, protocol, remote) {
                return Some(json!({
                    "error": format!("The router refused ({e}). Remove it in the router's settings instead.")
                }));
            }
            config::log(&format!("Asked the router to close port {external}/{protocol}"));

            let mut guard = LAST_EXPOSURE.lock().unwrap();
            if let Some(exp) = guard.as_mut() {
                if let Some(forwards) = exp["upnp"]["forwards"].as_array_mut() {
                    if let Some(idx) = forwards.iter().position(|f| {
                        f["external"].as_u64() == Some(external as u64)
                            && f["protocol"].as_str() == Some(protocol)
                    }) {
                        forwards.remove(idx);
                    }
                }
            }
            drop(guard);
            current_view()
        }
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_lan_urls_and_resolves_relative_paths() {
        assert!(is_lan_url("http://192.168.0.1:49000/igddesc.xml", Some("192.168.0.1")));
        assert!(!is_lan_url("http://192.168.0.2:49000/igddesc.xml", Some("192.168.0.1")));
        assert!(!is_lan_url("https://192.168.0.1/igddesc.xml", Some("192.168.0.1")));
        assert!(!is_lan_url("http://8.8.8.8/igddesc.xml", None));
        assert!(!is_lan_url("http://user@192.168.0.1/x", Some("192.168.0.1")));

        assert_eq!(
            resolve_url("http://192.168.0.1:49000/desc/igd.xml", "/upnp/control/wanip").as_deref(),
            Some("http://192.168.0.1:49000/upnp/control/wanip")
        );
        assert_eq!(
            resolve_url("http://192.168.0.1:49000/desc/igd.xml", "ctl/wanip").as_deref(),
            Some("http://192.168.0.1:49000/desc/ctl/wanip")
        );
    }

    #[test]
    fn parses_tasklist_csv_and_xml_soap() {
        let csv = "\"svchost.exe\",\"1204\",\"Dnscache, LanmanWorkstation\"\n\"code.exe\",\"8888\",\"N/A\"\n";
        let map = parse_tasklist_csv(csv);
        assert_eq!(map.get(&1204).map(String::as_str), Some("svchost.exe (Dnscache, LanmanWorkstation)"));
        assert_eq!(map.get(&8888).map(String::as_str), Some("code.exe"));

        let xml = "<s:Envelope><s:Body><u:GetGenericPortMappingEntryResponse xmlns:u=\"urn:test\"><NewExternalPort>443</NewExternalPort><NewProtocol>TCP</NewProtocol><NewPortMappingDescription>Web &amp; App</NewPortMappingDescription></u:GetGenericPortMappingEntryResponse></s:Body></s:Envelope>";
        let resp = xml_blocks(xml, "GetGenericPortMappingEntryResponse").pop().unwrap();
        assert_eq!(xml_tag_text(&resp, "NewExternalPort").as_deref(), Some("443"));
        assert_eq!(xml_tag_text(&resp, "NewProtocol").as_deref(), Some("TCP"));
        assert_eq!(xml_tag_text(&resp, "NewPortMappingDescription").as_deref(), Some("Web & App"));
    }

    #[test]
    fn validates_fix_commands_and_sanitizes_view() {
        assert_eq!(
            fix_command("firewall_on", &["public".into()]).unwrap(),
            ("netsh", "advfirewall set publicprofile state on".into())
        );
        assert_eq!(
            fix_command("firewall_on", &["private".into(), "public".into()]).unwrap(),
            ("netsh", "advfirewall set allprofiles state on".into())
        );
        assert!(fix_command("firewall_on", &["evil; calc.exe".into()]).is_err());
        assert!(fix_command("unknown", &[]).is_err());

        let raw = json!({
            "verdict": "warn",
            "upnp": {
                "control": "http://192.168.0.1:49000/ctl",
                "service": "urn:schemas-upnp-org:service:WANIPConnection:1",
                "forwards": [{ "external": 8080, "protocol": "TCP" }]
            }
        });
        let view = sanitize_view(&raw);
        assert!(view["upnp"].get("control").is_none());
        assert!(view["upnp"].get("service").is_none());
        assert_eq!(view["upnp"]["forwards"][0]["external"], 8080);
    }
}
