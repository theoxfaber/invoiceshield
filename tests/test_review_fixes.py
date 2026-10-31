"""Post-review hardening (docs/REVIEW_FIXES.md), Python side.

Finding 1: checker output reaches the governor only signed by the checker key and bound to the
evidence; finding 2: the netns fallback is loud and the governor fails closed on it; finding 3a:
no preexec_fn in the threaded extraction service; finding 3b: pdftoppm on attacker PDFs runs with
a scrubbed environment, rlimits, a private temp dir and a namespace attempt.
"""
from __future__ import annotations

import ast
import concurrent.futures
import hashlib
import json
import os
import pathlib
import subprocess
import sys
import time

import pytest

from extractor import jail
from extractor.line_proposer import RENDER_LIMITS, render_env, render_pages
from shield.client import BIN, GovernorProc

ROOT = pathlib.Path(__file__).resolve().parents[1]
CLEAN_PDF = ROOT / "dataset" / "clean" / "C01.pdf"


# ------------------------------------------------------------------ 3a: no preexec_fn

def test_no_preexec_fn_in_shipped_python():
    hits = []
    for p in ROOT.rglob("*.py"):
        rel = p.relative_to(ROOT)
        if rel.parts[0] in {"tests", ".git", "node_modules"} or "site-packages" in rel.parts:
            continue
        try:
            tree = ast.parse(p.read_text(errors="ignore"))
        except SyntaxError:
            continue
        if any(isinstance(n, ast.keyword) and n.arg == "preexec_fn" for n in ast.walk(tree)):
            hits.append(str(rel))
    assert hits == [], f"preexec_fn is unsafe in threaded processes: {hits}"


def _run_jailed(cfg: dict, code: str, env: dict) -> tuple[subprocess.CompletedProcess, bytes]:
    r, w = os.pipe()
    cfg = {**cfg, "netns_fd": w}
    p = subprocess.run(jail.argv(cfg, [sys.executable, "-c", code]), env=env, capture_output=True,
                       pass_fds=(w,), start_new_session=True, timeout=60)
    os.close(w)
    flag = os.read(r, 1)
    os.close(r)
    return p, flag


def test_jail_applies_rlimits_and_reports_netns():
    code = ("import json,os,resource;print(json.dumps({'as':resource.getrlimit(resource.RLIMIT_AS)[0],"
            "'cpu':resource.getrlimit(resource.RLIMIT_CPU)[0],'nofile':resource.getrlimit(resource.RLIMIT_NOFILE)[0],"
            "'core':resource.getrlimit(resource.RLIMIT_CORE)[1],'sid':os.getsid(0)==os.getpid()}))")
    cfg = {"cpu_secs": 7, "mem_bytes": 900 * 1024 * 1024, "nofile": 40, "isolate_network": True}
    p, flag = _run_jailed(cfg, code, {"PATH": "/usr/bin:/bin"})
    assert p.returncode == 0, p.stderr
    got = json.loads(p.stdout)
    assert (got["as"], got["cpu"], got["nofile"], got["core"]) == (900 * 1024 * 1024, 7, 40, 0)
    assert got["sid"], "start_new_session: the tool leads its own session / process group"
    assert flag in (b"0", b"1")
    if flag == b"0":
        assert b"WARNING: no network namespace" in p.stderr, "the fallback must be loud"


def test_jail_netns_blocks_network_when_available():
    code = "import socket\ntry:\n socket.create_connection(('1.1.1.1',53),timeout=2);print('NET')\nexcept OSError:\n print('NONET')"
    p, flag = _run_jailed({"isolate_network": True}, code, {"PATH": "/usr/bin:/bin"})
    if flag != b"1":
        pytest.skip("no network namespace on this host (the governor REVIEWs such evidence by default)")
    assert p.stdout.strip() == b"NONET"


def test_extraction_from_many_threads_does_not_hang():
    """The service is a ThreadingHTTPServer: extraction must be safe from several threads at once."""
    from extractor.sandbox import run_sandboxed

    pdf = CLEAN_PDF.read_bytes()
    with concurrent.futures.ThreadPoolExecutor(4) as ex:
        futs = [ex.submit(run_sandboxed, pdf, {}, "24ZZACB7319B1Z5") for _ in range(4)]
        res = [f.result(timeout=180) for f in futs]
    assert all(r.ok for r in res), [r.failure for r in res]
    assert all(isinstance(r.info.get("network_namespace"), bool) for r in res)


# ------------------------------------------------------------------ 3b: pdftoppm sandbox

def _fake_pdftoppm(tmp_path: pathlib.Path, body: str) -> str:
    tool = tmp_path / "fake-pdftoppm"
    tool.write_text(f"#!{sys.executable}\n" + body)
    tool.chmod(0o755)
    return str(tool)


SPY = """import json, os, resource, sys
out = sys.argv[-1] + "-1.png"
open(out, "w").write(json.dumps({"env": dict(os.environ), "cwd": os.getcwd(),
    "as": resource.getrlimit(resource.RLIMIT_AS)[0], "cpu": resource.getrlimit(resource.RLIMIT_CPU)[0],
    "fsize": resource.getrlimit(resource.RLIMIT_FSIZE)[0], "nofile": resource.getrlimit(resource.RLIMIT_NOFILE)[0],
    "mode": oct(os.stat(os.path.dirname(out)).st_mode & 0o777)}))
"""


def test_render_pages_scrubs_secrets_and_applies_limits(monkeypatch, tmp_path):
    monkeypatch.setenv("BASETEN_API_KEY", "sk-should-never-leak")
    monkeypatch.setenv("GOVERNOR_API_KEY", "gov-should-never-leak")
    tool = _fake_pdftoppm(tmp_path, SPY)
    monkeypatch.setattr("shutil.which", lambda name, path=None: tool)
    imgs = render_pages(b"%PDF-1.4 hostile", max_pages=1)
    assert len(imgs) == 1
    seen = json.loads(imgs[0])
    assert "BASETEN_API_KEY" not in seen["env"] and "GOVERNOR_API_KEY" not in seen["env"]
    assert not any("leak" in v for v in seen["env"].values())
    assert set(seen["env"]) <= set(render_env("x")) | {"PWD", "SHLVL", "_", "LC_CTYPE"}
    assert os.path.realpath(seen["env"]["HOME"]) == os.path.realpath(seen["cwd"]), "private temp dir as HOME and cwd"
    assert seen["mode"] == "0o700"
    assert (seen["as"], seen["cpu"], seen["fsize"], seen["nofile"]) == (
        RENDER_LIMITS["mem_bytes"], RENDER_LIMITS["cpu_secs"], RENDER_LIMITS["fsize_bytes"], RENDER_LIMITS["nofile"])


def test_render_pages_times_out_and_kills_the_group(monkeypatch, tmp_path):
    tool = _fake_pdftoppm(tmp_path, "import time\ntime.sleep(30)\n")
    monkeypatch.setattr("shutil.which", lambda name, path=None: tool)
    t0 = time.monotonic()
    assert render_pages(b"%PDF-1.4", timeout_s=0.5) == []
    assert time.monotonic() - t0 < 10


def test_render_pages_tool_failure_returns_no_images(monkeypatch, tmp_path):
    tool = _fake_pdftoppm(tmp_path, "import sys\nsys.exit(3)\n")
    monkeypatch.setattr("shutil.which", lambda name, path=None: tool)
    assert render_pages(b"%PDF-1.4") == []


@pytest.mark.skipif(not os.path.exists("/usr/bin/pdftoppm"), reason="poppler-utils not installed")
def test_render_pages_real_pdftoppm_still_renders():
    imgs = render_pages(CLEAN_PDF.read_bytes(), max_pages=1)
    assert len(imgs) == 1 and imgs[0][:8] == b"\x89PNG\r\n\x1a\n"


# ------------------------------------------------------------------ 1 + 2 end to end

needs_bin = pytest.mark.skipif(not BIN.exists(), reason="build governor first: cargo build --release")
CASE = next(c for c in json.loads((ROOT / "dataset" / "clean" / "manifest.json").read_text()) if c["id"] == "C01")
DOC = (ROOT / CASE["pdf"]).read_bytes()
SHA = hashlib.sha256(DOC).hexdigest()
PROP = {k: CASE[k] for k in ("vendor_id", "invoice_no", "amount_paise", "po_ref")}
CLEAR = json.dumps({"signals": []})


@pytest.fixture(scope="module")
def gov():
    with GovernorProc(allow_no_netns=True) as g:
        yield g


def sig_kinds(d):
    return [s["kind"] for s in d["signals"]]


@needs_bin
def test_signed_checker_path_allows_a_clean_invoice(gov):
    gov.reset()
    d = gov.evaluate_document(PROP, DOC, CASE["cover_email"], CLEAR, now=CASE["now"])
    assert d["verdict"] == "allow", (d["reasons"], d["signals"])


@needs_bin
def test_agent_supplied_empty_checker_is_review(gov):
    """The review's attack: the agent sends checker_raw={"signals":[]} itself."""
    gov.reset()
    env = gov.extractor.extract(DOC, CASE["cover_email"])
    d = gov.post({"proposal": {**PROP, "document_sha256": SHA}, "signed_evidence": env, "checker_raw": CLEAR}, CASE["now"])
    assert d["verdict"] == "review" and sig_kinds(d) == ["checker_unsigned"]


@needs_bin
def test_dropped_checker_is_review(gov):
    gov.reset()
    d = gov.evaluate_document(PROP, DOC, CASE["cover_email"], None, now=CASE["now"])
    assert d["verdict"] == "review" and sig_kinds(d) == ["checker_missing"]


@needs_bin
def test_checker_replayed_onto_new_evidence_is_review(gov):
    from checker.checker import sign_checker_report

    gov.reset()
    env_a = gov.extractor.extract(DOC, CASE["cover_email"])
    c_a = sign_checker_report(gov._checker_key, CLEAR, env_a)
    env_b = gov.extractor.extract(DOC, CASE["cover_email"])
    d = gov.post({"proposal": {**PROP, "document_sha256": SHA}, "signed_evidence": env_b, "signed_checker": c_a}, CASE["now"])
    assert d["verdict"] == "review" and sig_kinds(d) == ["checker_nonce_mismatch"]


@needs_bin
def test_checker_signed_by_another_key_is_review(gov):
    from checker.checker import sign_checker_report
    from extractor import signing

    gov.reset()
    env = gov.extractor.extract(DOC, CASE["cover_email"])
    forged = sign_checker_report(signing.generate(), CLEAR, env)
    d = gov.post({"proposal": {**PROP, "document_sha256": SHA}, "signed_evidence": env, "signed_checker": forged}, CASE["now"])
    assert d["verdict"] == "review" and sig_kinds(d) == ["checker_signature_invalid"]


class _InProcessExtractor:
    """An Extractor (real code, real signing) whose key the test holds, so it can re-sign the
    evidence with sandbox.network_namespace=false exactly as the service does on a host where
    unshare is refused."""

    def __init__(self):
        from extractor.service import Extractor

        self.ex = Extractor()
        self.public_key_hex, self.key_id = self.ex.public_hex, self.ex.key_id

    def extract_no_netns(self, doc: bytes, email: dict) -> dict:
        import secrets

        from extractor import signing

        p = json.loads(self.ex.extract(doc, email)["payload"])
        p["sandbox"] = {"wall_s": 0.1, "network_namespace": False}
        p["nonce"] = secrets.token_hex(16)
        return signing.sign(self.ex._sk, p)

    def close(self):
        pass


@needs_bin
@pytest.mark.parametrize("allow,want", [(False, "review"), (True, "allow")])
def test_governor_fails_closed_on_signed_netns_false(allow, want):
    """Finding 2: evidence signed with sandbox.network_namespace=false is REVIEW unless allowed."""
    from checker.checker import sign_checker_report

    ex = _InProcessExtractor()
    with GovernorProc(extractor=ex, allow_no_netns=allow) as g:
        env = ex.extract_no_netns(DOC, CASE["cover_email"])
        body = {"proposal": {**PROP, "document_sha256": SHA}, "signed_evidence": env,
                "signed_checker": sign_checker_report(g._checker_key, CLEAR, env)}
        d = g.post(body, CASE["now"])
    assert d["verdict"] == want, (d["reasons"], d["signals"])
    if not allow:
        assert [r["rule"] for r in d["reasons"]] == ["sandbox_network_not_isolated"]


# ------------------------------------------------------------------ sandbox caps (deployment)

def test_sandbox_default_caps_match_the_deployment(monkeypatch, tmp_path):
    import importlib

    for k in ("SANDBOX_SPEND_CAP_USD", "SANDBOX_MODEL_CALLS_PER_DAY", "SANDBOX_USD_PER_MODEL_CALL"):
        monkeypatch.delenv(k, raising=False)
    monkeypatch.setenv("SANDBOX_STATE", str(tmp_path / "s.json"))
    monkeypatch.delenv("BASETEN_API_KEY", raising=False)
    import sandbox.app as app

    app = importlib.reload(app)
    assert (app.SPEND_CAP_USD, app.MODEL_CALLS_PER_DAY, app.MODEL_CALLS_TOTAL) == (5.0, 50, 250)
    monkeypatch.setenv("SANDBOX_MODEL_CALLS_PER_DAY", "7")
    monkeypatch.setenv("SANDBOX_SPEND_CAP_USD", "1")
    app = importlib.reload(app)
    assert (app.MODEL_CALLS_PER_DAY, app.MODEL_CALLS_TOTAL) == (7, 50), "env overrides still work"
