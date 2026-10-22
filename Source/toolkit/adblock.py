"""Ad blocker test: are well-known ad, tracker and analytics domains blocked on this PC/network?

Checks each domain the way any program on this PC would reach it: DNS lookup, then a
connection. Blocked = the name doesn't resolve, resolves to 0.0.0.0/127.0.0.1, or the
connection is refused. Browser-extension blockers (uBlock Origin, AdBlock...) only work
inside the browser, so no outside program can see them.
"""
import ipaddress
import socket
import subprocess
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

from .common import no_window

TESTS = {
    "Ads": ["ad.doubleclick.net", "pagead2.googlesyndication.com", "googleadservices.com", "adservice.google.com",
            "ib.adnxs.com", "match.adsrvr.org", "aax.amazon-adsystem.com", "ads.pubmatic.com",
            "fastlane.rubiconproject.com", "static.media.net", "cdn.taboola.com", "widgets.outbrain.com", "static.criteo.net"],
    "Analytics": ["google-analytics.com", "ssl.google-analytics.com", "static.hotjar.com", "cdn.mouseflow.com",
                  "analytics.tiktok.com", "bat.bing.com", "api.segment.io", "stats.wp.com"],
    "Social trackers": ["connect.facebook.net", "pixel.facebook.com", "static.ads-twitter.com", "ads.linkedin.com",
                        "tr.snapchat.com", "ct.pinterest.com"],
    "Error trackers": ["browser.sentry-cdn.com", "sessions.bugsnag.com", "notify.bugsnag.com"],
    "Phone & TV makers": ["api.ad.xiaomi.com", "samsungads.com", "iadsdk.apple.com", "adx.ads.oppomobile.com",
                          "metrics.data.hicloud.com", "ads.yahoo.com", "adtago.s3.amazonaws.com"],
    "Mixed content": ["securepubads.g.doubleclick.net", "track.adform.net", "secure.quantserve.com", "sb.scorecardresearch.com",
                      "pixel.adsafeprotected.com", "ads.yieldmo.com", "pixel.quantserve.com", "js-sec.indexww.com"],
}
SINKHOLES = {"0.0.0.0", "127.0.0.1", "::", "::1", "0:0:0:0:0:0:0:0"}
BLOCKING_DNS = {  # well-known public DNS services that block ads
    "94.140.14.14": "AdGuard DNS", "94.140.15.15": "AdGuard DNS", "94.140.14.15": "AdGuard DNS (family)",
    "194.242.2.3": "Mullvad DNS (ad blocking)", "194.242.2.4": "Mullvad DNS (ad blocking)",
    "76.76.2.2": "Control D (ad blocking)", "45.90.28.0": "NextDNS", "45.90.30.0": "NextDNS",
}


def hosts_entries():
    """Domains blocked in the Windows hosts file."""
    path = Path(r"C:\Windows\System32\drivers\etc\hosts")
    blocked = set()
    try:
        for line in path.read_text("utf-8", "ignore").splitlines():
            parts = line.split("#")[0].split()
            if len(parts) >= 2 and parts[0] in SINKHOLES:
                blocked.update(p.lower() for p in parts[1:])
    except OSError:
        pass
    return blocked


def dns_servers():
    """The DNS servers Windows is using right now."""
    try:
        out = subprocess.run(["powershell.exe", "-NoProfile", "-NonInteractive", "-Command",
                              "(Get-DnsClientServerAddress -AddressFamily IPv4 | Where-Object ServerAddresses).ServerAddresses"],
                             capture_output=True, text=True, timeout=15, **no_window()).stdout
    except (OSError, subprocess.SubprocessError):
        return []
    servers = []
    for line in out.split():
        try:
            ipaddress.ip_address(line)
            if line not in servers:
                servers.append(line)
        except ValueError:
            pass
    return servers


def check_domain(domain, hosts):
    """One domain: {"domain", "blocked", "how"}."""
    if domain.lower() in hosts:
        return {"domain": domain, "blocked": True, "how": "hosts file"}
    try:
        infos = socket.getaddrinfo(domain, 443, type=socket.SOCK_STREAM)
    except socket.gaierror:
        return {"domain": domain, "blocked": True, "how": "DNS (no address)"}
    addresses = {i[4][0] for i in infos}
    if addresses & SINKHOLES or all(a in SINKHOLES for a in addresses):
        return {"domain": domain, "blocked": True, "how": "DNS (sinkhole)"}
    for family, kind, proto, _, addr in infos[:2]:
        try:
            with socket.socket(family, kind, proto) as s:
                s.settimeout(4)
                s.connect(addr)
                return {"domain": domain, "blocked": False, "how": ""}
        except OSError:
            continue
    return {"domain": domain, "blocked": True, "how": "connection blocked"}


def run(progress=None):
    progress = progress or (lambda done, total: None)
    hosts = hosts_entries()
    jobs = [(cat, d) for cat, domains in TESTS.items() for d in domains]
    results = {cat: [] for cat in TESTS}
    done = 0
    with ThreadPoolExecutor(max_workers=16) as pool:
        futures = {pool.submit(check_domain, d, hosts): cat for cat, d in jobs}
        for future, cat in futures.items():
            results[cat].append(future.result())
            done += 1
            progress(done, len(jobs))
    for cat in results:  # keep the list order stable
        order = TESTS[cat]
        results[cat].sort(key=lambda r: order.index(r["domain"]))
    blocked = sum(r["blocked"] for rows in results.values() for r in rows)
    servers = dns_servers()
    return {"categories": results, "blocked": blocked, "total": len(jobs),
            "score": round(100 * blocked / len(jobs)),
            "dns": [{"ip": s, "name": BLOCKING_DNS.get(s, "")} for s in servers],
            "hosts_count": len(hosts)}
