"""'Is my PC exposed?': a read-only look at the ways in.

- which programs accept connections from the network (TCP listeners, from Windows' own table)
- whether the Windows firewall is on, and whether this network counts as Public or Private
- Remote Desktop and the old SMB1 file sharing
- port forwards on your router (UPnP): the router is asked which doors it has opened to the internet

Nothing is changed here. Fixes are separate functions the window calls only when you click one.
"""
import ctypes, ipaddress, json, re, socket, struct, subprocess, sys, urllib.parse, urllib.request, winreg
import xml.etree.ElementTree as ET
from ctypes import wintypes

from . import netscan

# What the usual listening ports are, in plain words.
KNOWN = {
    135: ("Windows (RPC)", "Lets Windows programs talk to each other. Normal; the firewall keeps it inside your network."),
    139: ("File and printer sharing", "Older part of Windows file sharing."),
    445: ("File and printer sharing (SMB)", "Shared folders and printers. Normal at home; never forward it on your router."),
    3389: ("Remote Desktop", "Someone with your password can control this PC. Turn off if you don't use it."),
    5040: ("Windows (device connect)", "Used by Windows for nearby sharing and phone link."),
    5357: ("Network discovery", "Lets other PCs see this one in the Network folder."),
    7680: ("Windows Update sharing", "Delivery Optimization: shares update pieces with PCs on your network."),
    2869: ("UPnP / media sharing", "Windows media and device sharing."),
    5985: ("Remote management (WinRM)", "Lets administrators run commands remotely. Rarely needed at home."),
    22: ("SSH server", "Remote command line. Only keep if you set it up yourself."),
    21: ("FTP server", "Unencrypted file transfer server."),
    23: ("Telnet server", "Unencrypted remote access. Should never be on."),
    5900: ("VNC remote control", "Screen sharing / remote control."),
    8765: ("Swiftlane phone transfer", "Your phones send and receive files here (pairing code needed)."),
    8766: ("Swiftlane share links", "Only runs while a share link is active; reached through Tailscale Funnel."),
}
SAFE_LEVEL = {"ok": 0, "info": 1, "warn": 2, "bad": 3}


def _no_window():
    return {"creationflags": 0x08000000}


# ---------------------------------------------------------------- listening ports
def _process_names():
    """{pid: name}, with services named for svchost processes."""
    names = {0: "System Idle", 4: "Windows (System)"}
    try:
        out = subprocess.run(["tasklist", "/svc", "/fo", "csv", "/nh"], capture_output=True, text=True, timeout=15, **_no_window()).stdout
        for line in out.splitlines():
            cols = re.findall(r'"([^"]*)"', line)
            if len(cols) >= 3 and cols[1].isdigit():
                exe, services = cols[0], cols[2]
                names[int(cols[1])] = f"{exe} ({services})" if exe.lower() == "svchost.exe" and services not in ("N/A", "") else exe
    except (OSError, subprocess.SubprocessError):
        pass
    return names


def _tcp_listeners(family):
    iphlp = ctypes.windll.iphlpapi
    size = wintypes.DWORD(0)
    iphlp.GetExtendedTcpTable(None, ctypes.byref(size), False, family, 3, 0)  # 3 = TCP_TABLE_OWNER_PID_LISTENER
    buf = ctypes.create_string_buffer(size.value + 4096)
    if iphlp.GetExtendedTcpTable(buf, ctypes.byref(size), False, family, 3, 0) != 0:
        return []
    raw, out = buf.raw, []
    count = struct.unpack_from("<I", raw, 0)[0]
    if family == socket.AF_INET:
        for i in range(count):
            state, addr, port, _, _, pid = struct.unpack_from("<IIIIII", raw, 4 + i * 24)
            out.append((socket.inet_ntoa(struct.pack("<I", addr)), socket.ntohs(port & 0xFFFF), pid))
    else:
        for i in range(count):
            off = 4 + i * 56
            addr = raw[off:off + 16]
            port = struct.unpack_from("<I", raw, off + 20)[0]
            pid = struct.unpack_from("<I", raw, off + 52)[0]
            out.append((socket.inet_ntop(socket.AF_INET6, addr), socket.ntohs(port & 0xFFFF), pid))
    return out


def listeners():
    names = _process_names()
    seen = {}
    for family in (socket.AF_INET, socket.AF_INET6):
        for addr, port, pid in _tcp_listeners(family):
            local_only = addr in ("127.0.0.1", "::1") or addr.startswith("127.")
            key = (port, pid)
            item = seen.setdefault(key, {"port": port, "pid": pid, "program": names.get(pid, f"PID {pid}"), "addresses": [], "network": False})
            item["addresses"].append(addr)
            item["network"] = item["network"] or not local_only
    out = []
    for item in seen.values():
        known = KNOWN.get(item["port"])
        dynamic = item["port"] >= 49152 and re.match(r"(?i)(lsass|wininit|services|spoolsv|svchost)", item["program"] or "")
        item["what"] = known[0] if known else ("Windows (internal)" if dynamic else None)
        item["why"] = known[1] if known else ("Windows services that only answer other Windows parts." if dynamic else None)
        out.append(item)
    return sorted(out, key=lambda x: (not x["network"], x["port"]))


# ---------------------------------------------------------------- firewall, network type, RDP, SMB1
def firewall():
    script = ("$p = Get-NetFirewallProfile | Select-Object Name,@{n='Enabled';e={[string]$_.Enabled}},@{n='Inbound';e={[string]$_.DefaultInboundAction}};"
              "$c = Get-NetConnectionProfile | Select-Object Name,InterfaceAlias,@{n='Category';e={[string]$_.NetworkCategory}};"
              "@{profiles=@($p); networks=@($c)} | ConvertTo-Json -Depth 4 -Compress")
    try:
        out = subprocess.run(["powershell", "-NoProfile", "-NonInteractive", "-Command", script], capture_output=True, text=True,
                             timeout=30, **_no_window()).stdout
        data = json.loads(out)
    except (OSError, subprocess.SubprocessError, ValueError):
        return None
    on = lambda v: str(v).lower() in ("true", "1")
    return {"profiles": [{"name": p["Name"], "enabled": on(p["Enabled"]), "inbound": p["Inbound"]} for p in data.get("profiles") or []],
            "networks": [{"name": n.get("Name"), "adapter": n.get("InterfaceAlias"), "category": n.get("Category")} for n in data.get("networks") or []]}


def _reg(path, name):
    try:
        with winreg.OpenKey(winreg.HKEY_LOCAL_MACHINE, path) as k:
            return winreg.QueryValueEx(k, name)[0]
    except OSError:
        return None


def remote_desktop_on():
    return _reg(r"SYSTEM\CurrentControlSet\Control\Terminal Server", "fDenyTSConnections") == 0


def smb1_installed():
    try:
        winreg.OpenKey(winreg.HKEY_LOCAL_MACHINE, r"SYSTEM\CurrentControlSet\Services\mrxsmb10").Close()
        return True
    except OSError:
        return False


# ---------------------------------------------------------------- UPnP on the router
def _lan_url(url, gateway):
    """Only talk to the router itself: an http URL whose host is the gateway's private address."""
    try:
        u = urllib.parse.urlsplit(url)
        host = ipaddress.ip_address(u.hostname)
    except (ValueError, TypeError):
        return False
    return u.scheme == "http" and host.is_private and (gateway is None or str(host) == gateway)


def _fetch(url, data=None, headers=None, limit=256 * 1024):
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))  # never through a proxy
    req = urllib.request.Request(url, data=data, headers=headers or {})
    with opener.open(req, timeout=4) as r:
        body = r.read(limit + 1)
    if len(body) > limit:
        raise ValueError("answer too big")
    return body


def _soap(control, service, action, args=None):
    body = "".join(f"<{k}>{v}</{k}>" for k, v in (args or {}).items())
    envelope = ('<?xml version="1.0"?><s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/" '
                's:encodingStyle="http://schemas.xmlsoap.org/soap/encoding/"><s:Body>'
                f'<u:{action} xmlns:u="{service}">{body}</u:{action}></s:Body></s:Envelope>').encode()
    xml = _fetch(control, envelope, {"Content-Type": 'text/xml; charset="utf-8"', "SOAPAction": f'"{service}#{action}"'})
    root = ET.fromstring(xml)
    resp = next((e for e in root.iter() if e.tag.endswith(action + "Response")), None)
    if resp is None:
        raise LookupError(action)
    return {c.tag.split("}")[-1]: (c.text or "") for c in resp}


def find_router_upnp(gateway, timeout=2.5):
    """(control URL, service type) of the router's port-forwarding service, or None if UPnP is off."""
    msg = ("M-SEARCH * HTTP/1.1\r\nHOST: 239.255.255.250:1900\r\nMAN: \"ssdp:discover\"\r\nMX: 2\r\n"
           "ST: urn:schemas-upnp-org:device:InternetGatewayDevice:1\r\n\r\n").encode()
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    s.settimeout(timeout)
    locations = []
    try:
        s.sendto(msg, ("239.255.255.250", 1900))
        while True:
            data, (addr, _) = s.recvfrom(4096)
            m = re.search(rb"(?im)^location:\s*(\S+)", data)
            if m and (gateway is None or addr == gateway):
                locations.append(m.group(1).decode("ascii", "replace"))
    except (socket.timeout, OSError):
        pass
    finally:
        s.close()
    for loc in dict.fromkeys(locations):
        if not _lan_url(loc, gateway):
            continue
        try:
            root = ET.fromstring(_fetch(loc))
        except (OSError, ValueError, ET.ParseError):
            continue
        for svc in root.iter():
            if not svc.tag.endswith("service"):
                continue
            kids = {c.tag.split("}")[-1]: (c.text or "").strip() for c in svc}
            stype = kids.get("serviceType", "")
            if "WANIPConnection" in stype or "WANPPPConnection" in stype:
                control = urllib.parse.urljoin(loc, kids.get("controlURL", ""))
                if _lan_url(control, gateway):
                    return control, stype
    return None


def port_forwards(gateway):
    """Port forwards the router reports over UPnP. None = UPnP is off (or the router won't say)."""
    found = find_router_upnp(gateway)
    if not found:
        return None
    control, stype = found
    out = []
    for i in range(128):
        try:
            e = _soap(control, stype, "GetGenericPortMappingEntry", {"NewPortMappingIndex": i})
        except Exception:
            break
        out.append({"external": int(e.get("NewExternalPort") or 0), "protocol": e.get("NewProtocol", ""),
                    "internal": int(e.get("NewInternalPort") or 0), "client": e.get("NewInternalClient", ""),
                    "enabled": e.get("NewEnabled", "1") in ("1", "true"), "name": re.sub(r"[\x00-\x1f]", "", e.get("NewPortMappingDescription", ""))[:80],
                    "remote": e.get("NewRemoteHost", ""), "lease": int(e.get("NewLeaseDuration") or 0)})
    return {"control": control, "service": stype, "forwards": out}


def remove_forward(control, service, gateway, external, protocol, remote=""):
    if not _lan_url(control, gateway) or protocol not in ("TCP", "UDP"):
        raise ValueError("Not the router.")
    _soap(control, service, "DeletePortMapping", {"NewRemoteHost": remote, "NewExternalPort": int(external), "NewProtocol": protocol})


# ---------------------------------------------------------------- the whole check
def check(app=None):
    """app: Swiftlane's own state {server_port, https_only, remote, remote_power, share_links, funnel}."""
    ip = netscan.primary_ip()
    gw = netscan.gateway(ip) if ip else None
    found = []

    def add(id_, level, title, detail, fix=None):
        found.append({"id": id_, "level": level, "title": title, "detail": detail, "fix": fix})

    fw = firewall()
    if fw is None:
        add("firewall", "warn", "Couldn't read the firewall settings", "Check Windows Security > Firewall & network protection.")
    else:
        off = [p["name"] for p in fw["profiles"] if not p["enabled"]]
        if off:
            add("firewall", "bad", f"Firewall is off for {', '.join(off)} networks",
                "The firewall is what keeps others on the network out of the programs below.",
                {"action": "firewall_on", "args": [p.lower() for p in off], "label": "Turn the firewall on", "admin": True})
        else:
            add("firewall", "ok", "Firewall is on", "On for Domain, Private and Public networks.")
        for n in fw["networks"]:
            if re.match(r"(?i)(tailscale|vethernet|vmware|virtualbox|wsl)", n["adapter"] or ""):
                continue  # virtual adapters, not a Wi-Fi or cable you choose
            if n["category"] == "Private":
                add("profile", "info", f"\"{n['name']}\" is set as a Private network",
                    "Good at home: your PC can be seen by your own devices. On public Wi-Fi (café, hotel) choose Public instead in Windows settings.")
            elif n["category"] == "Public":
                add("profile", "ok", f"\"{n['name']}\" is set as a Public network", "Windows keeps this PC hidden from others on the network.")

    if remote_desktop_on():
        add("rdp", "warn", "Remote Desktop is on", "Anyone who reaches this PC and knows your password can take control of it.",
            {"action": "rdp_off", "args": [], "label": "Turn Remote Desktop off", "admin": True})
    else:
        add("rdp", "ok", "Remote Desktop is off", "Nobody can take over this PC's screen remotely.")
    if smb1_installed():
        add("smb1", "warn", "Old SMB1 file sharing is installed", "SMB1 was used by the WannaCry attack. Windows 11 doesn't need it.",
            {"action": "smb1_off", "args": [], "label": "Remove SMB1", "admin": True})

    ports = listeners()
    risky = [p for p in ports if p["network"] and p["port"] in (21, 23, 3389, 5900, 5985)]
    open_net = [p for p in ports if p["network"]]
    add("ports", "warn" if risky else "info", f"{len(open_net)} programs accept connections from the network",
        "Listed below. Most are part of Windows. Whether others actually get through is decided by the firewall.")

    upnp = None
    try:
        upnp = port_forwards(gw)
    except Exception:
        upnp = None
    if upnp is None:
        add("upnp", "ok", "Your router doesn't open doors by itself", "UPnP is off on the router (or it doesn't answer), so programs can't open ports to the internet on their own.")
    elif upnp["forwards"]:
        mine = [f for f in upnp["forwards"] if f["client"] == ip]
        add("upnp", "warn", f"Your router forwards {len(upnp['forwards'])} port{'s' if len(upnp['forwards']) != 1 else ''} from the internet",
            ("These reach devices at home directly from the internet." + (f" {len(mine)} go to this PC." if mine else "")) +
            " Remove the ones you don't recognise. To stop programs doing this, turn UPnP off in your router's settings.")
    else:
        add("upnp", "info", "UPnP is on, but no doors are open", "Programs could open ports on your router by themselves. Turning UPnP off in the router's settings is safer.")

    if app:
        if app.get("share_links"):
            add("swiftlane-share", "info", f"{app['share_links']} Swiftlane share link{'s are' if app['share_links'] != 1 else ' is'} reachable from the internet",
                "Only through Tailscale Funnel, only the files you shared, until the links expire.", {"action": "page", "args": ["share"], "label": "Open Share links"})
        if app.get("server_port"):
            level = "ok" if app.get("https_only") else "info"
            add("swiftlane-transfer", level, "Swiftlane phone transfer is listening on your network",
                "Phones need the pairing code. " + ("Only encrypted connections are allowed." if app.get("https_only")
                                                    else "Unencrypted connections are still allowed; you can turn them off in Settings once phones are set up."),
                None if app.get("https_only") else {"action": "page", "args": ["settings"], "label": "Open Settings"})
        if app.get("remote") and app.get("remote_power"):
            add("swiftlane-remote", "info", "Paired phones can sleep or shut down this PC", "From the PC remote. You can turn that off in Settings.",
                {"action": "page", "args": ["settings"], "label": "Open Settings"})

    worst = max((SAFE_LEVEL[f["level"]] for f in found), default=0)
    return {"findings": sorted(found, key=lambda f: -SAFE_LEVEL[f["level"]]), "ports": ports, "upnp": upnp, "ip": ip, "gateway": gw,
            "verdict": ["ok", "ok", "warn", "bad"][worst]}


# ---------------------------------------------------------------- fixes (each one asks Windows for admin)
FIX_COMMANDS = {
    "firewall_on": lambda profiles: ("netsh", " ".join(["advfirewall", "set", "allprofiles" if len(profiles) > 1 else f"{profiles[0]}profile", "state", "on"])),
    "rdp_off": lambda _: ("reg", r'add "HKLM\SYSTEM\CurrentControlSet\Control\Terminal Server" /v fDenyTSConnections /t REG_DWORD /d 1 /f'),
    "smb1_off": lambda _: ("dism", "/online /disable-feature /featurename:SMB1Protocol /norestart"),
}


def fix_command(action, args):
    if action not in FIX_COMMANDS:
        raise ValueError("Unknown fix.")
    if action == "firewall_on":
        args = [a for a in args if a in ("domain", "private", "public")]
        if not args:
            raise ValueError("Unknown firewall profile.")
    return FIX_COMMANDS[action](args)
