"""Run extraction of a hostile document in a resource-limited subprocess.

Limits (all configurable, see Limits): input size checked before spawning; page count
checked inside the worker; CPU seconds (RLIMIT_CPU), address space (RLIMIT_AS), file size
(RLIMIT_FSIZE), open files (RLIMIT_NOFILE), no core dumps; wall-clock timeout with SIGKILL
of the whole process group; stdout capped. Network: the worker is moved into fresh user +
network namespaces (no interfaces but a down loopback) when the kernel allows it, and Python
sockets are disabled inside the worker either way. Environment is emptied (no API keys).

Post-review hardening (docs/REVIEW_FIXES.md): no `preexec_fn` (unsafe in this threaded service);
limits and the namespace are applied by the exec wrapper extractor/jail.py in a fresh process.
When the namespace cannot be created the fallback is logged as a WARNING here and in the jail's
stderr, and the signed `sandbox.network_namespace=false` makes the governor answer REVIEW unless
GOVERNOR_ALLOW_NO_NETNS=1.
Any failure returns ok=False with a short reason; the service signs that as status "failed"
and the governor answers REVIEW.
"""
from __future__ import annotations

import json
import logging
import os
import signal
import subprocess
import sys
import tempfile
import time
from dataclasses import asdict, dataclass, field
from pathlib import Path

from extractor import jail

REPO = Path(__file__).resolve().parents[1]
log = logging.getLogger("extractor.sandbox")


@dataclass(frozen=True)
class Limits:
    max_bytes: int = 10 * 1024 * 1024
    max_pages: int = 50
    cpu_secs: int = 30
    mem_bytes: int = 1536 * 1024 * 1024
    wall_secs: float = 60.0
    fsize_bytes: int = 64 * 1024 * 1024
    nofile: int = 256
    max_stdout: int = 4 * 1024 * 1024
    isolate_network: bool = True


@dataclass
class SandboxResult:
    ok: bool
    evidence: dict | None = None
    failure: str | None = None
    info: dict = field(default_factory=dict)


def jail_config(lim: Limits, netns_fd: int | None) -> dict:
    return {"cpu_secs": lim.cpu_secs, "mem_bytes": lim.mem_bytes, "fsize_bytes": lim.fsize_bytes,
            "nofile": lim.nofile, "isolate_network": lim.isolate_network, "netns_fd": netns_fd}


def run_sandboxed(pdf: bytes, cover_email: dict | None, buyer_gstin: str, lim: Limits = Limits()) -> SandboxResult:
    info: dict = {"limits": asdict(lim)}
    if len(pdf) > lim.max_bytes:
        return SandboxResult(False, failure=f"file_too_large:{len(pdf)}>{lim.max_bytes}", info=info)
    if b"%PDF-" not in pdf[:1024]:
        return SandboxResult(False, failure="not_a_pdf", info=info)
    with tempfile.TemporaryDirectory(prefix="isx-") as d:
        os.chmod(d, 0o700)
        (Path(d) / "doc.pdf").write_bytes(pdf)
        (Path(d) / "req.json").write_text(json.dumps({"cover_email": cover_email or {}, "buyer_gstin": buyer_gstin, "max_pages": lim.max_pages}))
        r_fd, w_fd = os.pipe() if lim.isolate_network else (None, None)
        # Repo + the parent's installed packages (user site-packages is keyed on HOME, which we change).
        site = [p for p in sys.path if p and ("site-packages" in p or "dist-packages" in p)]
        env = {"PYTHONPATH": os.pathsep.join([str(REPO), *site]), "PATH": "/usr/bin:/bin", "LANG": "C.UTF-8", "PYTHONDONTWRITEBYTECODE": "1",
               "HOME": d, "TMPDIR": d, "OMP_THREAD_LIMIT": "1"}
        t0 = time.monotonic()
        cmd = jail.argv(jail_config(lim, w_fd), [sys.executable, "-m", "extractor.worker", d])
        proc = subprocess.Popen(cmd, cwd=str(REPO), env=env, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                                stderr=subprocess.PIPE, start_new_session=True,
                                pass_fds=(w_fd,) if w_fd is not None else ())
        if w_fd is not None:
            os.close(w_fd)
        try:
            out, err = proc.communicate(timeout=lim.wall_secs)
        except subprocess.TimeoutExpired:
            try:
                os.killpg(proc.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            proc.communicate()
            info["wall_s"] = round(time.monotonic() - t0, 3)
            return SandboxResult(False, failure="wall_clock_timeout", info=info)
        finally:
            if r_fd is not None:
                flag = os.read(r_fd, 1)
                os.close(r_fd)
                info["network_namespace"] = flag == b"1"
                if flag != b"1":
                    log.warning("extraction sandbox: NO network namespace (unshare refused); worker ran with "
                                "Python socket patching only. The governor will REVIEW this document unless "
                                "GOVERNOR_ALLOW_NO_NETNS=1.")
            try:
                os.killpg(proc.pid, signal.SIGKILL)  # reap stray children (pdftoppm/tesseract)
            except (ProcessLookupError, PermissionError):
                pass
        info["wall_s"] = round(time.monotonic() - t0, 3)
        info["returncode"] = proc.returncode
        if proc.returncode != 0:
            sig = -proc.returncode if proc.returncode < 0 else None
            reason = {signal.SIGXCPU: "cpu_limit", signal.SIGKILL: "killed", signal.SIGSEGV: "crash"}.get(sig, f"exit_{proc.returncode}")
            if b"MemoryError" in err:
                reason = "memory_limit"
            return SandboxResult(False, failure=reason, info=info)
        if len(out) > lim.max_stdout:
            return SandboxResult(False, failure="output_too_large", info=info)
        try:
            res = json.loads(out.decode("utf-8"))
        except (UnicodeDecodeError, json.JSONDecodeError):
            return SandboxResult(False, failure="worker_output_invalid", info=info)
        if not isinstance(res, dict) or not res.get("ok"):
            return SandboxResult(False, failure=str((res or {}).get("failure", "worker_failed"))[:200], info=info)
        ev = res.get("evidence")
        if not isinstance(ev, dict):
            return SandboxResult(False, failure="worker_output_invalid", info=info)
        info["pages"] = res.get("pages")
        return SandboxResult(True, evidence=ev, info=info)
