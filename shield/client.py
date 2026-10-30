"""Governor + extraction-service process management and HTTP clients for the shield-ON pipeline.

Trust boundary (docs/DECISIONS.md, "Evidence is signed by the extraction service"):
the extractor runs as its own process with its own (ephemeral, in-memory) Ed25519 key; this
process only ever sees its public key, which it hands to the governor. Evidence reaches the
governor only as the extractor's signed envelope, bound to the document's sha256.
"""
from __future__ import annotations

import hashlib
import json
import os
import secrets
import socket
import subprocess
import sys
import tempfile
import time
from pathlib import Path

import httpx

REPO = Path(__file__).resolve().parents[1]
BIN = Path(os.environ.get("GOVERNOR_BIN") or REPO / "governor" / "target" / "release" / "governor-server")
MASTER = REPO / "dataset" / "fixtures" / "master.json"


def _free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def _client(base: str, headers: dict | None = None, timeout: float = 10) -> httpx.Client:
    return httpx.Client(base_url=base, headers=headers or {}, timeout=timeout, trust_env=False)


class ExtractorProc:
    """Starts `python -m extractor.service --ephemeral` on a free local port.

    The signing key lives only in that child process's memory; we read back the public key.
    """

    def __init__(self):
        self.port = _free_port()
        self._tmp = tempfile.TemporaryDirectory(prefix="isx-svc-")
        pub = Path(self._tmp.name) / "pub.json"
        env = {k: v for k, v in os.environ.items() if not k.endswith(("_API_KEY", "_TOKEN"))}
        env["PYTHONPATH"] = os.pathsep.join([str(REPO), env.get("PYTHONPATH", "")])
        self.proc = subprocess.Popen([sys.executable, "-m", "extractor.service", "--ephemeral", "--port", str(self.port),
                                      "--pubkey-out", str(pub)], cwd=str(REPO), env=env, stderr=subprocess.DEVNULL)
        for _ in range(200):
            if pub.exists():
                break
            if self.proc.poll() is not None:
                raise RuntimeError("extractor exited during startup")
            time.sleep(0.05)
        else:
            self.close()
            raise RuntimeError("extractor did not start")
        info = json.loads(pub.read_text())
        self.public_key_hex, self.key_id = info["public_key_hex"], info["key_id"]
        self.http = _client(f"http://127.0.0.1:{self.port}", timeout=60)

    def extract(self, document: bytes, cover_email: dict | None) -> dict:
        import base64
        r = self.http.post("/v1/extract", json={"document_b64": base64.b64encode(document).decode(), "cover_email": cover_email or {}})
        r.raise_for_status()
        return r.json()

    def close(self):
        if self.proc.poll() is None:
            self.proc.terminate()
            try:
                self.proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self.proc.kill()
        self._tmp.cleanup()

    def __enter__(self):
        return self

    def __exit__(self, *a):
        self.close()


class GovernorProc:
    """Starts the Rust server on a free local port in eval mode (clock header + reset enabled).

    Default: strict provenance. An extraction service is started (or passed in) and the
    governor trusts only its public key. `dev_unsigned=True` is the DEV/TEST-ONLY old path
    (GOVERNOR_DEV_UNSIGNED_EVIDENCE=1) for unit tests that hand-build evidence dicts.
    """

    def __init__(self, master: Path = MASTER, *, dev_unsigned: bool = False, extractor: ExtractorProc | None = None,
                 extra_env: dict | None = None):
        self.key = secrets.token_hex(16)
        self.port = _free_port()
        self.dev_unsigned = dev_unsigned
        self._own_extractor = extractor is None and not dev_unsigned
        self.extractor = extractor or (None if dev_unsigned else ExtractorProc())
        env = {**os.environ, "GOVERNOR_API_KEY": self.key, "GOVERNOR_MASTER": str(master),
               "GOVERNOR_PORT": str(self.port), "GOVERNOR_EVAL_CLOCK": "1"}
        env.pop("GOVERNOR_DEV_UNSIGNED_EVIDENCE", None)
        if self.extractor:
            env["GOVERNOR_EXTRACTOR_PUBKEY"] = self.extractor.public_key_hex
        if dev_unsigned:
            env["GOVERNOR_DEV_UNSIGNED_EVIDENCE"] = "1"
        env.update(extra_env or {})
        self.proc = subprocess.Popen([str(BIN)], env=env, stderr=subprocess.DEVNULL)
        self.base = f"http://127.0.0.1:{self.port}"
        self.http = _client(self.base, {"Authorization": f"Bearer {self.key}"})
        for _ in range(50):
            try:
                if self.http.get("/health").status_code == 200:
                    return
            except httpx.HTTPError:
                time.sleep(0.1)
        self.close()
        raise RuntimeError("governor did not start")

    def reset(self):
        self.http.post("/v1/eval/reset").raise_for_status()

    def post(self, body: dict, now: int = 1_793_000_000) -> dict:
        r = self.http.post("/v1/actions", json=body, headers={"x-eval-now": str(now)})
        r.raise_for_status()
        return r.json()

    def evaluate(self, proposal: dict, evidence: dict, checker_raw: str | None, now: int = 1_793_000_000,
                 proposed_item_lines: list | None = None) -> dict:
        """Old unsigned path. Only meaningful with dev_unsigned=True; a strict governor answers
        REVIEW (evidence_unsigned) because it never reads caller-supplied evidence."""
        body = {"proposal": proposal, "evidence": evidence}
        if checker_raw is not None:
            body["checker_raw"] = checker_raw
        if proposed_item_lines is not None:
            body["proposed_item_lines"] = proposed_item_lines
        return self.post(body, now)

    def evaluate_signed(self, proposal: dict, envelope: dict, checker_raw: str | None, document_sha256: str | None,
                        now: int = 1_793_000_000, proposed_item_lines: list | None = None) -> dict:
        p = dict(proposal)
        if document_sha256 is not None:
            p["document_sha256"] = document_sha256
        body = {"proposal": p, "signed_evidence": envelope}
        if checker_raw is not None:
            body["checker_raw"] = checker_raw
        if proposed_item_lines is not None:
            # Untrusted (model output); the governor verifies it against the signed text layer.
            body["proposed_item_lines"] = proposed_item_lines
        return self.post(body, now)

    def evaluate_document(self, proposal: dict, document: bytes | str | Path, cover_email: dict | None,
                          checker_raw: str | None, now: int = 1_793_000_000, proposed_item_lines: list | None = None) -> dict:
        """Signed path end to end: document → extraction service → signed envelope → governor."""
        if self.extractor is None:
            raise RuntimeError("no extraction service (dev_unsigned governor)")
        doc = document if isinstance(document, bytes) else Path(document).read_bytes()
        env = self.extractor.extract(doc, cover_email)
        return self.evaluate_signed(proposal, env, checker_raw, hashlib.sha256(doc).hexdigest(), now, proposed_item_lines)

    def audit(self) -> dict:
        return self.http.get("/v1/audit/verify").json()

    def close(self):
        if self.proc.poll() is None:
            self.proc.terminate()
            self.proc.wait(timeout=5)
        if self._own_extractor and self.extractor:
            self.extractor.close()

    def __enter__(self):
        return self

    def __exit__(self, *a):
        self.close()
