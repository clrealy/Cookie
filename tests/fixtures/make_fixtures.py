#!/usr/bin/env python3
"""Generate the synthetic APKs the integration tests run against.

These are structurally real APKs: a zip containing a genuine Android binary
XML AndroidManifest.xml, native library directories, and a launcher icon.
They contain no actual code, so there is nothing to run and nothing to
redistribute.

Written independently of Cookie's own AXML code, so the integration tests
check the parser against a second implementation rather than itself.

Usage:  python3 tests/fixtures/make_fixtures.py
"""

import io
import json
import struct
import sys
import zipfile
import zlib
import os

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import make_signatures as sig

ANDROID_NS = "http://schemas.android.com/apk/res/android"
NO_STRING = 0xFFFFFFFF

TYPE_REFERENCE = 0x01
TYPE_STRING = 0x03
TYPE_INT_DEC = 0x10


class Axml:
    def __init__(self):
        self.strings = []
        self.body = bytearray()

    def intern(self, s):
        if s in self.strings:
            return self.strings.index(s)
        self.strings.append(s)
        return len(self.strings) - 1

    def start(self, name, attrs=()):
        name_idx = self.intern(name)
        emits = []
        for android, attr, kind, value in attrs:
            ns = self.intern(ANDROID_NS) if android else NO_STRING
            n = self.intern(attr)
            if kind == "s":
                si = self.intern(value)
                emits.append((ns, n, si, TYPE_STRING, si))
            elif kind == "i":
                emits.append((ns, n, NO_STRING, TYPE_INT_DEC, value & 0xFFFFFFFF))
            elif kind == "r":
                emits.append((ns, n, NO_STRING, TYPE_REFERENCE, value))
            else:
                raise ValueError(kind)

        size = 16 + 20 + len(emits) * 20
        c = bytearray()
        c += struct.pack("<HHI", 0x0102, 16, size)
        c += struct.pack("<II", 1, NO_STRING)          # line, comment
        c += struct.pack("<II", NO_STRING, name_idx)   # ns, name
        c += struct.pack("<HHHHHH", 20, 20, len(emits), 0, 0, 0)
        for ns, n, raw, dtype, payload in emits:
            c += struct.pack("<III", ns, n, raw)
            c += struct.pack("<HBBI", 8, 0, dtype, payload)
        self.body += c

    def end(self, name):
        name_idx = self.intern(name)
        c = bytearray()
        c += struct.pack("<HHI", 0x0103, 16, 24)
        c += struct.pack("<II", 1, NO_STRING)
        c += struct.pack("<II", NO_STRING, name_idx)
        self.body += c

    def finish(self):
        data = bytearray()
        offsets = []
        for s in self.strings:
            offsets.append(len(data))
            units = s.encode("utf-16-le")
            data += struct.pack("<H", len(s))
            data += units
            data += b"\x00\x00"
        while len(data) % 4:
            data += b"\x00"

        header = 28
        strings_start = header + len(offsets) * 4
        pool_size = strings_start + len(data)
        pool = bytearray()
        pool += struct.pack("<HHI", 0x0001, header, pool_size)
        pool += struct.pack("<III", len(self.strings), 0, 0)   # counts, flags (UTF-16)
        pool += struct.pack("<II", strings_start, 0)
        for o in offsets:
            pool += struct.pack("<I", o)
        pool += data

        total = 8 + len(pool) + len(self.body)
        out = bytearray()
        out += struct.pack("<HHI", 0x0003, 8, total)
        out += pool
        out += self.body
        return bytes(out)


def base_manifest(package, version_code, version_name, label):
    a = Axml()
    a.start("manifest", [
        (False, "package", "s", package),
        (True, "versionCode", "i", version_code),
        (True, "versionName", "s", version_name),
    ])
    a.start("uses-sdk", [
        (True, "minSdkVersion", "i", 24),
        (True, "targetSdkVersion", "i", 34),
    ])
    a.end("uses-sdk")
    a.start("application", [
        (True, "label", "s", label),
        (True, "icon", "r", 0x7F080000),
    ])
    a.start("activity", [(True, "name", "s", ".MainActivity")])
    a.start("intent-filter")
    a.start("action", [(True, "name", "s", "android.intent.action.MAIN")])
    a.end("action")
    a.start("category", [(True, "name", "s", "android.intent.category.LAUNCHER")])
    a.end("category")
    a.end("intent-filter")
    a.end("activity")
    a.end("application")
    a.end("manifest")
    return a.finish()


def split_manifest(package, split, version_code):
    a = Axml()
    a.start("manifest", [
        (False, "package", "s", package),
        (False, "split", "s", split),
        (True, "versionCode", "i", version_code),
    ])
    a.end("manifest")
    return a.finish()


def png(size, rgb):
    """A tiny solid-colour PNG, so the icon picker has something real to find."""
    raw = bytearray()
    for _ in range(size):
        raw.append(0)
        raw += bytes(rgb) * size

    def chunk(tag, data):
        return (struct.pack(">I", len(data)) + tag + data +
                struct.pack(">I", zlib.crc32(tag + data) & 0xFFFFFFFF))

    return (b"\x89PNG\r\n\x1a\n"
            + chunk(b"IHDR", struct.pack(">IIBBBBB", size, size, 8, 2, 0, 0, 0))
            + chunk(b"IDAT", zlib.compress(bytes(raw), 9))
            + chunk(b"IEND", b""))


HERE = os.path.dirname(os.path.abspath(__file__))


def build_zip(entries):
    """A zip in memory, so a signing block can be spliced in afterwards."""
    buffer = io.BytesIO()
    with zipfile.ZipFile(buffer, "w", zipfile.ZIP_DEFLATED) as z:
        for entry, data in entries:
            z.writestr(entry, data)
    return buffer.getvalue()


def write(name, entries, signer=None, schemes=("v1", "v2", "v3")):
    """Write a fixture APK, optionally signed.

    `signer` is a (key, cert) pair. v1 adds a PKCS#7 blob under META-INF;
    v2 and v3 add an APK Signing Block after the file entries.
    """
    entries = list(entries)
    if signer is not None and "v1" in schemes:
        key, cert = signer
        entries.append(("META-INF/CERT.RSA", sig.pkcs7_blob(key, cert)))
        entries.append(("META-INF/CERT.SF", b"Signature-Version: 1.0\n"))
        entries.append(("META-INF/MANIFEST.MF", b"Manifest-Version: 1.0\n"))

    data = build_zip(entries)

    if signer is not None:
        key, cert = signer
        block_schemes = tuple(s for s in schemes if s in ("v2", "v3"))
        if block_schemes:
            data = sig.sign(data, key, cert, block_schemes)

    path = os.path.join(HERE, name)
    with open(path, "wb") as f:
        f.write(data)
    print(f"{name}: {os.path.getsize(path)} bytes")


# Two signing keys: the app's own, and someone else's.
teapot_signer = sig.make_key_and_cert("Teapot Test Key")
other_signer = sig.make_key_and_cert("Someone Else Test Key")

teapot_entries = [
    ("AndroidManifest.xml", base_manifest("com.example.teapot", 4210, "2.1.0", "Teapot")),
    ("lib/arm64-v8a/libteapot.so", b"\x7fELF" + b"\x00" * 64),
    ("res/mipmap-xxxhdpi/ic_launcher.png", png(16, (210, 140, 60))),
    ("res/mipmap-mdpi/ic_launcher.png", png(8, (210, 140, 60))),
    ("res/drawable-hdpi/background.png", png(8, (10, 10, 10))),
    ("classes.dex", b"dex\n035\x00" + b"\x00" * 32),
]


def teapot_entries_at(version_code, version_name):
    entries = list(teapot_entries)
    entries[0] = (
        "AndroidManifest.xml",
        base_manifest("com.example.teapot", version_code, version_name, "Teapot"),
    )
    return entries


# The app as first released: signed under every scheme, as a real app is.
write("teapot.apk", teapot_entries_at(4210, "2.1.0"), teapot_signer)

# A genuine update: same key, higher version.
write("teapot-v2.apk", teapot_entries_at(4300, "2.2.0"), teapot_signer)

# An older build, for the downgrade path.
write("teapot-old.apk", teapot_entries_at(4100, "2.0.0"), teapot_signer)

# The dangerous one: same package id, different key.
write("teapot-resigned.apk", teapot_entries_at(4300, "2.2.0"), other_signer)

# Signed only under v2: no META-INF at all, as a modern app may be.
write("teapot-v2only.apk", teapot_entries_at(4300, "2.2.0"), teapot_signer, schemes=("v2",))

# Signed only under v1, the legacy jar scheme.
write("teapot-v1only.apk", teapot_entries_at(4300, "2.2.0"), teapot_signer, schemes=("v1",))

# The matching arm64 split.
write("teapot-split-arm64.apk", [
    ("AndroidManifest.xml", split_manifest("com.example.teapot", "config.arm64_v8a", 4210)),
    ("lib/arm64-v8a/libextra.so", b"\x7fELF" + b"\x00" * 32),
], teapot_signer)

# A split that belongs to a different app, for the mismatch test.
write("kettle-split.apk", [
    ("AndroidManifest.xml", split_manifest("com.example.kettle", "config.arm64_v8a", 1)),
])

# No native code, no icon, no signature at all.
write("plain.apk", [
    ("AndroidManifest.xml", base_manifest("com.example.plain", 1, "1.0", "Plain")),
    ("classes.dex", b"dex\n035\x00" + b"\x00" * 16),
])

# A bundle holding a base plus its split.
bundle_path = os.path.join(HERE, "teapot.xapk")
with zipfile.ZipFile(bundle_path, "w", zipfile.ZIP_DEFLATED) as z:
    for src, dest in [("teapot.apk", "com.example.teapot.apk"),
                      ("teapot-split-arm64.apk", "config.arm64_v8a.apk")]:
        with open(os.path.join(HERE, src), "rb") as f:
            z.writestr(dest, f.read())
    z.writestr("manifest.json", '{"package_name": "com.example.teapot"}')
    z.writestr("icon.png", png(16, (210, 140, 60)))
print(f"teapot.xapk: {os.path.getsize(bundle_path)} bytes")

# Record the certificate digests, computed here with hashlib, so the Rust
# tests check Cookie's reader against an independent implementation rather
# than against itself.
expected = {
    "teapot": sig.certificate_sha256(teapot_signer[1]),
    "other": sig.certificate_sha256(other_signer[1]),
}
with open(os.path.join(HERE, "signatures.json"), "w") as f:
    json.dump(expected, f, indent=2, sort_keys=True)
    f.write("\n")
print("signatures.json:", expected)
