"""Phase 2 trust boundary, end to end through the real governor binary and extraction service:
the agent side can only forward evidence the extractor signed, for the document it names."""
import base64
import hashlib
import json
import pathlib
import time

import pytest
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey

from extractor import signing
from shield.client import BIN, ExtractorProc, GovernorProc

pytestmark = pytest.mark.skipif(not BIN.exists(), reason="build governor first: cargo build --release")
ROOT = pathlib.Path(__file__).resolve().parents[1]
CASE = next(c for c in json.loads((ROOT / "dataset" / "clean" / "manifest.json").read_text()) if c["id"] == "C01")
DOC = (ROOT / CASE["pdf"]).read_bytes()
SHA = hashlib.sha256(DOC).hexdigest()
PROP = {k: CASE[k] for k in ("vendor_id", "invoice_no", "amount_paise", "po_ref")}
CLEAR = json.dumps({"signals": []})
NOW = CASE["now"]


@pytest.fixture(scope="module")
def gov():
    with GovernorProc() as g:
        yield g


def rules(d):
    return [r["rule"] for r in d["reasons"]]


def test_valid_signed_evidence_passes(gov):
    gov.reset()
    d = gov.evaluate_document(PROP, DOC, CASE["cover_email"], CLEAR, now=NOW)
    assert d["verdict"] == "allow", d["reasons"]


def test_forged_unsigned_evidence_is_never_read(gov):
    """The review's attack: empty printed_bank_account, correct GSTIN, sent by the agent."""
    gov.reset()
    forged = {"text_layer_gstin": "36ZZACD8350H1Z3", "sender_domain": "deccanelectricals.co.in", "has_text_layer": True,
              "printed_bank_account": None, "all_printed_accounts": []}
    d = gov.evaluate({**PROP, "document_sha256": SHA}, forged, CLEAR, now=NOW)
    assert d["verdict"] == "review" and rules(d) == ["evidence_unsigned"] and d["payee"] is None


def test_forged_signature_blocks(gov):
    gov.reset()
    attacker = Ed25519PrivateKey.generate()
    payload = {"v": 1, "key_id": gov.extractor.key_id, "doc_sha256": SHA, "extractor_version": "x", "issued_at": int(time.time()),
               "nonce": "ab" * 16, "status": "ok", "evidence": {"text_layer_gstin": "36ZZACD8350H1Z3", "has_text_layer": True}}
    env = signing.sign(attacker, payload)
    d = gov.evaluate_signed(PROP, env, CLEAR, SHA, now=NOW)
    assert d["verdict"] == "block" and rules(d) == ["evidence_signature_invalid"]


def test_edited_signed_payload_blocks(gov):
    gov.reset()
    env = gov.extractor.extract(DOC, CASE["cover_email"])
    p = json.loads(env["payload"])
    p["evidence"]["all_printed_accounts"] = []
    env["payload"] = signing.canonical(p).decode()
    d = gov.evaluate_signed(PROP, env, CLEAR, SHA, now=NOW)
    assert d["verdict"] == "block" and rules(d) == ["evidence_signature_invalid"]


def test_tampered_document_hash_blocks(gov):
    gov.reset()
    env = gov.extractor.extract(DOC, CASE["cover_email"])
    d = gov.evaluate_signed(PROP, env, CLEAR, hashlib.sha256(DOC + b"x").hexdigest(), now=NOW)
    assert d["verdict"] == "block" and rules(d) == ["document_hash_mismatch"]


def test_evidence_for_another_document_blocks(gov):
    """Agent swaps in signed evidence of a clean invoice while proposing for an attack invoice."""
    gov.reset()
    other = (ROOT / "dataset" / "clean" / "C02.pdf").read_bytes()
    env = gov.extractor.extract(other, CASE["cover_email"])
    d = gov.evaluate_signed(PROP, env, CLEAR, SHA, now=NOW)
    assert d["verdict"] == "block" and rules(d) == ["document_hash_mismatch"]


def test_missing_document_hash_reviews(gov):
    gov.reset()
    env = gov.extractor.extract(DOC, CASE["cover_email"])
    d = gov.evaluate_signed(PROP, env, CLEAR, None, now=NOW)
    assert d["verdict"] == "review" and rules(d) == ["document_hash_missing"]


def test_replay_blocks(gov):
    gov.reset()
    env = gov.extractor.extract(DOC, CASE["cover_email"])
    assert gov.evaluate_signed(PROP, env, CLEAR, SHA, now=NOW)["verdict"] == "allow"
    gov.reset()  # a fresh ledger does not make a spent nonce usable again
    d = gov.evaluate_signed(PROP, env, CLEAR, SHA, now=NOW)
    assert d["verdict"] == "block" and rules(d) == ["evidence_replayed"]


def test_evidence_from_another_extractor_blocks(gov):
    gov.reset()
    with ExtractorProc() as rogue:
        env = rogue.extract(DOC, CASE["cover_email"])
    d = gov.evaluate_signed(PROP, env, CLEAR, SHA, now=NOW)
    assert d["verdict"] == "block" and rules(d) == ["evidence_signature_invalid"]


def test_unparseable_document_reviews(gov):
    gov.reset()
    bad = DOC[: len(DOC) // 2]
    d = gov.evaluate_document(PROP, bad, CASE["cover_email"], CLEAR, now=NOW)
    assert d["verdict"] == "review" and rules(d) == ["extraction_failed"]


def test_stale_evidence_reviews():
    with GovernorProc(extra_env={"GOVERNOR_EVIDENCE_MAX_AGE_SECS": "1"}) as g:
        env = g.extractor.extract(DOC, CASE["cover_email"])
        time.sleep(2.2)
        d = g.evaluate_signed(PROP, env, CLEAR, SHA, now=NOW)
    assert d["verdict"] == "review" and rules(d) == ["evidence_stale"]


def test_audit_chain_records_rejections(gov):
    gov.reset()
    gov.evaluate({**PROP, "document_sha256": SHA}, {}, CLEAR, now=NOW)
    a = gov.audit()
    assert a["records"] >= 1 and a["first_broken"] is None


def test_governor_refuses_to_start_without_pubkey():
    import os, subprocess
    env = {**os.environ, "GOVERNOR_API_KEY": "k" * 32, "GOVERNOR_PORT": "1", "GOVERNOR_MASTER": str(ROOT / "dataset" / "fixtures" / "master.json")}
    env.pop("GOVERNOR_EXTRACTOR_PUBKEY", None)
    env.pop("GOVERNOR_DEV_UNSIGNED_EVIDENCE", None)
    r = subprocess.run([str(BIN)], env=env, capture_output=True, timeout=10)
    assert r.returncode == 2 and b"GOVERNOR_EXTRACTOR_PUBKEY" in r.stderr


def test_dev_mode_refused_outside_eval_clock():
    import os, subprocess
    env = {**os.environ, "GOVERNOR_API_KEY": "k" * 32, "GOVERNOR_PORT": "1", "GOVERNOR_DEV_UNSIGNED_EVIDENCE": "1",
           "GOVERNOR_MASTER": str(ROOT / "dataset" / "fixtures" / "master.json")}
    env.pop("GOVERNOR_EVAL_CLOCK", None)
    r = subprocess.run([str(BIN)], env=env, capture_output=True, timeout=10)
    assert r.returncode == 2 and b"tests only" in r.stderr


def test_agent_process_never_holds_the_signing_key():
    """The client only learns the public key; the private key never leaves the extractor process."""
    with ExtractorProc() as ex:
        assert len(bytes.fromhex(ex.public_key_hex)) == 32
        assert not any(isinstance(v, Ed25519PrivateKey) for v in vars(ex).values())
