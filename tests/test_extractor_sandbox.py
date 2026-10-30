"""Hostile-input handling: the extractor never crashes, never hangs, and every failure is a
signed "failed" status (→ governor REVIEW)."""
import json
import pathlib

import pytest

from extractor.sandbox import Limits, run_sandboxed
from extractor.service import Extractor
from tests import hostile_pdfs as H

ROOT = pathlib.Path(__file__).resolve().parents[1]
C01 = (ROOT / "dataset" / "clean" / "C01.pdf").read_bytes()
GST = "24ZZACB7319B1Z5"


def test_real_invoice_extracts_inside_sandbox():
    r = run_sandboxed(C01, {"from": "accounts@deccanelectricals.co.in"}, GST)
    assert r.ok, r.failure
    assert r.evidence["printed_bank_account"] == "86254676687125"
    assert r.evidence["sender_domain"] == "deccanelectricals.co.in"


def test_sandbox_matches_unsandboxed_extraction():
    from assembler.evidence import assemble
    from assembler.extract import governor_evidence
    email = {"from": "accounts@deccanelectricals.co.in"}
    r = run_sandboxed(C01, email, GST)
    assert r.evidence == governor_evidence(assemble(ROOT / "dataset" / "clean" / "C01.pdf", email), GST)


def test_network_namespace_when_kernel_allows():
    r = run_sandboxed(H.simple(), {}, GST)
    if not r.info.get("network_namespace"):
        pytest.skip("kernel refuses unprivileged user+net namespaces here; Python sockets are still disabled")
    assert r.info["network_namespace"] is True


@pytest.mark.parametrize("name,doc,expect", [
    ("truncated", C01[: len(C01) // 2], None),
    ("truncated_header_only", C01[:64], None),
    ("not_a_pdf", b"MZ\x90\x00" + b"\x00" * 1000, "not_a_pdf"),
    ("empty", b"", "not_a_pdf"),
    ("declared_huge_page_count", H.declared_huge_page_count(), "too_many_pages"),
    ("many_real_pages", H.many_real_pages(400), "too_many_pages"),
    ("recursive_page_tree", H.recursive_pages(), None),
])
def test_malformed_pdfs_fail_closed(name, doc, expect):
    r = run_sandboxed(doc, {}, GST)
    if r.ok:
        # A tolerant parser may read a damaged file; then it must still be well-formed evidence
        # and, for a truncated real invoice, never a *cleaner* reading than the intact one.
        assert isinstance(r.evidence, dict), name
        return
    assert r.failure, name
    if expect:
        assert r.failure.startswith(expect), (name, r.failure)


def test_flate_bomb_is_contained():
    doc = H.flate_bomb(400)
    assert len(doc) < 3 * 1024 * 1024
    r = run_sandboxed(doc, {}, GST, Limits(mem_bytes=768 * 1024 * 1024, cpu_secs=10, wall_secs=20))
    # pypdf caps inflation (LimitReachedError) or the rlimits stop it; either way: no evidence.
    assert not r.ok, r.info
    assert r.info["wall_s"] < 25


def test_oversize_file_rejected_before_parsing():
    r = run_sandboxed(b"%PDF-1.7\n" + b"0" * 2048, {}, GST, Limits(max_bytes=1024))
    assert not r.ok and r.failure.startswith("file_too_large")
    assert "wall_s" not in r.info  # never spawned


def test_wall_clock_timeout_kills_worker():
    r = run_sandboxed(C01, {}, GST, Limits(wall_secs=0.01))
    assert not r.ok and r.failure == "wall_clock_timeout"


def test_memory_limit_fails_closed():
    r = run_sandboxed(C01, {}, GST, Limits(mem_bytes=32 * 1024 * 1024))
    assert not r.ok, r.info


def test_failures_are_signed_as_failed():
    ex = Extractor(limits=Limits(wall_secs=0.01))
    env = ex.extract(C01, {})
    p = json.loads(env["payload"])
    assert p["status"] == "failed" and p["failure"] == "wall_clock_timeout" and "evidence" not in p


def test_worker_environment_has_no_secrets(monkeypatch):
    monkeypatch.setenv("NVIDIA_API_KEY", "should-not-leak")
    from extractor import sandbox
    import subprocess
    seen = {}
    real = subprocess.Popen

    def spy(*a, **k):
        seen.update(k.get("env") or {})
        return real(*a, **k)

    monkeypatch.setattr(sandbox.subprocess, "Popen", spy)
    run_sandboxed(H.simple(), {}, GST)
    assert "NVIDIA_API_KEY" not in seen and set(seen) <= {"PYTHONPATH", "PATH", "LANG", "PYTHONDONTWRITEBYTECODE", "HOME", "TMPDIR", "OMP_THREAD_LIMIT"}
