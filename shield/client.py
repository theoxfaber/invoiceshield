"""Governor process management + HTTP client for the shield-ON pipeline."""
from __future__ import annotations

import os
import secrets
import socket
import subprocess
import time
from pathlib import Path

import httpx

REPO = Path(__file__).resolve().parents[1]
BIN = REPO / "governor" / "target" / "release" / "governor-server"
MASTER = REPO / "dataset" / "fixtures" / "master.json"


def _free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


class GovernorProc:
    """Starts the Rust server on a free local port in eval mode (clock header + reset enabled)."""

    def __init__(self, master: Path = MASTER):
        self.key = secrets.token_hex(16)
        self.port = _free_port()
        env = {**os.environ, "GOVERNOR_API_KEY": self.key, "GOVERNOR_MASTER": str(master),
               "GOVERNOR_PORT": str(self.port), "GOVERNOR_EVAL_CLOCK": "1"}
        self.proc = subprocess.Popen([str(BIN)], env=env, stderr=subprocess.DEVNULL)
        self.base = f"http://127.0.0.1:{self.port}"
        self.http = httpx.Client(base_url=self.base, headers={"Authorization": f"Bearer {self.key}"}, timeout=10, trust_env=False)
        for _ in range(50):
            try:
                if self.http.get("/health").status_code == 200:
                    return
            except httpx.HTTPError:
                time.sleep(0.1)
        raise RuntimeError("governor did not start")

    def reset(self):
        self.http.post("/v1/eval/reset").raise_for_status()

    def evaluate(self, proposal: dict, evidence: dict, checker_raw: str | None, now: int = 1_793_000_000) -> dict:
        body = {"proposal": proposal, "evidence": evidence}
        if checker_raw is not None:
            body["checker_raw"] = checker_raw
        r = self.http.post("/v1/actions", json=body, headers={"x-eval-now": str(now)})
        r.raise_for_status()
        return r.json()

    def audit(self) -> dict:
        return self.http.get("/v1/audit/verify").json()

    def close(self):
        self.proc.terminate()
        self.proc.wait(timeout=5)

    def __enter__(self):
        return self

    def __exit__(self, *a):
        self.close()
