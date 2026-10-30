"""Extraction service: document bytes in, Ed25519-signed evidence out.

    python -m extractor.service [--port 8090] [--key-file secrets/extractor_ed25519.pem | --ephemeral]
                                [--pubkey-out PATH]

Runs as its own process (and in production its own OS user/container) so the agent process
never holds the signing key. HTTP API (127.0.0.1 only):
  GET  /health
  GET  /v1/pubkey                 -> {"public_key_hex", "key_id", "extractor_version"}
  POST /v1/extract                {"document_b64": str, "cover_email": {...}}  -> envelope
Optional EXTRACTOR_API_KEY env: bearer token required on /v1/extract.

Each response is freshly signed (new nonce, issued_at = now) even when the extraction result
itself is served from the in-memory cache keyed by (document sha256, cover-email sha256).
A document that fails the sandbox is still signed, with status "failed", so the governor can
bind the failure to the document hash and answer REVIEW.
"""
from __future__ import annotations

import argparse
import base64
import binascii
import hashlib
import hmac
import json
import os
import secrets
import sys
import threading
import time
from collections import OrderedDict
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

from extractor import EXTRACTOR_VERSION
from extractor.sandbox import Limits, run_sandboxed
from extractor import signing

DEFAULT_BUYER_GSTIN = "24ZZACB7319B1Z5"


class Extractor:
    def __init__(self, sk=None, *, buyer_gstin: str = DEFAULT_BUYER_GSTIN, limits: Limits = Limits(), cache_size: int = 512):
        self._sk = sk or signing.generate()
        self.pk = self._sk.public_key()
        self.key_id = signing.key_id(self.pk)
        self.public_hex = signing.public_hex(self.pk)
        self.buyer_gstin = buyer_gstin
        self.limits = limits
        self._cache: OrderedDict[tuple[str, str], tuple[str, dict | None, str | None, dict]] = OrderedDict()
        self._cache_size = cache_size
        self._lock = threading.Lock()

    @staticmethod
    def email_sha256(cover_email: dict | None) -> str:
        return hashlib.sha256(signing.canonical(cover_email or {})).hexdigest()

    def _extract(self, doc: bytes, cover_email: dict | None, doc_sha: str, email_sha: str):
        key = (doc_sha, email_sha)
        with self._lock:
            hit = self._cache.get(key)
            if hit:
                self._cache.move_to_end(key)
                return hit
        r = run_sandboxed(doc, cover_email, self.buyer_gstin, self.limits)
        sandbox = {"wall_s": r.info.get("wall_s"), "network_namespace": r.info.get("network_namespace")}
        val = ("ok" if r.ok else "failed", r.evidence, r.failure, sandbox)
        if r.ok or r.failure in ("file_too_large", "not_a_pdf") or (r.failure or "").startswith(("too_many_pages", "parse_error")):
            with self._lock:  # deterministic outcomes only; never cache a timeout/resource failure
                self._cache[key] = val
                while len(self._cache) > self._cache_size:
                    self._cache.popitem(last=False)
        return val

    def extract(self, doc: bytes, cover_email: dict | None = None) -> dict:
        doc_sha = hashlib.sha256(doc).hexdigest()
        email_sha = self.email_sha256(cover_email)
        status, evidence, failure, sandbox = self._extract(doc, cover_email, doc_sha, email_sha)
        payload = {
            "v": 1, "key_id": self.key_id, "doc_sha256": doc_sha, "email_sha256": email_sha,
            "extractor_version": EXTRACTOR_VERSION, "issued_at": int(time.time()), "nonce": secrets.token_hex(16),
            "status": status, "sandbox": sandbox,
        }
        if status == "ok":
            payload["evidence"] = evidence
        else:
            payload["failure"] = failure
        return signing.sign(self._sk, payload)


MAX_BODY = 16 * 1024 * 1024  # base64 of a 10 MB document plus headroom


def make_handler(ex: Extractor, api_key: str | None):
    class H(BaseHTTPRequestHandler):
        def log_message(self, *a):
            pass

        def _send(self, code: int, obj: dict):
            body = json.dumps(obj).encode()
            self.send_response(code)
            self.send_header("content-type", "application/json")
            self.send_header("content-length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def do_GET(self):
            if self.path == "/health":
                return self._send(200, {"ok": True})
            if self.path == "/v1/pubkey":
                return self._send(200, {"public_key_hex": ex.public_hex, "key_id": ex.key_id, "extractor_version": EXTRACTOR_VERSION})
            self._send(404, {"error": "not found"})

        def do_POST(self):
            if self.path != "/v1/extract":
                return self._send(404, {"error": "not found"})
            if api_key:
                got = self.headers.get("authorization", "").removeprefix("Bearer ")
                if not hmac.compare_digest(got.encode(), api_key.encode()):
                    return self._send(401, {"error": "unauthorized"})
            try:
                n = int(self.headers.get("content-length", "0"))
            except ValueError:
                return self._send(400, {"error": "bad content-length"})
            if n <= 0 or n > MAX_BODY:
                return self._send(413, {"error": "body too large or empty"})
            try:
                req = json.loads(self.rfile.read(n))
                doc = base64.b64decode(req["document_b64"], validate=True)
                email = req.get("cover_email") or {}
                if not isinstance(email, dict):
                    raise ValueError("cover_email must be an object")
            except (ValueError, KeyError, TypeError, binascii.Error) as e:
                return self._send(400, {"error": f"bad request: {type(e).__name__}"})
            self._send(200, ex.extract(doc, email))

    return H


def main(argv=None) -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--port", type=int, default=int(os.environ.get("EXTRACTOR_PORT", "8090")))
    g = ap.add_mutually_exclusive_group()
    g.add_argument("--key-file", default=os.environ.get("EXTRACTOR_SIGNING_KEY_FILE"))
    g.add_argument("--ephemeral", action="store_true", help="in-memory key, gone when the process exits")
    ap.add_argument("--pubkey-out", default=None, help="write the public key (hex) here once listening")
    a = ap.parse_args(argv)
    if a.ephemeral or not a.key_file:
        sk = signing.generate()
    else:
        sk = signing.load_or_create_key(a.key_file)
    ex = Extractor(sk)
    srv = ThreadingHTTPServer(("127.0.0.1", a.port), make_handler(ex, os.environ.get("EXTRACTOR_API_KEY") or None))
    if a.pubkey_out:
        tmp = a.pubkey_out + ".tmp"
        with open(tmp, "w") as f:
            f.write(json.dumps({"public_key_hex": ex.public_hex, "key_id": ex.key_id, "port": srv.server_address[1]}))
        os.replace(tmp, a.pubkey_out)
    print(f"extractor listening on 127.0.0.1:{srv.server_address[1]} key_id={ex.key_id}", file=sys.stderr, flush=True)
    srv.serve_forever()
    return 0


if __name__ == "__main__":
    sys.exit(main())
