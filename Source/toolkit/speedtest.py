"""Internet speed test against Cloudflare's free speed-test servers (speed.cloudflare.com).

run(progress, cancelled) measures ping/jitter, then download and upload with several
parallel connections, reporting live Mbps through progress(phase, value, extra).
"""
import http.client
import json
import os
import statistics
import threading
import time

from .common import Cancelled

HOST = "speed.cloudflare.com"
STREAMS = 4
RATE_LIMITED = ("Cloudflare is limiting speed tests from your connection right now (too many tests in a short time). "
                "Try again in a few minutes.")


class RateLimited(Exception):
    pass
PHASE_SECONDS = 8
CHUNK = 64 * 1024


def _conn():
    return http.client.HTTPSConnection(HOST, timeout=15)


CITIES = {  # Cloudflare locations (airport codes) you're likely to get in Europe
    "TXL": "Berlin", "BER": "Berlin", "FRA": "Frankfurt", "HAM": "Hamburg", "MUC": "Munich", "DUS": "Düsseldorf",
    "STR": "Stuttgart", "AMS": "Amsterdam", "LHR": "London", "CDG": "Paris", "WAW": "Warsaw", "MXP": "Milan",
    "FCO": "Rome", "VIE": "Vienna", "ZRH": "Zurich", "PRG": "Prague", "CPH": "Copenhagen", "BRU": "Brussels",
    "MAD": "Madrid", "BCN": "Barcelona", "ARN": "Stockholm", "OSL": "Oslo", "HEL": "Helsinki", "DUB": "Dublin",
}


def server_info():
    """Which Cloudflare location we're measuring against, and IPv4/IPv6."""
    c = _conn()
    try:
        c.request("GET", "/cdn-cgi/trace", headers={"User-Agent": "Swiftlane"})
        text = c.getresponse().read().decode("utf-8", "replace")
    finally:
        c.close()
    data = dict(line.split("=", 1) for line in text.splitlines() if "=" in line)
    colo = data.get("colo", "")
    return {"colo": colo, "city": CITIES.get(colo, colo), "country": data.get("loc"),
            "ip_version": "IPv6" if ":" in data.get("ip", "") else "IPv4"}


def measure_ping(count=10, cancelled=lambda: False):
    """Round trips of an empty request over one kept-open connection (the first one is warm-up)."""
    c = _conn()
    times = []
    try:
        for i in range(count + 1):
            if cancelled():
                raise Cancelled()
            t0 = time.perf_counter()
            c.request("GET", "/__down?bytes=0", headers={"User-Agent": "Swiftlane"})
            r = c.getresponse()
            r.read()
            if r.status == 429:
                raise RuntimeError(RATE_LIMITED)
            elapsed = (time.perf_counter() - t0) * 1000
            # Cloudflare reports its own processing time; take it out to get the network round trip
            server = r.getheader("Server-Timing") or ""
            try:
                elapsed -= float(server.split("dur=")[1].split(",")[0])
            except (IndexError, ValueError):
                pass
            if i:
                times.append(max(0.1, elapsed))
    finally:
        c.close()
    jitter = statistics.mean(abs(a - b) for a, b in zip(times, times[1:])) if len(times) > 1 else 0
    return {"ping": statistics.median(times), "jitter": jitter}


def _transfer(direction, progress, cancelled):
    """Run STREAMS parallel downloads/uploads for PHASE_SECONDS; return Mbps."""
    total = [0]
    lock = threading.Lock()
    stop = threading.Event()
    errors = []
    payload = os.urandom(4 * 1024 * 1024) if direction == "up" else None

    def worker():
        try:
            while not stop.is_set():
                c = _conn()
                try:
                    if direction == "down":
                        c.request("GET", "/__down?bytes=25000000", headers={"User-Agent": "Swiftlane"})
                        r = c.getresponse()
                        if r.status == 429:
                            raise RateLimited()
                        if r.status != 200:
                            raise RuntimeError(f"server answered {r.status}")
                        while not stop.is_set():
                            data = r.read(CHUNK)
                            if not data:
                                break
                            with lock:
                                total[0] += len(data)
                    else:
                        c.putrequest("POST", "/__up")
                        c.putheader("Content-Length", str(len(payload)))
                        c.putheader("Content-Type", "application/octet-stream")
                        c.putheader("User-Agent", "Swiftlane")
                        c.endheaders()
                        view = memoryview(payload)
                        for i in range(0, len(payload), CHUNK):
                            if stop.is_set():
                                break
                            c.send(view[i:i + CHUNK])
                            with lock:
                                total[0] += min(CHUNK, len(payload) - i)
                        if not stop.is_set():
                            r = c.getresponse()
                            r.read()
                            if r.status == 429:
                                raise RateLimited()
                finally:
                    c.close()
        except Exception as e:  # a dropped stream shouldn't end the test
            errors.append(e)
            if isinstance(e, RateLimited):
                stop.set()

    threads = [threading.Thread(target=worker, daemon=True) for _ in range(STREAMS)]
    for th in threads:
        th.start()
    start = time.perf_counter()
    warmup = 1.0  # ignore the TCP slow-start second
    base_bytes, base_time = 0, None
    samples = []
    try:
        while time.perf_counter() - start < PHASE_SECONDS:
            if cancelled():
                raise Cancelled()
            time.sleep(0.25)
            now = time.perf_counter()
            with lock:
                done = total[0]
            if base_time is None and now - start >= warmup:
                base_bytes, base_time = done, now
            if base_time and now > base_time:
                mbps = (done - base_bytes) * 8 / (now - base_time) / 1e6
                samples.append(mbps)
                progress(direction, mbps, (now - start) / PHASE_SECONDS)
    finally:
        stop.set()
    if any(isinstance(e, RateLimited) for e in errors):
        raise RuntimeError(RATE_LIMITED)
    with lock:
        moved = total[0]
    if not samples or moved == 0:
        raise RuntimeError("No data got through. Check your internet connection." if errors else "The test was too short.")
    return samples[-1]


def run(progress=None, cancelled=None):
    progress = progress or (lambda phase, value, fraction: None)
    cancelled = cancelled or (lambda: False)
    result = {}
    try:
        result["server"] = server_info()
    except Exception:
        result["server"] = {}
    progress("ping", None, 0)
    result.update(measure_ping(cancelled=cancelled))
    progress("ping", result["ping"], 1)
    result["download"] = _transfer("down", progress, cancelled)
    result["upload"] = _transfer("up", progress, cancelled)
    return result
