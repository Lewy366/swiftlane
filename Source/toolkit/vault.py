"""Encrypted vault: a folder of files only your password opens.

Layout of a vault folder:
    vault.json      header: scrypt settings + salt, and the master key wrapped with the password key
    index.bin       encrypted list of files (names, sizes, dates)
    files/<id>.bin  each file, encrypted in 1 MiB chunks

A random 256-bit master key encrypts everything; the password only unwraps it, so changing the
password never re-encrypts files. Every chunk is AES-256-GCM with the file id, chunk number and an
"last chunk" flag bound in, so a changed, swapped, reordered or cut-off file fails to open instead of
giving back damaged data. There is no recovery: without the password the files stay locked.
"""
import hashlib, json, os, re, secrets, shutil, struct, tempfile, time
from pathlib import Path

from cryptography.exceptions import InvalidTag
from cryptography.hazmat.primitives.ciphers.aead import AESGCM

from .common import Cancelled

FORMAT = "swiftlane-vault"
VERSION = 1
CHUNK = 1 << 20
MAGIC = b"SLV1"
SCRYPT = {"n": 1 << 17, "r": 8, "p": 1}
KEY_AAD = b"swiftlane-vault/key/v1"
INDEX_AAD = b"swiftlane-vault/index/v1"
MIN_PASSWORD = 8
TEMP_ROOT = Path(tempfile.gettempdir()) / "Swiftlane vault"


class WrongPassword(Exception):
    pass


class VaultError(Exception):
    pass


def _derive(password, kdf):
    return hashlib.scrypt(password.encode("utf-8"), salt=bytes.fromhex(kdf["salt"]), n=kdf["n"], r=kdf["r"], p=kdf["p"],
                          maxmem=256 * 1024 * 1024, dklen=32)


def _wrap(password, master):
    kdf = {"name": "scrypt", **SCRYPT, "salt": secrets.token_hex(16)}
    nonce = secrets.token_bytes(12)
    return kdf, (nonce + AESGCM(_derive(password, kdf)).encrypt(nonce, master, KEY_AAD)).hex()


def _write_atomic(path, data):
    tmp = path.with_name(path.name + ".tmp")
    with open(tmp, "wb") as f:
        f.write(data)
        f.flush()
        os.fsync(f.fileno())
    os.replace(tmp, path)


def check_password(password):
    """None if the password is acceptable, otherwise why not."""
    if len(password) < MIN_PASSWORD:
        return f"Use at least {MIN_PASSWORD} characters."
    if len(set(password)) < 4:
        return "That password is too easy to guess."
    return None


def safe_name(name):
    name = re.sub(r'[<>:"/\\|?*\x00-\x1f]', "_", Path(str(name)).name).strip(" .")
    return name[:200] or "file"


def unique_path(folder, name):
    p = Path(folder) / name
    stem, suffix, n = p.stem, p.suffix, 2
    while p.exists():
        p = Path(folder) / f"{stem} ({n}){suffix}"
        n += 1
    return p


def shred(path):
    """Overwrite a file once with random bytes, then delete it. On SSDs and synced folders (OneDrive)
    old copies may survive elsewhere, so this lowers the risk rather than guaranteeing it."""
    p = Path(path)
    size = p.stat().st_size
    with open(p, "r+b", buffering=0) as f:
        left = size
        while left > 0:
            n = min(CHUNK, left)
            f.write(secrets.token_bytes(n))
            left -= n
        f.flush()
        os.fsync(f.fileno())
    p.unlink()


class Vault:
    def __init__(self, folder):
        self.folder = Path(folder)
        self._key = None
        self._index = None
        self._temp = None

    # ---------------------------------------------------------------- state
    @property
    def header_path(self):
        return self.folder / "vault.json"

    def exists(self):
        return self.header_path.is_file()

    @property
    def unlocked(self):
        return self._key is not None

    def _header(self):
        try:
            h = json.loads(self.header_path.read_text("utf-8"))
        except (OSError, ValueError):
            raise VaultError("This folder doesn't hold a Swiftlane vault.")
        if h.get("format") != FORMAT or h.get("version") != VERSION:
            raise VaultError("This vault was made by a different version of Swiftlane.")
        kdf = h.get("kdf") or {}
        # Refuse settings that are weaker than ours (someone editing the header to speed up guessing) or absurdly big.
        if kdf.get("name") != "scrypt" or not (SCRYPT["n"] <= kdf.get("n", 0) <= 1 << 20) or kdf.get("r") != 8 or kdf.get("p") != 1 \
                or len(kdf.get("salt", "")) != 32:
            raise VaultError("The vault's settings look damaged.")
        return h

    # ---------------------------------------------------------------- open / close
    def create(self, password):
        if self.exists():
            raise VaultError("There is already a vault in this folder.")
        why = check_password(password)
        if why:
            raise VaultError(why)
        (self.folder / "files").mkdir(parents=True, exist_ok=True)
        master = secrets.token_bytes(32)
        kdf, wrapped = _wrap(password, master)
        _write_atomic(self.header_path, json.dumps({"format": FORMAT, "version": VERSION, "kdf": kdf, "key": wrapped,
                                                    "created": int(time.time())}, indent=1).encode())
        self._key, self._index = master, {}
        self._save_index()

    def unlock(self, password):
        h = self._header()
        blob = bytes.fromhex(h["key"])
        try:
            master = AESGCM(_derive(password, h["kdf"])).decrypt(blob[:12], blob[12:], KEY_AAD)
        except InvalidTag:
            raise WrongPassword("Wrong password.")
        self._key = master
        self._index = self._load_index()

    def lock(self):
        self._key = None
        self._index = None
        self.wipe_temp()

    def change_password(self, old, new):
        why = check_password(new)
        if why:
            raise VaultError(why)
        h = self._header()
        blob = bytes.fromhex(h["key"])
        try:
            master = AESGCM(_derive(old, h["kdf"])).decrypt(blob[:12], blob[12:], KEY_AAD)
        except InvalidTag:
            raise WrongPassword("The current password is wrong.")
        h["kdf"], h["key"] = _wrap(new, master)
        _write_atomic(self.header_path, json.dumps(h, indent=1).encode())

    def _need_key(self):
        if self._key is None:
            raise VaultError("The vault is locked.")
        return AESGCM(self._key)

    # ---------------------------------------------------------------- index
    def _load_index(self):
        path = self.folder / "index.bin"
        if not path.exists():
            return {}
        blob = path.read_bytes()
        try:
            return json.loads(self._need_key().decrypt(blob[:12], blob[12:], INDEX_AAD))
        except (InvalidTag, ValueError):
            raise VaultError("The vault's file list is damaged.")

    def _save_index(self):
        nonce = secrets.token_bytes(12)
        data = json.dumps(self._index).encode()
        _write_atomic(self.folder / "index.bin", nonce + self._need_key().encrypt(nonce, data, INDEX_AAD))

    def items(self):
        self._need_key()
        return sorted(({"id": i, **v} for i, v in self._index.items()), key=lambda x: -x["added"])

    # ---------------------------------------------------------------- files
    def _blob(self, file_id):
        if not re.fullmatch(r"[0-9a-f]{32}", file_id):
            raise VaultError("Unknown file.")
        return self.folder / "files" / f"{file_id}.bin"

    @staticmethod
    def _aad(file_id, n, last):
        return file_id.encode() + struct.pack(">IB", n, 1 if last else 0)

    def add(self, paths, progress=None, cancelled=None, shred_originals=False):
        aes = self._need_key()
        sources = [Path(p) for p in paths if Path(p).is_file()]
        total = sum(p.stat().st_size for p in sources) or 1
        done, added = 0, []
        for src in sources:
            file_id = secrets.token_hex(16)
            prefix = secrets.token_bytes(8)
            out = self._blob(file_id)
            tmp = out.with_name(out.name + ".tmp")
            try:
                with open(src, "rb") as f, open(tmp, "wb") as o:
                    o.write(MAGIC + prefix)
                    n, chunk = 0, f.read(CHUNK)
                    while True:
                        if cancelled and cancelled():
                            raise Cancelled()
                        nxt = f.read(CHUNK)
                        last = not nxt
                        o.write(aes.encrypt(prefix + struct.pack(">I", n), chunk, self._aad(file_id, n, last)))
                        done += len(chunk)
                        if progress:
                            progress(done / total)
                        if last:
                            break
                        chunk, n = nxt, n + 1
                    o.flush()
                    os.fsync(o.fileno())
                os.replace(tmp, out)
            except BaseException:
                tmp.unlink(missing_ok=True)
                raise
            self._index[file_id] = {"name": safe_name(src.name), "size": src.stat().st_size, "added": int(time.time())}
            self._save_index()
            added.append(file_id)
            if shred_originals:
                shred(src)
        return added

    def _decrypt_to(self, file_id, dest, cancelled=None):
        aes = self._need_key()
        if file_id not in self._index:
            raise VaultError("Unknown file.")
        tmp = Path(dest).with_name(Path(dest).name + ".part")
        try:
            with open(self._blob(file_id), "rb") as f, open(tmp, "wb") as o:
                head = f.read(12)
                if head[:4] != MAGIC:
                    raise VaultError("This file in the vault is damaged.")
                prefix, n = head[4:], 0
                while True:
                    if cancelled and cancelled():
                        raise Cancelled()
                    block = f.read(CHUNK + 16)
                    if not block:
                        raise VaultError("This file in the vault is cut off.")
                    last = not f.peek(1) if hasattr(f, "peek") else f.tell() == os.fstat(f.fileno()).st_size
                    try:
                        o.write(aes.decrypt(prefix + struct.pack(">I", n), block, self._aad(file_id, n, last)))
                    except InvalidTag:
                        raise VaultError("This file in the vault was changed or damaged, so it won't be opened.")
                    if last:
                        break
                    n += 1
            os.replace(tmp, dest)
        except BaseException:
            tmp.unlink(missing_ok=True)
            raise
        return Path(dest)

    def export(self, ids, folder, cancelled=None):
        return [self._decrypt_to(i, unique_path(folder, self._index[i]["name"]), cancelled) for i in ids if i in self._index]

    def open_copy(self, file_id):
        """Decrypt one file into a private temp folder (wiped when the vault locks) and return its path."""
        if file_id not in self._need_key_index():
            raise VaultError("Unknown file.")
        if self._temp is None:
            TEMP_ROOT.mkdir(parents=True, exist_ok=True)
            self._temp = Path(tempfile.mkdtemp(dir=TEMP_ROOT))
        return self._decrypt_to(file_id, unique_path(self._temp, self._index[file_id]["name"]))

    def _need_key_index(self):
        self._need_key()
        return self._index

    def remove(self, ids):
        index = self._need_key_index()
        for i in ids:
            if i in index:
                del index[i]
                self._save_index()
                self._blob(i).unlink(missing_ok=True)

    def rename(self, file_id, name):
        index = self._need_key_index()
        if file_id not in index:
            raise VaultError("Unknown file.")
        index[file_id]["name"] = safe_name(name)
        self._save_index()

    def wipe_temp(self):
        """Delete the decrypted copies. Files still open in another app can't be deleted yet; they go next time."""
        self._temp = None
        wipe_leftovers()


def wipe_leftovers():
    if not TEMP_ROOT.exists():
        return True
    for p in sorted(TEMP_ROOT.rglob("*"), key=lambda x: -len(x.parts)):
        try:
            if p.is_file():
                shred(p)
            else:
                p.rmdir()
        except OSError:
            pass
    try:
        TEMP_ROOT.rmdir()
    except OSError:
        return False
    return True
