"""Ed25519 signing of extraction results over canonical JSON.

Canonical form: json.dumps(obj, sort_keys=True, separators=(",", ":"), ensure_ascii=False),
UTF-8 encoded. The governor verifies the signature over the exact payload bytes it receives,
so the payload travels as a string inside the envelope {"payload": str, "sig": base64}.

Keys are never committed: `load_or_create_key` writes a fresh key to a gitignored path
(mode 0600) or the service runs with an in-memory ephemeral key.
"""
from __future__ import annotations

import base64
import hashlib
import json
import os
from pathlib import Path

from cryptography.hazmat.primitives import serialization
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey, Ed25519PublicKey


def canonical(obj) -> bytes:
    return json.dumps(obj, sort_keys=True, separators=(",", ":"), ensure_ascii=False).encode("utf-8")


def raw_public(pk: Ed25519PublicKey) -> bytes:
    return pk.public_bytes(serialization.Encoding.Raw, serialization.PublicFormat.Raw)


def key_id(pk: Ed25519PublicKey) -> str:
    """First 16 hex chars of sha256(raw public key); must match governor trust::key_id."""
    return hashlib.sha256(raw_public(pk)).hexdigest()[:16]


def public_hex(pk: Ed25519PublicKey) -> str:
    return raw_public(pk).hex()


def sign(sk: Ed25519PrivateKey, payload: dict) -> dict:
    data = canonical(payload)
    return {"payload": data.decode("utf-8"), "sig": base64.b64encode(sk.sign(data)).decode("ascii")}


def generate() -> Ed25519PrivateKey:
    return Ed25519PrivateKey.generate()


def load_or_create_key(path: str | Path) -> Ed25519PrivateKey:
    """PEM (PKCS8) private key at `path`; created with mode 0600 if absent."""
    p = Path(path)
    if p.exists():
        key = serialization.load_pem_private_key(p.read_bytes(), password=None)
        if not isinstance(key, Ed25519PrivateKey):
            raise ValueError(f"{p} is not an Ed25519 private key")
        return key
    key = generate()
    p.parent.mkdir(parents=True, exist_ok=True)
    pem = key.private_bytes(serialization.Encoding.PEM, serialization.PrivateFormat.PKCS8, serialization.NoEncryption())
    fd = os.open(str(p), os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(fd, "wb") as f:
        f.write(pem)
    return key
