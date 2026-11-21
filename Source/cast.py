"""Cast to this PC from a browser: a laptop (Windows, Mac, Linux, Chromebook) opens Swiftlane's cast
page, shares its screen, and the picture shows in a window here.

The picture goes straight from the laptop's browser to Swiftlane's window over the home network
(WebRTC, always encrypted). Swiftlane's phone server only passes the two short connection
descriptions ("offer" and "answer") back and forth. Safety:
- it only works while "Cast from a browser" is on, and the link carries a random code that's new
  every time you switch it on (it's separate from the phone pairing code);
- every cast waits for you to click Allow on this PC;
- at most 3 requests wait at once, and they expire after 2 minutes;
- addresses in the descriptions are replaced with the ones the connection really came from, so
  the picture only flows between the laptop and this PC.
"""
import hmac
import ipaddress
import re
import secrets
import threading
import time

MAX_SDP = 64 * 1024
WAIT_SECONDS = 120
MAX_WAITING = 3


def _host(ip):
    """IPv4 inside IPv6 (::ffff:1.2.3.4) as plain IPv4."""
    try:
        addr = ipaddress.ip_address(ip)
    except ValueError:
        return None
    if addr.version == 6 and addr.ipv4_mapped:
        addr = addr.ipv4_mapped
    return str(addr)


def pin_addresses(sdp, ip):
    """Replace the browser's hidden addresses (random names ending in .local) with the address the
    request really came from, and drop candidates that point anywhere else."""
    ip = _host(ip)
    out = []
    for line in sdp.replace("\r\n", "\n").split("\n"):
        if line.startswith("a=candidate:"):
            parts = line.split(" ")
            if len(parts) < 8 or parts[7] != "host":
                continue  # only direct addresses on the home network, nothing relayed elsewhere
            if parts[4].endswith(".local") or _host(parts[4]) == ip:
                parts[4] = ip
            else:
                continue
            line = " ".join(parts)
        out.append(line)
    return "\r\n".join(out)


class CastHub:
    def __init__(self, on_offer):
        self.on_offer = on_offer      # on_offer(info): show "X wants to show its screen" on the PC
        self.token = None
        self.offers = {}
        self.lock = threading.Lock()

    @property
    def on(self):
        return self.token is not None

    def enable(self):
        if not self.token:
            self.token = secrets.token_urlsafe(9)

    def disable(self):
        self.token = None
        with self.lock:
            self.offers.clear()

    def check(self, supplied):
        token = self.token
        return bool(token) and hmac.compare_digest(str(supplied or "").encode(), token.encode())

    def _prune(self):
        now = time.time()
        for k in [k for k, o in self.offers.items() if o["status"] == "waiting" and now - o["time"] > WAIT_SECONDS]:
            self.offers[k]["status"] = "expired"
        for k in [k for k, o in self.offers.items() if now - o["time"] > 3600]:
            del self.offers[k]

    def offer(self, ip, sdp, name):
        if not isinstance(sdp, str) or not sdp.startswith("v=0") or len(sdp) > MAX_SDP or "m=video" not in sdp:
            raise ValueError("That isn't a screen to show.")
        name = re.sub(r"[^\w .'()-]", "", str(name or ""), flags=re.UNICODE).strip()[:40] or "A browser"
        with self.lock:
            self._prune()
            if sum(o["status"] == "waiting" for o in self.offers.values()) >= MAX_WAITING:
                raise RuntimeError("Too many requests are waiting. Try again in a moment.")
            oid = secrets.token_urlsafe(12)
            self.offers[oid] = {"id": oid, "ip": _host(ip), "name": name, "sdp": pin_addresses(sdp, ip),
                                "status": "waiting", "answer": None, "time": time.time()}
        self.on_offer({"id": oid, "name": name, "ip": _host(ip)})
        return oid

    def get(self, oid):
        with self.lock:
            return self.offers.get(oid)

    def decide(self, oid, allow):
        with self.lock:
            o = self.offers.get(oid)
            if not o or o["status"] != "waiting":
                return None
            o["status"] = "allowed" if allow else "denied"
            return dict(o)

    def answer(self, oid, sdp):
        with self.lock:
            o = self.offers.get(oid)
            if o and o["status"] == "allowed" and isinstance(sdp, str) and sdp.startswith("v=0") and len(sdp) <= MAX_SDP:
                o["answer"], o["status"] = sdp, "answered"
                return True
        return False

    on_end = None   # on_end(oid): the laptop stopped, close its window

    def end(self, oid, ip=None):
        """Ended by the PC (ip None) or by the laptop that sent it (its ip must match)."""
        with self.lock:
            o = self.offers.get(oid)
            if not o or (ip is not None and o["ip"] != _host(ip)) or o["status"] == "ended":
                return False
            o["status"] = "ended"
        if ip is not None and self.on_end:
            self.on_end(oid)
        return True

    def status(self, oid, ip, own_ip):
        """What the laptop sees while it waits. own_ip: this PC's address on that connection."""
        with self.lock:
            self._prune()
            o = self.offers.get(oid)
            if not o or o["ip"] != _host(ip):
                return {"status": "unknown"}
            if o["status"] == "answered":
                return {"status": "answered", "sdp": pin_addresses(o["answer"], own_ip)}
            return {"status": o["status"]}
