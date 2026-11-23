//! Ad blocker test: are well-known ad, tracker and analytics domains blocked on this PC/network?
//! Port of Source/toolkit/adblock.py plus adblock_test in Source/swiftlane.py.
//!
//! Read-only check: inspects the Windows hosts file, resolves each test domain via system DNS,
//! and attempts a short TCP/443 connect. Browser-extension blockers only act inside the browser,
//! so this measures system/network-level blocking.

use serde_json::{json, Map, Value};
use std::collections::{HashSet, VecDeque};
use std::net::{IpAddr, TcpStream, ToSocketAddrs};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::AppHandle;

use crate::api::push;

const TESTS: &[(&str, &[&str])] = &[
    (
        "Ads",
        &[
            "ad.doubleclick.net",
            "pagead2.googlesyndication.com",
            "googleadservices.com",
            "adservice.google.com",
            "ib.adnxs.com",
            "match.adsrvr.org",
            "aax.amazon-adsystem.com",
            "ads.pubmatic.com",
            "fastlane.rubiconproject.com",
            "static.media.net",
            "cdn.taboola.com",
            "widgets.outbrain.com",
            "static.criteo.net",
        ],
    ),
    (
        "Analytics",
        &[
            "google-analytics.com",
            "ssl.google-analytics.com",
            "static.hotjar.com",
            "cdn.mouseflow.com",
            "analytics.tiktok.com",
            "bat.bing.com",
            "api.segment.io",
            "stats.wp.com",
        ],
    ),
    (
        "Social trackers",
        &[
            "connect.facebook.net",
            "pixel.facebook.com",
            "static.ads-twitter.com",
            "ads.linkedin.com",
            "tr.snapchat.com",
            "ct.pinterest.com",
        ],
    ),
    (
        "Error trackers",
        &[
            "browser.sentry-cdn.com",
            "sessions.bugsnag.com",
            "notify.bugsnag.com",
        ],
    ),
    (
        "Phone & TV makers",
        &[
            "api.ad.xiaomi.com",
            "samsungads.com",
            "iadsdk.apple.com",
            "adx.ads.oppomobile.com",
            "metrics.data.hicloud.com",
            "ads.yahoo.com",
            "adtago.s3.amazonaws.com",
        ],
    ),
    (
        "Mixed content",
        &[
            "securepubads.g.doubleclick.net",
            "track.adform.net",
            "secure.quantserve.com",
            "sb.scorecardresearch.com",
            "pixel.adsafeprotected.com",
            "ads.yieldmo.com",
            "pixel.quantserve.com",
            "js-sec.indexww.com",
        ],
    ),
];

fn is_sinkhole_str(s: &str) -> bool {
    matches!(s, "0.0.0.0" | "127.0.0.1" | "::" | "::1" | "0:0:0:0:0:0:0:0")
}

fn is_sinkhole_ip(ip: IpAddr) -> bool {
    ip.is_unspecified() || ip.is_loopback()
}

fn blocking_dns_name(ip: &str) -> &'static str {
    match ip {
        "94.140.14.14" | "94.140.15.15" => "AdGuard DNS",
        "94.140.14.15" => "AdGuard DNS (family)",
        "194.242.2.3" | "194.242.2.4" => "Mullvad DNS (ad blocking)",
        "76.76.2.2" => "Control D (ad blocking)",
        "45.90.28.0" | "45.90.30.0" => "NextDNS",
        _ => "",
    }
}

fn parse_hosts(text: &str) -> HashSet<String> {
    let mut blocked = HashSet::new();
    for line in text.lines() {
        let clean = line.split('#').next().unwrap_or("");
        let mut parts = clean.split_whitespace();
        let Some(ip) = parts.next() else { continue };
        if is_sinkhole_str(ip) {
            for host in parts {
                blocked.insert(host.to_ascii_lowercase());
            }
        }
    }
    blocked
}

fn hosts_entries() -> HashSet<String> {
    let path = if cfg!(windows) {
        Path::new(r"C:\Windows\System32\drivers\etc\hosts")
    } else {
        Path::new("/etc/hosts")
    };
    let Ok(bytes) = std::fs::read(path) else {
        return HashSet::new();
    };
    let text = String::from_utf8_lossy(&bytes);
    parse_hosts(&text)
}

fn parse_dns_output(out: &str) -> Vec<String> {
    let mut servers = Vec::new();
    for token in out.split_whitespace() {
        if token.parse::<IpAddr>().is_ok() && !servers.iter().any(|s| s == token) {
            servers.push(token.to_string());
        }
    }
    servers
}

fn dns_servers() -> Vec<String> {
    if !cfg!(windows) {
        return Vec::new();
    }
    let Some(ps) = super::program("powershell") else {
        return Vec::new();
    };
    let out = super::command(&ps)
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "(Get-DnsClientServerAddress -AddressFamily IPv4 | Where-Object ServerAddresses).ServerAddresses",
        ])
        .output();
    match out {
        Ok(o) => parse_dns_output(&String::from_utf8_lossy(&o.stdout)),
        Err(_) => Vec::new(),
    }
}

pub fn check_domain(domain: &str, hosts: &HashSet<String>) -> Value {
    if hosts.contains(&domain.to_ascii_lowercase()) {
        return json!({ "domain": domain, "blocked": true, "how": "hosts file" });
    }
    let Ok(addrs_iter) = (domain, 443u16).to_socket_addrs() else {
        return json!({ "domain": domain, "blocked": true, "how": "DNS (no address)" });
    };
    let addrs: Vec<_> = addrs_iter.collect();
    if addrs.is_empty() {
        return json!({ "domain": domain, "blocked": true, "how": "DNS (no address)" });
    }
    if addrs.iter().any(|a| is_sinkhole_ip(a.ip())) {
        return json!({ "domain": domain, "blocked": true, "how": "DNS (sinkhole)" });
    }
    for addr in addrs.iter().take(2) {
        if TcpStream::connect_timeout(addr, Duration::from_secs(4)).is_ok() {
            return json!({ "domain": domain, "blocked": false, "how": "" });
        }
    }
    json!({ "domain": domain, "blocked": true, "how": "connection blocked" })
}

fn run_test(progress: impl Fn(usize, usize) + Send + Sync) -> Value {
    let hosts = Arc::new(hosts_entries());
    let mut jobs: Vec<(usize, usize, &'static str)> = Vec::new();
    for (ci, (_, domains)) in TESTS.iter().enumerate() {
        for (di, &domain) in domains.iter().enumerate() {
            jobs.push((ci, di, domain));
        }
    }
    let total = jobs.len();
    let queue = Arc::new(Mutex::new(VecDeque::from(jobs)));
    let slots: Arc<Mutex<Vec<Vec<Option<Value>>>>> = Arc::new(Mutex::new(
        TESTS
            .iter()
            .map(|(_, domains)| vec![None; domains.len()])
            .collect(),
    ));
    let done = Arc::new(AtomicUsize::new(0));
    let progress = &progress;

    let workers = total.min(16).max(1);
    std::thread::scope(|scope| {
        for _ in 0..workers {
            let queue = Arc::clone(&queue);
            let slots = Arc::clone(&slots);
            let hosts = Arc::clone(&hosts);
            let done = Arc::clone(&done);
            scope.spawn(move || loop {
                let next = queue.lock().unwrap().pop_front();
                let Some((ci, di, domain)) = next else { break };
                let result = check_domain(domain, &hosts);
                slots.lock().unwrap()[ci][di] = Some(result);
                let finished = done.fetch_add(1, Ordering::Relaxed) + 1;
                progress(finished, total);
            });
        }
    });

    let slots = slots.lock().unwrap();
    let mut categories = Map::new();
    let mut blocked = 0usize;
    for (ci, (cat, _)) in TESTS.iter().enumerate() {
        let rows: Vec<Value> = slots[ci]
            .iter()
            .filter_map(|v| v.clone())
            .inspect(|v| {
                if v["blocked"].as_bool().unwrap_or(false) {
                    blocked += 1;
                }
            })
            .collect();
        categories.insert((*cat).to_string(), Value::Array(rows));
    }

    let score = if total > 0 {
        ((100.0 * blocked as f64) / total as f64).round() as u64
    } else {
        0
    };
    let dns: Vec<Value> = dns_servers()
        .into_iter()
        .map(|ip| {
            let name = blocking_dns_name(&ip);
            json!({ "ip": ip, "name": name })
        })
        .collect();

    json!({
        "categories": categories,
        "blocked": blocked,
        "total": total,
        "score": score,
        "dns": dns,
        "hosts_count": hosts.len(),
    })
}

pub fn dispatch(app: &AppHandle, method: &str, _args: &[Value]) -> Option<Value> {
    if method != "adblock_test" {
        return None;
    }
    let result = run_test(|done, total| {
        push(app, "adblock-progress", json!({ "done": done, "total": total }));
    });
    Some(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_hosts_file_entries() {
        let sample = "\
# comment line
127.0.0.1 localhost
0.0.0.0 Ad.DoubleClick.Net tracker.example.com # inline comment
::1 blocked-v6.example
192.168.1.50 printer.local
";
        let entries = parse_hosts(sample);
        assert!(entries.contains("localhost"));
        assert!(entries.contains("ad.doubleclick.net"));
        assert!(entries.contains("tracker.example.com"));
        assert!(entries.contains("blocked-v6.example"));
        assert!(!entries.contains("printer.local"));
    }

    #[test]
    fn identifies_sinkholes_and_dns() {
        assert!(is_sinkhole_ip("0.0.0.0".parse().unwrap()));
        assert!(is_sinkhole_ip("127.0.0.1".parse().unwrap()));
        assert!(is_sinkhole_ip("::".parse().unwrap()));
        assert!(is_sinkhole_ip("::1".parse().unwrap()));
        assert!(!is_sinkhole_ip("94.140.14.14".parse().unwrap()));

        assert_eq!(blocking_dns_name("94.140.14.14"), "AdGuard DNS");
        assert_eq!(blocking_dns_name("1.1.1.1"), "");

        let parsed = parse_dns_output("94.140.14.14\n1.1.1.1\n94.140.14.14\nnot-an-ip\n");
        assert_eq!(parsed, vec!["94.140.14.14", "1.1.1.1"]);
    }

    #[test]
    fn hosts_file_blocks_domain_immediately() {
        let mut hosts = HashSet::new();
        hosts.insert("ad.doubleclick.net".to_string());
        let res = check_domain("Ad.DoubleClick.Net", &hosts);
        assert_eq!(res["blocked"], true);
        assert_eq!(res["how"], "hosts file");
    }
}
