"""Send to contacts: Swiftlane to Swiftlane, straight from one PC to the other.

The connection is made with iroh (encrypted QUIC). Each PC has its own identity key; the public half is
its contact code. Two PCs find each other by key through iroh's public discovery service and connect
directly through their routers without opening any ports; only when a direct path can't be made does
the (still end-to-end encrypted) data go through an iroh relay. Files are never stored anywhere else.

Rules:
- Only contacts get in: a connection from a key that isn't in your contacts is closed without a word.
- The receiver always decides: nothing is written until they click Accept.
- Every file is checked (size and SHA-256); a broken or cancelled file is deleted, never kept half-done.
- Received files get Windows' "from the internet" mark and never overwrite anything.
"""
import asyncio, base64, hashlib, json, os, re, secrets, shutil, struct, threading, time
from pathlib import Path

ALPN = b"swiftlane/send/1"
CODE_PREFIX = "SL1"
FRAME_MAX = 64 * 1024        # a control message (offer, answer, result) is never bigger than this
MAX_FILES = 1000
ASK_SECONDS = 120            # how long the receiver has to accept
CHUNK = 1 << 20
HEADROOM = 512 * 1024 ** 2   # keep at least this much free on the receiver's disk


class PeerError(Exception):
    """Something to show to the user as it is."""


# ---------------------------------------------------------------- contact codes
def make_code(endpoint_id, name):
    """What you give to a friend: SL1.<key>.<your name>. Contains no address, nothing secret."""
    label = base64.urlsafe_b64encode(clean_name(name).encode()).decode().rstrip("=")
    return f"{CODE_PREFIX}.{endpoint_id}.{label}"


def parse_code(code):
    """(endpoint id, name) from a contact code; raises PeerError if it isn't one."""
    parts = re.sub(r"\s+", "", str(code or "")).split(".")
    if len(parts) != 3 or parts[0] != CODE_PREFIX or not re.fullmatch(r"[a-z0-9]{40,80}", parts[1]):
        raise PeerError("That isn't a Swiftlane contact code.")
    try:
        name = base64.urlsafe_b64decode(parts[2] + "=" * (-len(parts[2]) % 4)).decode()
    except (ValueError, UnicodeDecodeError):
        name = ""
    return parts[1], clean_name(name) or "Contact"


def safety_words(endpoint_id):
    """A short check both people can read out to each other: same on both screens = the right person."""
    h = hashlib.sha256(endpoint_id.encode()).hexdigest().upper()
    return "-".join(h[i:i + 4] for i in range(0, 16, 4))


def clean_name(name):
    return re.sub(r"[\x00-\x1f<>]", "", str(name or "")).strip()[:40]


# ---------------------------------------------------------------- contacts
class Contacts:
    def __init__(self, path):
        self.path = Path(path)
        self.lock = threading.Lock()
        try:
            self.items = json.loads(self.path.read_text("utf-8"))
        except (OSError, ValueError):
            self.items = []

    def save(self):
        tmp = self.path.with_name(self.path.name + ".tmp")
        tmp.write_text(json.dumps(self.items, indent=1), "utf-8")
        tmp.replace(self.path)

    def list(self):
        with self.lock:
            return [dict(c, safety=safety_words(c["id"])) for c in self.items]

    def get(self, cid):
        with self.lock:
            return next((dict(c) for c in self.items if c["id"] == cid), None)

    def add(self, code, name=None, own_id=None):
        cid, code_name = parse_code(code)
        if cid == own_id:
            raise PeerError("That's your own contact code.")
        with self.lock:
            if any(c["id"] == cid for c in self.items):
                raise PeerError("That person is already in your contacts.")
            entry = {"id": cid, "name": clean_name(name) or code_name, "added": int(time.time()), "last": None}
            self.items.append(entry)
            self.save()
        return dict(entry)

    def remove(self, cid):
        with self.lock:
            self.items = [c for c in self.items if c["id"] != cid]
            self.save()

    def rename(self, cid, name):
        with self.lock:
            for c in self.items:
                if c["id"] == cid and clean_name(name):
                    c["name"] = clean_name(name)
            self.save()

    def seen(self, cid):
        with self.lock:
            for c in self.items:
                if c["id"] == cid:
                    c["last"] = int(time.time())
            self.save()


# ---------------------------------------------------------------- framing
async def _send_frame(stream, obj):
    data = json.dumps(obj).encode()
    await stream.write_all(struct.pack(">I", len(data)) + data)


async def _read_frame(stream):
    size = struct.unpack(">I", await stream.read_exact(4))[0]
    if size > FRAME_MAX:
        raise PeerError("The other side sent something too big.")
    return json.loads(await stream.read_exact(size))


# ---------------------------------------------------------------- the peer
class Peer:
    """One PC's endpoint. Runs its own asyncio loop in a background thread.

    hooks (all called from that thread, so keep them quick):
      contacts:  Contacts
      my_name(): str                                   shown to the people you send to
      ask(offer) -> threading.Event-like decision:      called for an incoming offer from a contact; must
                    return an object with .wait(timeout) -> bool and .accepted: bool
      save_dir(): Path                                  where accepted files go
      place(tmp, name) -> Path                          moves a finished temp file to its final place
      received(path, size, contact): None               after each file is saved
      progress(transfer): None                          live updates (incoming or outgoing)
    """

    def __init__(self, secret_key, contacts, my_name, ask, save_dir, place, received=None, progress=None):
        import iroh  # only loaded when sending to contacts is used
        self.iroh = iroh
        self.secret_key = secret_key
        self.contacts = contacts
        self.my_name, self.ask, self.save_dir, self.place = my_name, ask, save_dir, place
        self.received_hook = received or (lambda *a: None)
        self.progress_hook = progress or (lambda t: None)
        self.loop = asyncio.new_event_loop()
        self.endpoint = None
        self.ready = threading.Event()
        self.error = None
        self.transfers = {}  # id -> live info
        self._thread = threading.Thread(target=self._run, daemon=True, name="swiftlane-peer")

    @property
    def id(self):
        return str(self.iroh.SecretKey.from_bytes(self.secret_key).public())

    # -- lifecycle
    def start(self, timeout=25):
        self._thread.start()
        self.ready.wait(timeout)
        if self.error:
            raise PeerError(self.error)
        return self

    def _run(self):
        asyncio.set_event_loop(self.loop)
        self.loop.run_until_complete(self._serve())

    async def _serve(self):
        iroh = self.iroh
        try:
            self.endpoint = await iroh.Endpoint.bind(iroh.EndpointOptions(preset=iroh.preset_n0(), secret_key=self.secret_key, alpns=[ALPN]))
            try:
                await asyncio.wait_for(self.endpoint.online(), 20)
            except asyncio.TimeoutError:
                pass  # no relay yet (offline?): direct connections on the local network still work
        except Exception as e:
            self.error = f"Couldn't start sending to contacts ({e.__class__.__name__})."
            self.ready.set()
            return
        self.ready.set()
        while not self.endpoint.is_closed():
            try:
                incoming = await self.endpoint.accept_next()
            except Exception:
                break
            if incoming is None:
                break
            asyncio.ensure_future(self._handle(incoming))

    def stop(self, wait=3):
        """Close the endpoint (others can no longer reach this PC). Doesn't hold up closing Swiftlane for long."""
        if self.endpoint and not self.endpoint.is_closed():
            fut = asyncio.run_coroutine_threadsafe(self.endpoint.close(), self.loop)
            try:
                fut.result(wait)
            except Exception:
                pass

    @property
    def running(self):
        return bool(self.ready.is_set() and not self.error and self.endpoint and not self.endpoint.is_closed())

    # -- receiving
    async def _handle(self, incoming):
        try:
            conn = await (await incoming.accept()).connect()
        except Exception:
            return
        sender = str(conn.remote_id())
        contact = self.contacts.get(sender)
        if not contact:  # not one of your contacts: say nothing, take nothing
            conn.close(1, b"")
            return
        tid = secrets.token_hex(6)
        info = {"id": tid, "dir": "in", "contact": contact["name"], "contact_id": sender, "status": "asking",
                "files": [], "total": 0, "done": 0, "error": None}
        tmp_paths = []
        try:
            bi = await asyncio.wait_for(conn.accept_bi(), 30)
            r, w = bi.recv(), bi.send()
            offer = await asyncio.wait_for(_read_frame(r), 30)
            files = offer.get("files") if isinstance(offer, dict) else None
            if offer.get("t") != "offer" or not isinstance(files, list) or not 0 < len(files) <= MAX_FILES:
                raise PeerError("bad offer")
            from server import safe_name
            files = [{"name": safe_name(str(f.get("name", ""))), "size": int(f.get("size", -1))} for f in files]
            if any(f["size"] < 0 for f in files):
                raise PeerError("bad offer")
            total = sum(f["size"] for f in files)
            info.update(files=[dict(f) for f in files], total=total, note=clean_name(offer.get("from_name", "")))
            self.transfers[tid] = info
            self.contacts.seen(sender)
            folder = self.save_dir()
            if shutil.disk_usage(folder).free < total + HEADROOM:
                await _send_frame(w, {"t": "answer", "accept": False, "reason": "Not enough free space on their PC."})
                info.update(status="declined", error="Not enough free space on this PC.")
                return
            self.progress_hook(dict(info))
            decision = self.ask(dict(info))
            accepted = await self.loop.run_in_executor(None, lambda: decision.wait(ASK_SECONDS) and decision.accepted)
            if not accepted:
                await _send_frame(w, {"t": "answer", "accept": False, "reason": "They declined." if decision.wait(0) else "No answer in time."})
                info.update(status="declined")
                return
            await _send_frame(w, {"t": "answer", "accept": True})
            info["status"] = "receiving"
            self.progress_hook(dict(info))
            saved = []
            for f in files:
                tmp = folder / f".peer-{tid}-{len(saved)}.part"
                tmp_paths.append(tmp)
                h, got, last = hashlib.sha256(), 0, 0.0
                with open(tmp, "wb") as out:
                    while got < f["size"]:
                        chunk = await r.read(min(CHUNK, f["size"] - got))
                        if not chunk:
                            raise PeerError("The sender stopped.")
                        out.write(chunk); h.update(chunk); got += len(chunk); info["done"] += len(chunk)
                        if time.monotonic() - last > 0.25:
                            last = time.monotonic(); self.progress_hook(dict(info))
                end = await _read_frame(r)
                if end.get("t") != "file" or end.get("sha256") != h.hexdigest():
                    raise PeerError(f"{f['name']} arrived damaged, so it was thrown away.")
                final = self.place(tmp, f["name"])
                tmp_paths.remove(tmp)
                from share import mark_from_internet
                mark_from_internet(final)
                saved.append(final.name)
                self.received_hook(final, f["size"], contact)
            await _send_frame(w, {"t": "result", "saved": saved})
            await w.finish()
            info["status"] = "done"
            await asyncio.sleep(0.3)
        except Exception as e:
            info["status"], info["error"] = "failed", (str(e) if isinstance(e, PeerError) else "The connection was lost.")
        finally:
            for p in tmp_paths:
                try:
                    p.unlink(missing_ok=True)
                except OSError:
                    pass
            if tid in self.transfers:
                self.progress_hook(dict(info))
            try:
                conn.close(0, b"")
            except Exception:
                pass

    # -- sending (call from any thread; blocks until done)
    def send(self, contact_id, paths, cancelled=lambda: False, progress=None):
        """Send files to a contact. progress(info) gets live updates for this transfer. Returns the saved names."""
        if not self.ready.is_set() or self.error:
            raise PeerError(self.error or "Sending to contacts isn't running.")
        fut = asyncio.run_coroutine_threadsafe(self._send(contact_id, [Path(p) for p in paths], cancelled, progress), self.loop)
        return fut.result()

    async def _send(self, contact_id, paths, cancelled, progress=None):
        def report(info):
            self.progress_hook(dict(info))
            if progress:
                progress(dict(info))
        iroh = self.iroh
        contact = self.contacts.get(contact_id)
        if not contact:
            raise PeerError("That person isn't in your contacts.")
        files = [p for p in paths if p.is_file()]
        if not files or len(files) > MAX_FILES:
            raise PeerError("Pick between 1 and 1000 files.")
        tid = secrets.token_hex(6)
        info = {"id": tid, "dir": "out", "contact": contact["name"], "contact_id": contact_id, "status": "connecting",
                "files": [{"name": p.name, "size": p.stat().st_size} for p in files], "done": 0, "error": None}
        info["total"] = sum(f["size"] for f in info["files"])
        self.transfers[tid] = info
        report(info)
        conn = None
        try:
            addr = iroh.EndpointAddr(iroh.EndpointId.from_string(contact_id), None, [])
            try:
                conn = await asyncio.wait_for(self.endpoint.connect(addr, ALPN), 25)
            except Exception:
                raise PeerError(f"Couldn't reach {contact['name']}. Their PC needs to be on with Swiftlane open.")
            if str(conn.remote_id()) != contact_id:  # iroh guarantees this; checked anyway
                raise PeerError("Connected to the wrong PC.")
            self.contacts.seen(contact_id)
            bi = await conn.open_bi()
            w, r = bi.send(), bi.recv()
            await _send_frame(w, {"t": "offer", "v": 1, "from_name": clean_name(self.my_name()), "files": info["files"]})
            info["status"] = "waiting"
            report(info)
            answer = await asyncio.wait_for(_read_frame(r), ASK_SECONDS + 15)
            if not answer.get("accept"):
                info["status"] = "declined"
                raise PeerError(answer.get("reason") or f"{contact['name']} declined.")
            info["status"] = "sending"
            last = 0.0
            for p in files:
                h = hashlib.sha256()
                with open(p, "rb") as f:
                    while True:
                        if cancelled():
                            raise PeerError("Cancelled.")
                        chunk = f.read(CHUNK)
                        if not chunk:
                            break
                        await w.write_all(chunk); h.update(chunk); info["done"] += len(chunk)
                        if time.monotonic() - last > 0.25:
                            last = time.monotonic(); report(info)
                await _send_frame(w, {"t": "file", "sha256": h.hexdigest()})
            await w.finish()
            result = await asyncio.wait_for(_read_frame(r), 60)
            info["status"] = "done"
            return result.get("saved", [])
        except PeerError as e:
            if info["status"] != "declined":
                info["status"] = "failed"
            info["error"] = str(e)
            raise
        except Exception:
            info.update(status="failed", error="The connection was lost.")
            raise PeerError(f"The connection to {contact['name']} was lost.")
        finally:
            report(info)
            if conn:
                try:
                    conn.close(0, b"")
                except Exception:
                    pass


# ---------------------------------------------------------------- the identity key on disk
def load_or_create_key(path, protect, unprotect):
    """32-byte identity key, stored locked to this Windows account (DPAPI)."""
    p = Path(path)
    if p.exists():
        key = unprotect(p.read_bytes())
        if len(key) == 32:
            return key
    import iroh
    key = iroh.SecretKey.generate().to_bytes()
    tmp = p.with_name(p.name + ".tmp")
    tmp.write_bytes(protect(key))
    tmp.replace(p)
    return key


class Decision:
    """What Peer.ask returns: the window sets it when the user clicks Accept or Decline."""

    def __init__(self):
        self._event = threading.Event()
        self.accepted = False

    def answer(self, accepted):
        self.accepted = bool(accepted)
        self._event.set()

    def wait(self, timeout=None):
        return self._event.wait(timeout)
