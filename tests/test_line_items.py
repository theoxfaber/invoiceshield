"""Review items 1 + 2 (2026-10-31), end to end through the real extraction service and governor:
an invoice whose item table the deterministic parser cannot read goes to REVIEW
(line_items_unreadable) unless model-proposed lines verify verbatim against the signed text."""
import hashlib
import io
import json
import pathlib

import pytest
from reportlab.lib.pagesizes import A4
from reportlab.pdfgen import canvas

from extractor.line_proposer import needs_model_lines, propose_lines
from providers.llm import ChatResult
from shield.client import BIN, GovernorProc

pytestmark = pytest.mark.skipif(not BIN.exists(), reason="build governor first: cargo build --release")
CLEAR = json.dumps({"signals": []})
NOW = 1_793_000_000


def _pdf(rows, fee=False, columns=False) -> bytes:
    """A layout no deterministic parser reads: no column header, each row printed as one run of text
    ("1. Stationery - A4 paper  100 nos @ 250.00") with the amount at the right, 'Grand Total', no
    'Total Payable'. `columns=True` draws the original Sl/Particulars/Qty/Rate/Amount table, which the
    word-box geometry parser (e53b468) now reads, so it no longer exercises the model path."""
    buf = io.BytesIO()
    c = canvas.Canvas(buf, pagesize=A4)
    y = 800
    for line in ["TAX INVOICE", "Sharma Office Supplies Pvt Ltd", "GSTIN: 24ZZACS4821A1ZX", "Invoice No: SOS/LT/0001",
                 "Bill To: GSTIN 24ZZACB7319B1Z5"] + (["Sl   Particulars                      Qty    Rate      Amount"] if columns else []):
        c.drawString(40, y, line)
        y -= 18
    for i, (desc, qty, rate, amt) in enumerate(rows, 1):
        if columns:
            c.drawString(40, y, str(i))
            c.drawString(70, y, desc)
            c.drawString(300, y, str(qty))
            c.drawString(350, y, rate)
        else:
            c.drawString(40, y, f"{i}. {desc}  {qty} nos @ {rate}")
        c.drawString(430, y, amt)
        y -= 18
    for label, amt in [("Sub Total", "30,000.00"), ("CGST @9%", "2,700.00"), ("SGST @9%", "2,700.00"), ("Grand Total", "35,400.00")]:
        c.drawString(300, y, label)
        c.drawString(430, y, amt)
        y -= 18
    c.drawString(40, y, "Amount in words: Rupees Thirty Five Thousand Four Hundred Only")
    c.save()
    return buf.getvalue()


ROWS = [("Stationery - A4 paper", 100, "250.00", "25,000.00"), ("Stationery - pens", 50, "100.00", "5,000.00")]
ROWS_FEE = [("Stationery - A4 paper", 100, "250.00", "25,000.00"), ("Urgent handling fee", 1, "5,000.00", "5,000.00")]
PROP = {"vendor_id": "V001", "invoice_no": "SOS/LT/0001", "amount_paise": 35_400_00, "po_ref": "PO-2026-0101"}
GOOD = [
    {"description": "Stationery - A4 paper", "amount_paise": 25_000_00, "qty": 100, "unit_price_minor": 250_00, "is_tax": False},
    {"description": "Stationery - pens", "amount_paise": 5_000_00, "qty": 50, "unit_price_minor": 100_00, "is_tax": False},
    {"description": "CGST @9%", "amount_paise": 2_700_00, "is_tax": True},
    {"description": "SGST @9%", "amount_paise": 2_700_00, "is_tax": True},
]


@pytest.fixture(scope="module")
def gov():
    with GovernorProc() as g:
        yield g


def rules(d):
    return [r["rule"] for r in d["reasons"]]


def _run(gov, doc, lines):
    gov.reset()
    env = gov.extractor.extract(doc, None)
    ev = json.loads(env["payload"])["evidence"]
    d = gov.evaluate_signed(PROP, env, CLEAR, hashlib.sha256(doc).hexdigest(), now=NOW, proposed_item_lines=lines)
    return ev, d


def test_parser_reads_nothing_on_this_layout_and_the_gate_fails_closed(gov):
    ev, d = _run(gov, _pdf(ROWS), None)
    assert ev["item_lines"] == [] and ev["printed_total_paise"] == 35_400_00 and needs_model_lines(ev)
    assert "Stationery - A4 paper" in ev["text_layer"]
    assert d["verdict"] == "review" and "line_items_unreadable" in rules(d), d["reasons"]


def test_column_layout_is_now_read_by_the_geometry_parser(gov):
    ev, d = _run(gov, _pdf(ROWS, columns=True), None)
    assert [ln["amount_paise"] for ln in ev["item_lines"]] == [25_000_00, 5_000_00, 2_700_00, 2_700_00]
    assert "line_items_unreadable" not in rules(d), d["reasons"]


def test_verified_model_lines_let_the_gate_check_and_pay(gov):
    _, d = _run(gov, _pdf(ROWS), GOOD)
    assert d["verdict"] == "allow", d["reasons"]
    assert "line_items_model_verified" in rules(d)
    assert d["payee"]["beneficiary_id"] == "BENE_V001"


def test_invented_model_lines_review(gov):
    bad = [dict(GOOD[0], amount_paise=26_000_00, unit_price_minor=260_00), *GOOD[1:]]
    _, d = _run(gov, _pdf(ROWS), bad)
    assert d["verdict"] == "review" and "line_items_unreadable" in rules(d)


def test_padded_fee_read_honestly_is_caught_by_the_po_rule(gov):
    honest = [GOOD[0], {"description": "Urgent handling fee", "amount_paise": 5_000_00, "qty": 1, "unit_price_minor": 5_000_00,
                        "is_tax": False}, *GOOD[2:]]
    _, d = _run(gov, _pdf(ROWS_FEE), honest)
    assert d["verdict"] == "review" and "charge_not_on_po" in rules(d), d["reasons"]


def test_fooled_model_hiding_the_fee_is_not_verified(gov):
    hidden = [{"description": "Stationery - A4 paper", "amount_paise": 30_000_00, "is_tax": False}, *GOOD[2:]]
    _, d = _run(gov, _pdf(ROWS_FEE), hidden)
    assert d["verdict"] == "review" and "line_items_unreadable" in rules(d), d["reasons"]


def test_malformed_proposed_lines_review(gov):
    gov.reset()
    doc = _pdf(ROWS)
    env = gov.extractor.extract(doc, None)
    d = gov.evaluate_signed(PROP, env, CLEAR, hashlib.sha256(doc).hexdigest(), now=NOW,
                            proposed_item_lines=[{"description": "x", "amount_paise": "lots"}])
    assert d["verdict"] == "review" and rules(d) == ["request_unparseable"]


class _Fake:
    name = "fake"

    def __init__(self, content=None, error=None):
        self.content, self.error = content, error

    def chat(self, messages, **kw):
        assert "untrusted" in messages[1]["content"]
        return ChatResult(content=self.content or "", error=self.error)


def test_proposer_parses_strict_json():
    out = json.dumps({"lines": [{"description": "Stationery - A4 paper", "amount_minor": 2500000, "qty": 100,
                                 "unit_price_minor": 25000, "is_tax": False}]})
    p = propose_lines(_Fake("<think>x</think>" + out), "text")
    assert p.ok and p.lines == [{"description": "Stationery - A4 paper", "amount_paise": 2500000, "qty": 100,
                                 "unit_price_minor": 25000, "is_tax": False}]


@pytest.mark.parametrize("content", ["", "not json", '{"lines": "x"}', '{"lines": [{"description": "a", "amount_minor": 1.5}]}',
                                     '{"lines": [], "extra": 1}', '[1]'])
def test_proposer_fails_closed_on_bad_output(content):
    p = propose_lines(_Fake(content), "text")
    assert not p.ok and p.lines == []


def test_proposer_provider_error_is_no_lines():
    p = propose_lines(_Fake(error="timeout"), "text")
    assert not p.ok and p.lines == [] and "timeout" in p.error


def test_needs_model_lines():
    assert needs_model_lines({"has_text_layer": True, "printed_total_paise": 1, "item_lines": []})
    assert not needs_model_lines({"has_text_layer": True, "printed_total_paise": None, "item_lines": []})
    assert not needs_model_lines({"has_text_layer": True, "printed_total_paise": 1, "item_lines": [{"x": 1}]})
    assert pathlib.Path(__file__).exists()
