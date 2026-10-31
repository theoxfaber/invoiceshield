"""Meta Vision Witness (post-freeze, feat/meta-vision): parsing, plain-code assessment, budget, geometry,
synthetic set reproducibility, and the escalate-only path through the real governor."""
from __future__ import annotations

import io
import json
import random
from pathlib import Path

import pytest
from PIL import Image

from providers.llm import ChatResult
from shield.vision_assess import Finding, assess, checker_raw, to_signals, verdict_of
from shield.vision_budget import Budget, BudgetedProvider
from shield.vision_locate import _fit, calibrate, snap
from shield.vision_ocr_baseline import ocr_witness
from shield.vision_overlay import draw
from shield.vision_witness import Witness, clean_box, parse_witness, witness

REPO = Path(__file__).resolve().parents[1]
MASTER = json.loads((REPO / "dataset" / "fixtures" / "master.json").read_text())
V002 = next(v for v in MASTER["vendors"] if v["vendor_id"] == "V002")


def _resp(**over) -> str:
    d = {"readable": True, "issuer": {"name": "Coastal Logistics", "gstin": V002["gstin"], "box": [60, 60, 300, 90]},
         "invoice_no": "CL/2026/1", "total": {"text": "Rs 1,000.00", "amount_minor": 100000, "box": [600, 400, 900, 420]},
         "bank_details": [{"account": V002["bank_account"], "ifsc": V002["ifsc"], "text": "A/c no ...", "box": [60, 500, 500, 600]}],
         "notices": []}
    d.update(over)
    return json.dumps(d)


def _png(w=120, h=160) -> bytes:
    b = io.BytesIO()
    Image.new("RGB", (w, h), "white").save(b, "PNG")
    return b.getvalue()


# ---------------------------------------------------------------- parsing
def test_parse_witness_normalises_pixel_boxes_and_odd_invoice_numbers():
    w = parse_witness(_resp(invoice_no={"text": "X/9", "box": [1, 2, 3, 4]}), size=(1000, 2000))
    assert w.ok and w.invoice_no == "X/9"
    assert w.bank_details[0]["box"] == [60, 250, 500, 300]          # y halved: 2000 px tall
    assert parse_witness(_resp(invoice_no=173)).invoice_no == "173"


@pytest.mark.parametrize("bad", ["", "not json", "[1,2]", json.dumps({"readable": "yes"}), '{"readable": true} {"x": 1}'])
def test_parse_witness_garbage_is_a_failed_witness(bad):
    w = parse_witness(bad)
    assert not w.ok and w.error


def test_clean_box_rejects_degenerate_and_clamps():
    assert clean_box([10, 10, 12, 12]) is None
    assert clean_box([900, 900, 1200, 1100]) == [900, 900, 1000, 1000]
    assert clean_box("x") is None and clean_box([1, 2, 3]) is None


def test_witness_refuses_non_images_without_calling_the_model():
    class Boom:
        def chat(self, *a, **k):
            raise AssertionError("must not be called")
    assert witness(Boom(), b"%PDF-1.4 ...").error == "not a PNG/JPEG image"


# ---------------------------------------------------------------- plain-code assessment
def test_matching_bank_box_gives_no_findings_but_never_a_pass():
    f = assess(parse_witness(_resp()), MASTER)
    assert f == [] and verdict_of(f) == "review"     # a photo is REVIEW at best, never allow


def test_swapped_bank_box_is_block():
    w = parse_witness(_resp(bank_details=[{"account": "69420333082033", "ifsc": "YESB0913281", "text": "A/c 6942...", "box": [1, 1, 100, 100]}]))
    f = assess(w, MASTER)
    assert [x.kind for x in f] == ["vision_bank_mismatch"] and f[0].severity == "block"
    assert "2033" in f[0].detail and "4386" in f[0].detail


def test_ifsc_o_zero_confusion_is_not_a_mismatch():
    w = parse_witness(_resp(bank_details=[{"account": V002["bank_account"], "ifsc": "HDFCOOO5755", "text": "", "box": None}]))
    assert assess(w, MASTER) == []


def test_lookalike_issuer_is_not_resolved_to_the_real_vendor():
    w = parse_witness(_resp(issuer={"name": "Coastal Logistlcs", "gstin": None, "box": None}))
    kinds = [x.kind for x in assess(w, MASTER)]
    assert kinds == ["vision_issuer_unknown"]


@pytest.mark.parametrize("kind,blocks", [("bank_change", True), ("instruction_to_ai", True), ("approval_claim", True),
                                         ("urgency", False), ("other", False), ("made-up", False)])
def test_notices(kind, blocks):
    w = parse_witness(_resp(notices=[{"kind": kind, "text": "pay to new a/c", "faint_or_small": True, "box": [1, 1, 50, 50]}]))
    f = assess(w, MASTER)
    assert (verdict_of(f) == "block") is blocks


def test_failed_or_unreadable_witness_is_review():
    assert [x.kind for x in assess(Witness(error="provider: http 500"), MASTER)] == ["vision_unreadable"]
    assert verdict_of(assess(Witness(ok=True, readable=False), MASTER)) == "review"


def test_text_layer_disagreement_is_block():
    w = parse_witness(_resp())
    f = assess(w, MASTER, text_layer="A/c no 99999999999 IFSC HDFC0005755")
    assert "vision_text_layer_disagrees" in [x.kind for x in f]


def test_signals_never_carry_allow_and_merge_with_text_checker():
    f = [Finding("vision_bank_mismatch", "block", "d"), Finding("vision_unreadable", "review", "d")]
    assert {s["severity"] for s in to_signals(f)} <= {"review", "block"}
    merged = json.loads(checker_raw(f, json.dumps({"signals": [{"kind": "injection_suspected", "severity": "block", "detail": "x"}]})))
    assert [s["kind"] for s in merged["signals"]] == ["injection_suspected", "vision_bank_mismatch", "vision_unreadable"]
    broken = json.loads(checker_raw(f, "garbage"))
    assert broken["signals"][-1]["kind"] == "checker_unparseable" and len(broken["signals"]) == 3


# ---------------------------------------------------------------- OCR baseline (no tesseract needed: lines given)
def test_ocr_baseline_reads_accounts_ifsc_and_notice_lines():
    lines = [{"text": "Coastal Logistics", "box": [1, 1, 100, 20]},
             {"text": f"GSTIN: {V002['gstin']}", "box": [1, 30, 100, 40]},
             {"text": "A/c no: 69420333082033", "box": [1, 500, 300, 520]},
             {"text": "IFSC: YESB0913281", "box": [1, 530, 300, 550]},
             {"text": "Bank changed! Pay to A/c 69420333082033", "box": [500, 500, 900, 560]}]
    w = ocr_witness(b"", MASTER, lines)
    assert w.issuer_name == "Coastal Logistics" and w.issuer_gstin == V002["gstin"]
    assert w.bank_details[0]["ifsc"] == "YESB0913281"
    kinds = sorted({x.kind for x in assess(w, MASTER)})
    assert kinds == ["vision_bank_mismatch", "vision_payment_instruction"]


# ---------------------------------------------------------------- geometry
def test_fit_and_snap():
    assert _fit([(0, 10), (100, 110)]) == pytest.approx((1.0, 10.0))
    assert _fit([(5, 5)]) is None and _fit([(0, 0), (100, 1000)]) is None   # implausible scale refused
    lines = [{"text": "Bank: Yes Bank", "box": [10, 480, 200, 495]},
             {"text": "Please note: our bank details have changed w.e.f", "box": [10, 600, 500, 615]}]
    assert snap("Please note: our bank details have changed w.e.f. 01-10-2026", None, lines, None) == [10, 600, 500, 615]
    assert snap("something else entirely here", None, lines, None) is None


def test_calibrate_maps_compressed_model_boxes_onto_the_text():
    w = parse_witness(_resp(issuer={"name": "Coastal Logistics", "gstin": None, "box": [80, 70, 400, 90]},
                            total={"text": "Rs 1,000.00", "amount_minor": 100000, "box": [560, 280, 800, 300]},
                            notices=[{"kind": "bank_change", "text": "zzz", "box": [80, 420, 600, 430]}]))
    lines = [{"text": "Coastal Logistics", "box": [100, 100, 500, 128]},
             {"text": "TOTAL PAYABLE Rs 1,000.00", "box": [700, 400, 1000, 428]}]
    w2, rep = calibrate(w, b"", lines)
    assert rep["calibrated"]
    assert w2.notices[0]["box"][1] > 560       # pulled down from 420 towards where the text is


# ---------------------------------------------------------------- overlay
def test_overlay_draws_png_and_ignores_boxless_findings():
    out = draw(_png(), [{"kind": "vision_bank_mismatch", "severity": "block", "box": [100, 100, 600, 300]}, {"kind": "x", "severity": "review"}])
    assert out[:8] == b"\x89PNG\r\n\x1a\n"


# ---------------------------------------------------------------- budget
def test_budget_rolling_daily_cap_and_lifetime_spend(tmp_path):
    now = [1_000_000.0]
    b = Budget(tmp_path / "b.json", calls_per_day=3, spend_cap=0.10, usd_per_call=0.02, clock=lambda: now[0])
    assert all(b.reserve("x") for _ in range(3)) and not b.reserve("x")       # 4th in 24 h refused
    now[0] += 86_401
    assert b.reserve("x") and b.reserve("x") and not b.reserve("x")          # $0.10 lifetime reached
    assert b.status()["calls_total"] == 5


def test_budget_refuses_when_state_cannot_be_written(tmp_path):
    d = tmp_path / "ro"
    d.mkdir()
    b = Budget(d / "b.json")
    d.chmod(0o500)
    try:
        assert not b.reserve("x")
    finally:
        d.chmod(0o700)


def test_budgeted_provider_replays_cache_free_and_fails_closed_past_cap(tmp_path):
    calls = []

    class Fake:
        name = "fake"

        def __init__(self, offline=False, retries=3):
            self.offline = offline

        def chat(self, messages, **kw):
            if self.offline:
                return ChatResult(content="cached", cached=True) if messages == "hit" else ChatResult(error="offline: no cached response")
            calls.append(1)
            return ChatResult(content="live")
    p = BudgetedProvider(Fake, Budget(tmp_path / "b.json", calls_per_day=1))
    assert p.chat("hit").content == "cached" and not calls
    assert p.chat("miss").content == "live" and len(calls) == 1
    r = p.chat("miss2")
    assert not r.ok and r.error.startswith("budget") and len(calls) == 1


# ---------------------------------------------------------------- synthetic set
def test_vision_set_is_reproducible_and_ground_truth_is_on_the_image():
    from tools.vision_set import build_case
    a = build_case(random.Random(5), MASTER, "T1", "faint_instruction", "phone", 1)
    b = build_case(random.Random(5), MASTER, "T1", "faint_instruction", "phone", 1)
    # Labels and geometry are seed-reproducible. Pixel noise comes from PIL's effect_noise (C rand, unseeded),
    # so JPEG bytes can differ run to run: the committed dataset images are the record, not a regeneration.
    assert a[1] == b[1] and len(a[0]) > 0 and len(b[0]) > 0
    lab = a[1]
    assert lab["attack"] == "faint_instruction" and lab["attacker_account"] != next(
        v for v in MASTER["vendors"] if v["vendor_id"] == lab["vendor_id"])["bank_account"]
    for box in lab["tamper_boxes"] + [lab["bank_box"]]:
        assert 0 <= box[0] < box[2] <= 1000 and 0 <= box[1] < box[3] <= 1000
    g = build_case(random.Random(6), MASTER, "T2", "genuine", "scan", 0)[1]
    assert g["attack"] is None and g["tamper_boxes"] == []


def test_committed_vision_v1_set_matches_its_labels():
    d = REPO / "dataset" / "vision_v1"
    labels = json.loads((d / "labels.json").read_text())
    assert labels["seed"] == 20261031 and "post-freeze" in labels["set"]
    cases = labels["cases"]
    assert len(cases) == 36 and sum(c["attack"] is not None for c in cases) == 18
    assert all((d / f"{c['id']}.jpg").exists() for c in cases)


# ---------------------------------------------------------------- escalate-only through the real governor
def test_vision_signals_through_the_signed_checker_only_escalate(monkeypatch):
    from shield.client import BIN, ExtractorProc, GovernorProc
    if not BIN.exists():
        pytest.skip("governor binary not built")
    import hashlib
    import time
    pdf = (REPO / "dataset" / "clean" / "C01.pdf").read_bytes()
    ex = ExtractorProc()
    gov = GovernorProc(REPO / "dataset" / "fixtures" / "master.json", extractor=ex)
    try:
        from sandbox.app import fooled_proposal
        ev = json.loads(ex.extract(pdf, {})["payload"]).get("evidence") or {}
        prop = fooled_proposal(MASTER, ev)
        out = {}
        for name, raw in (("none", json.dumps({"signals": []})),
                          ("review", checker_raw([Finding("vision_unreadable", "review", "x")])),
                          ("block", checker_raw([Finding("vision_bank_mismatch", "block", "x")]))):
            gov.reset()
            out[name] = gov.evaluate_signed(prop, ex.extract(pdf, {}), raw, hashlib.sha256(pdf).hexdigest(), now=int(time.time()))["verdict"]
    finally:
        gov.close()
        ex.close()
    order = {"allow": 0, "review": 1, "block": 2}
    assert out["block"] == "block"
    assert order[out["review"]] >= max(order[out["none"]], 1)
