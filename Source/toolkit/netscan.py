"""Network scanner: which devices are on your home network right now.

Only looks at the private /24 network your PC is on (e.g. 192.168.0.1-254). It sends one ping to each
address and then reads Windows' own ARP table, which lists every device that answered on the local
network (even ones that ignore pings). No ports are tried and nothing is sent anywhere else. Names
come from your router's DNS, when it knows them.

Devices you've seen before are remembered by their hardware (MAC) address, so new ones stand out.
"""
import ctypes, ipaddress, json, re, socket, struct, subprocess, sys, threading, time
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

from .common import Cancelled

PRIVATE = [ipaddress.ip_network(n) for n in ("10.0.0.0/8", "172.16.0.0/12", "192.168.0.0/16")]
MAC_RE = re.compile(r"^\s*(\d+\.\d+\.\d+\.\d+)\s+([0-9a-f]{2}(?:-[0-9a-f]{2}){5})\s", re.I | re.M)


def _no_window():
    return {"creationflags": 0x08000000} if sys.platform == "win32" else {}


def primary_ip():
    """The address Windows uses for the internet (sends nothing)."""
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    try:
        s.connect(("8.8.8.8", 80))
        return s.getsockname()[0]
    except OSError:
        return None
    finally:
        s.close()


def home_network(ip=None):
    """The /24 around our own address, only if it's a private (home/office) network."""
    ip = ip or primary_ip()
    if not ip:
        return None
    addr = ipaddress.ip_address(ip)
    if not any(addr in n for n in PRIVATE):
        return None
    return ipaddress.ip_network(f"{ip}/24", strict=False)


def gateway(ip):
    """The router for that address (GetBestRoute's next hop)."""
    if sys.platform != "win32":
        return None
    row = (ctypes.c_ulong * 14)()
    dest = struct.unpack("<I", socket.inet_aton("8.8.8.8"))[0]
    src = struct.unpack("<I", socket.inet_aton(ip))[0]
    if ctypes.windll.iphlpapi.GetBestRoute(dest, src, ctypes.byref(row)) != 0:
        return None
    hop = socket.inet_ntoa(struct.pack("<I", row[3]))
    return hop if hop != "0.0.0.0" else None


class _Pinger:
    """ICMP echo through Windows' IcmpSendEcho (works without admin rights)."""

    def __init__(self):
        api = ctypes.windll.iphlpapi
        api.IcmpCreateFile.restype = ctypes.c_void_p
        api.IcmpSendEcho.argtypes = [ctypes.c_void_p, ctypes.c_ulong, ctypes.c_void_p, ctypes.c_ushort, ctypes.c_void_p,
                                     ctypes.c_void_p, ctypes.c_ulong, ctypes.c_ulong]
        api.IcmpCloseHandle.argtypes = [ctypes.c_void_p]
        self.api = api

    def ping(self, ip, timeout_ms=700):
        h = self.api.IcmpCreateFile()
        try:
            data = b"swiftlane"
            reply = ctypes.create_string_buffer(256)
            n = self.api.IcmpSendEcho(h, struct.unpack("<I", socket.inet_aton(ip))[0], data, len(data), None, reply, 256, timeout_ms)
            return n > 0 and struct.unpack_from("<I", reply.raw, 4)[0] == 0
        finally:
            self.api.IcmpCloseHandle(h)


def arp_table():
    """{ip: mac} for everything Windows currently knows on the local network."""
    try:
        out = subprocess.run(["arp", "-a"], capture_output=True, text=True, timeout=10, **_no_window()).stdout
    except (OSError, subprocess.SubprocessError):
        return {}
    table = {}
    for ip, mac in MAC_RE.findall(out):
        mac = mac.lower().replace("-", ":")
        if mac in ("ff:ff:ff:ff:ff:ff", "00:00:00:00:00:00") or mac.startswith("01:00:5e"):
            continue
        table[ip] = mac
    return table


def own_mac(ip):
    try:
        out = subprocess.run(["getmac", "/v", "/fo", "csv"], capture_output=True, text=True, timeout=10, **_no_window()).stdout
    except (OSError, subprocess.SubprocessError):
        return None
    # getmac doesn't print IPs; the adapter used for `ip` is usually the only connected one with a MAC
    macs = re.findall(r'"([0-9A-F]{2}(?:-[0-9A-F]{2}){5})"', out)
    return macs[0].lower().replace("-", ":") if len(macs) == 1 else None


def randomized(mac):
    """Phones and newer laptops use a made-up ('private') MAC per Wi-Fi network: the locally administered bit."""
    try:
        return bool(int(mac.split(":")[0], 16) & 0x02)
    except (ValueError, AttributeError, IndexError):
        return False


def reverse_name(ip, timeout=1.5):
    box = {}
    t = threading.Thread(target=lambda: box.update(name=socket.gethostbyaddr(ip)[0]), daemon=True)
    t.start()
    t.join(timeout)
    name = box.get("name")
    if not name or name == ip:
        return None
    return re.sub(r"[^\w.\- ]", "", name)[:80] or None


def scan(progress=None, cancelled=None):
    ip = primary_ip()
    net = home_network(ip)
    if not net:
        raise ValueError("You don't seem to be on a home or office network right now.")
    gw = gateway(ip)
    hosts = [str(h) for h in net.hosts()]
    pinger, alive, done = _Pinger(), set(), [0]
    lock = threading.Lock()

    def one(h):
        if cancelled and cancelled():
            return
        ok = pinger.ping(h)
        with lock:
            done[0] += 1
            if ok:
                alive.add(h)
            if progress and done[0] % 8 == 0:
                progress(done[0] / len(hosts) * 0.85, len(alive))

    with ThreadPoolExecutor(64) as pool:
        list(pool.map(one, hosts))
    if cancelled and cancelled():
        raise Cancelled()
    arp = {k: v for k, v in arp_table().items() if ipaddress.ip_address(k) in net}
    found = sorted(alive | set(arp) | {ip}, key=lambda a: ipaddress.ip_address(a))
    with ThreadPoolExecutor(32) as pool:
        names = dict(zip(found, pool.map(reverse_name, found)))
    if progress:
        progress(1, len(found))
    devices = []
    for a in found:
        mac = arp.get(a) or (own_mac(a) if a == ip else None)
        devices.append({"ip": a, "mac": mac, "name": names.get(a) or (socket.gethostname() if a == ip else None),
                        "self": a == ip, "router": a == gw, "randomized": randomized(mac) if mac else False,
                        "pings": a in alive})
    return {"network": str(net), "ip": ip, "gateway": gw, "gateway_mac": arp.get(gw), "devices": devices, "time": int(time.time())}


class Memory:
    """Devices seen before, per network (keyed by the router's MAC), with names you gave them."""

    def __init__(self, path):
        self.path = Path(path)
        try:
            self.data = json.loads(self.path.read_text("utf-8"))
        except (OSError, ValueError):
            self.data = {"networks": {}}

    def _net(self, result):
        key = result.get("gateway_mac") or result["network"]
        return self.data["networks"].setdefault(key, {"devices": {}})

    @staticmethod
    def _key(d):
        return d["mac"] or ("ip:" + d["ip"])

    def merge(self, result):
        """Marks each device new/known and remembers it. Returns the new ones."""
        net, now, new = self._net(result), result["time"], []
        first_scan = not net["devices"]
        for d in result["devices"]:
            k = self._key(d)
            seen = net["devices"].get(k)
            if seen is None:
                seen = net["devices"][k] = {"first": now, "label": ""}
                if not first_scan and not d["self"]:
                    new.append(d)
            seen.update(last=now, ip=d["ip"], name=d["name"])
            d.update(key=k, first=seen["first"], label=seen.get("label", ""), new=seen.get("new", False) or d in new)
            if d in new:
                seen["new"] = True
        result["missing"] = [{"key": k, **{x: v.get(x) for x in ("ip", "name", "label", "last")}}
                             for k, v in net["devices"].items() if k not in {self._key(d) for d in result["devices"]}]
        self.save()
        return new

    def label(self, result, key, text):
        dev = self._net(result)["devices"].get(key)
        if dev is None:
            return False
        dev["label"] = re.sub(r"[\x00-\x1f]", "", str(text))[:60]
        dev["new"] = False
        self.save()
        return True

    def acknowledge(self, result):
        for dev in self._net(result)["devices"].values():
            dev["new"] = False
        self.save()

    def forget(self, result, key):
        self._net(result)["devices"].pop(key, None)
        self.save()

    def save(self):
        tmp = self.path.with_name(self.path.name + ".tmp")
        tmp.write_text(json.dumps(self.data, indent=1), "utf-8")
        tmp.replace(self.path)
