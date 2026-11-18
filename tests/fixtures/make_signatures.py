#!/usr/bin/env python3
"""Signing helpers for the fixture APKs.

Produces real X.509 certificates, a real PKCS#7 SignedData blob for v1 jar
signing, and a structurally correct APK Signing Block carrying v2 and v3
scheme blocks.

The *signatures* inside are not cryptographically meaningful, and do not need
to be: Cookie never verifies a signature, it only reads the signer's
certificate to decide whether two APKs came from the same key. Android does
the real verification at install time. What matters here is that the
certificates and the container structure are genuine, so the parser is tested
against the real formats.
"""

import datetime
import struct

from cryptography import x509
from cryptography.hazmat.primitives import hashes, serialization
from cryptography.hazmat.primitives.asymmetric import rsa
from cryptography.hazmat.primitives.serialization import pkcs7
from cryptography.x509.oid import NameOID

APK_SIG_BLOCK_MAGIC = b"APK Sig Block 42"
V2_BLOCK_ID = 0x7109871A
V3_BLOCK_ID = 0xF05368C0

EOCD_SIGNATURE = b"PK\x05\x06"


def make_key_and_cert(common_name, seed_size=2048):
    """A self-signed certificate, as an app signing key would be."""
    key = rsa.generate_private_key(public_exponent=65537, key_size=seed_size)
    subject = issuer = x509.Name([
        x509.NameAttribute(NameOID.COMMON_NAME, common_name),
        x509.NameAttribute(NameOID.ORGANIZATION_NAME, "Cookie test fixtures"),
    ])
    not_before = datetime.datetime(2020, 1, 1, tzinfo=datetime.timezone.utc)
    cert = (
        x509.CertificateBuilder()
        .subject_name(subject)
        .issuer_name(issuer)
        .public_key(key.public_key())
        .serial_number(x509.random_serial_number())
        .not_valid_before(not_before)
        .not_valid_after(not_before + datetime.timedelta(days=365 * 30))
        .sign(key, hashes.SHA256())
    )
    return key, cert


def pkcs7_blob(key, cert, payload=b"cookie fixture"):
    """A DER PKCS#7 SignedData, as META-INF/CERT.RSA holds."""
    return (
        pkcs7.PKCS7SignatureBuilder()
        .set_data(payload)
        .add_signer(cert, key, hashes.SHA256())
        .sign(
            serialization.Encoding.DER,
            [pkcs7.PKCS7Options.DetachedSignature, pkcs7.PKCS7Options.NoAttributes],
        )
    )


def _u32_prefixed(payload):
    return struct.pack("<I", len(payload)) + payload


def _sequence(items):
    """A sequence of length-prefixed items."""
    return b"".join(_u32_prefixed(item) for item in items)


def scheme_block(cert, key):
    """A v2/v3 scheme block holding one signer with this certificate.

        signers
          signer
            signed data
              digests
              certificates
              additional attributes
            signatures
            public key
    """
    cert_der = cert.public_bytes(serialization.Encoding.DER)
    public_key = key.public_key().public_bytes(
        serialization.Encoding.DER,
        serialization.PublicFormat.SubjectPublicKeyInfo,
    )

    # One digest: algorithm id 0x0103 (RSASSA-PKCS1-v1_5 with SHA-256), then
    # a length-prefixed digest value.
    digest = _sequence([struct.pack("<I", 0x0103) + _u32_prefixed(b"\x00" * 32)])
    certificates = _sequence([cert_der])
    attributes = b""

    signed_data = (
        _u32_prefixed(digest)
        + _u32_prefixed(certificates)
        + _u32_prefixed(attributes)
    )
    signatures = _sequence([struct.pack("<I", 0x0103) + _u32_prefixed(b"\x00" * 256)])

    signer = (
        _u32_prefixed(signed_data)
        + _u32_prefixed(signatures)
        + _u32_prefixed(public_key)
    )
    return _u32_prefixed(_sequence([signer]))


def signing_block(pairs):
    """Wrap id-value pairs in the APK Signing Block container.

        u64 size (of everything after this field)
        id-value pairs
        u64 size (again)
        magic
    """
    payload = b"".join(
        struct.pack("<Q", 4 + len(value)) + struct.pack("<I", block_id) + value
        for block_id, value in pairs
    )
    size = len(payload) + 8 + len(APK_SIG_BLOCK_MAGIC)
    return struct.pack("<Q", size) + payload + struct.pack("<Q", size) + APK_SIG_BLOCK_MAGIC


def insert_signing_block(zip_bytes, block):
    """Splice a signing block into a finished zip.

    It goes between the last local file entry and the central directory, so
    only the EOCD's "offset of central directory" has to move; the central
    directory entries point at local headers, which do not.
    """
    eocd = zip_bytes.rfind(EOCD_SIGNATURE)
    if eocd < 0:
        raise ValueError("no end-of-central-directory record")

    cd_offset = struct.unpack_from("<I", zip_bytes, eocd + 16)[0]
    if cd_offset == 0xFFFFFFFF:
        raise ValueError("zip64 fixtures are not supported here")

    out = bytearray()
    out += zip_bytes[:cd_offset]
    out += block
    out += zip_bytes[cd_offset:]

    new_eocd = eocd + len(block)
    struct.pack_into("<I", out, new_eocd + 16, cd_offset + len(block))
    return bytes(out)


def sign(zip_bytes, key, cert, schemes=("v2", "v3")):
    """Add an APK Signing Block with the requested schemes."""
    pairs = []
    if "v2" in schemes:
        pairs.append((V2_BLOCK_ID, scheme_block(cert, key)))
    if "v3" in schemes:
        pairs.append((V3_BLOCK_ID, scheme_block(cert, key)))
    if not pairs:
        return zip_bytes
    return insert_signing_block(zip_bytes, signing_block(pairs))


def certificate_sha256(cert):
    """The value Cookie records: SHA-256 over the certificate's DER."""
    import hashlib

    return hashlib.sha256(cert.public_bytes(serialization.Encoding.DER)).hexdigest()
