"""Encrypted phone transfer: Swiftlane's own small certificate authority (CA).

- The CA is limited (X.509 name constraints) to private and Tailscale IP addresses and one
  made-up name, so even if it were stolen it could never vouch for a real website.
- Its private key is stored encrypted with Windows DPAPI (tied to this Windows account), so a
  copied file is useless on any other account or PC.
- The server certificate is made fresh at every start for this PC's current addresses; its key
  only ever exists in memory (written to a temporary file just long enough to load it).
- The phone trusts the CA once (Connect phone > secure connection), then every connection is
  HTTPS on the same port as before.
"""
import base64
import ctypes
import datetime
import hashlib
import ipaddress
import os
import secrets
import socket
import ssl
import sys
import tempfile
import uuid
from ctypes import wintypes
from pathlib import Path

from cryptography import x509
from cryptography.hazmat.primitives import hashes, serialization
from cryptography.hazmat.primitives.asymmetric import ec
from cryptography.x509.oid import ExtendedKeyUsageOID, NameOID

ALLOWED_NETWORKS = [ipaddress.ip_network(n) for n in
                    ("10.0.0.0/8", "172.16.0.0/12", "192.168.0.0/16", "100.64.0.0/10", "127.0.0.0/8")]
ONLY_NAME = "swiftlane.invalid"  # the only DNS name the CA may vouch for (it doesn't exist)


# ---------------------------------------------------------------- Windows DPAPI
class _Blob(ctypes.Structure):
    _fields_ = [("cbData", wintypes.DWORD), ("pbData", ctypes.POINTER(ctypes.c_char))]


def _dpapi(data, protect):
    if sys.platform != "win32":
        raise OSError("Windows only")
    buf = ctypes.create_string_buffer(data, len(data))
    src, out = _Blob(len(data), ctypes.cast(buf, ctypes.POINTER(ctypes.c_char))), _Blob()
    fn = ctypes.windll.crypt32.CryptProtectData if protect else ctypes.windll.crypt32.CryptUnprotectData
    if protect:
        ok = fn(ctypes.byref(src), "Swiftlane", None, None, None, 0x1, ctypes.byref(out))  # CRYPTPROTECT_UI_FORBIDDEN
    else:
        ok = fn(ctypes.byref(src), None, None, None, None, 0x1, ctypes.byref(out))
    if not ok:
        raise OSError("Windows couldn't unlock Swiftlane's certificate key")
    try:
        return ctypes.string_at(out.pbData, out.cbData)
    finally:
        ctypes.windll.kernel32.LocalFree(out.pbData)


# ---------------------------------------------------------------- the CA
class Authority:
    def __init__(self, folder):
        self.folder = Path(folder)
        self.cert_path = self.folder / "swiftlane-ca.pem"
        self.key_path = self.folder / "swiftlane-ca.key.dpapi"
        self.cert, self.key = self._load() or self._create()

    def _load(self):
        try:
            cert = x509.load_pem_x509_certificate(self.cert_path.read_bytes())
            key = serialization.load_der_private_key(_dpapi(self.key_path.read_bytes(), False), None)
        except (OSError, ValueError):
            return None
        if cert.not_valid_after_utc < datetime.datetime.now(datetime.timezone.utc) + datetime.timedelta(days=30):
            return None
        return cert, key

    def _create(self):
        key = ec.generate_private_key(ec.SECP256R1())
        name = x509.Name([x509.NameAttribute(NameOID.COMMON_NAME, f"Swiftlane on {socket.gethostname()}"[:64]),
                          x509.NameAttribute(NameOID.ORGANIZATION_NAME, "Swiftlane (this PC only)")])
        now = datetime.datetime.now(datetime.timezone.utc)
        permitted = [x509.IPAddress(n) for n in ALLOWED_NETWORKS] + [x509.DNSName(ONLY_NAME)]
        cert = (x509.CertificateBuilder().subject_name(name).issuer_name(name).public_key(key.public_key())
                .serial_number(x509.random_serial_number())
                .not_valid_before(now - datetime.timedelta(minutes=5)).not_valid_after(now + datetime.timedelta(days=3650))
                .add_extension(x509.BasicConstraints(ca=True, path_length=0), critical=True)
                .add_extension(x509.KeyUsage(False, False, False, False, False, True, True, False, False), critical=True)
                .add_extension(x509.NameConstraints(permitted_subtrees=permitted, excluded_subtrees=None), critical=True)
                .add_extension(x509.SubjectKeyIdentifier.from_public_key(key.public_key()), critical=False)
                .sign(key, hashes.SHA256()))
        self.folder.mkdir(parents=True, exist_ok=True)
        der = key.private_bytes(serialization.Encoding.DER, serialization.PrivateFormat.PKCS8, serialization.NoEncryption())
        self.key_path.write_bytes(_dpapi(der, True))
        self.cert_path.write_bytes(cert.public_bytes(serialization.Encoding.PEM))
        return cert, key

    @property
    def der(self):
        return self.cert.public_bytes(serialization.Encoding.DER)

    @property
    def fingerprint(self):
        h = hashlib.sha256(self.der).hexdigest().upper()
        return " ".join(h[i:i + 4] for i in range(0, 24, 4))  # first 96 bits, grouped, to compare by eye

    def mobileconfig(self):
        """iPhone profile that installs the CA (the user then turns on full trust in Settings)."""
        ident = "local.swiftlane.ca." + hashlib.sha256(self.der).hexdigest()[:16]
        return f"""<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>PayloadContent</key><array><dict>
<key>PayloadCertificateFileName</key><string>Swiftlane.cer</string>
<key>PayloadContent</key><data>{base64.b64encode(self.der).decode()}</data>
<key>PayloadDescription</key><string>Lets this phone talk to Swiftlane on your PC over an encrypted connection. It can only vouch for home-network addresses, never for websites.</string>
<key>PayloadDisplayName</key><string>Swiftlane (your PC)</string>
<key>PayloadIdentifier</key><string>{ident}.cert</string>
<key>PayloadType</key><string>com.apple.security.root</string>
<key>PayloadUUID</key><string>{uuid.uuid5(uuid.NAMESPACE_URL, ident + '.cert')}</string>
<key>PayloadVersion</key><integer>1</integer>
</dict></array>
<key>PayloadDisplayName</key><string>Swiftlane: encrypted connection to your PC</string>
<key>PayloadIdentifier</key><string>{ident}</string>
<key>PayloadRemovalDisallowed</key><false/>
<key>PayloadType</key><string>Configuration</string>
<key>PayloadUUID</key><string>{uuid.uuid5(uuid.NAMESPACE_URL, ident)}</string>
<key>PayloadVersion</key><integer>1</integer>
</dict></plist>""".encode()

    def server_context(self, addresses):
        """TLS context with a fresh certificate for these IP addresses (private/Tailscale ones only)."""
        ips = sorted({ipaddress.ip_address(a) for a in [*addresses, "127.0.0.1"]
                      if any(ipaddress.ip_address(a) in n for n in ALLOWED_NETWORKS)}, key=str)
        key = ec.generate_private_key(ec.SECP256R1())
        now = datetime.datetime.now(datetime.timezone.utc)
        cert = (x509.CertificateBuilder()
                .subject_name(x509.Name([x509.NameAttribute(NameOID.COMMON_NAME, ONLY_NAME)]))
                .issuer_name(self.cert.subject).public_key(key.public_key()).serial_number(x509.random_serial_number())
                .not_valid_before(now - datetime.timedelta(minutes=5)).not_valid_after(now + datetime.timedelta(days=397))
                .add_extension(x509.SubjectAlternativeName([x509.DNSName(ONLY_NAME), *map(x509.IPAddress, ips)]), critical=False)
                .add_extension(x509.BasicConstraints(ca=False, path_length=None), critical=True)
                .add_extension(x509.KeyUsage(True, False, False, False, False, False, False, False, False), critical=True)
                .add_extension(x509.ExtendedKeyUsage([ExtendedKeyUsageOID.SERVER_AUTH]), critical=False)
                .add_extension(x509.AuthorityKeyIdentifier.from_issuer_public_key(self.key.public_key()), critical=False)
                .sign(self.key, hashes.SHA256()))
        ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        ctx.minimum_version = ssl.TLSVersion.TLSv1_2
        fd, tmp = tempfile.mkstemp(prefix="swl-", suffix=".pem")
        try:  # ssl only loads keys from a file: it exists for a moment, readable only by this account
            with os.fdopen(fd, "wb") as f:
                f.write(key.private_bytes(serialization.Encoding.PEM, serialization.PrivateFormat.PKCS8, serialization.NoEncryption()))
                f.write(cert.public_bytes(serialization.Encoding.PEM))
                f.write(self.cert.public_bytes(serialization.Encoding.PEM))
            ctx.load_cert_chain(tmp)
        finally:
            os.remove(tmp)
        return ctx
