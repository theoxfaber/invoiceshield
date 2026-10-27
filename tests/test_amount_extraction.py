"""Item-table and total extraction (deterministic) against generator output."""
import pathlib

from assembler.evidence import assemble
from assembler.extract import governor_evidence, item_lines, printed_total_paise
from dataset.gen.make_invoice import make_t1
from shield.pipeline import BUYER_GSTIN


def _ev(tmp_path, **kw):
    m = make_t1("V006", "NITS-2026-900", "PO-2026-0601", kw.pop("amount", 4720000), tmp_path / "i.pdf", **kw)
    return assemble(m["pdf"], m["cover_email"])


def test_clean_invoice_lines_add_up_to_the_printed_total(tmp_path):
    ev = _ev(tmp_path)
    rows = item_lines(ev.text)
    assert [r["is_tax"] for r in rows] == [False, True]
    assert sum(r["amount_paise"] for r in rows) == printed_total_paise(ev.text) == 4720000


def test_extra_fee_line_is_a_non_tax_row(tmp_path):
    ev = _ev(tmp_path, amount=7040000, extra_lines=[("Expedite / priority handling fee", 35000.0)])
    rows = item_lines(ev.text)
    fee = [r for r in rows if not r["is_tax"] and "Expedite" in r["description"]]
    assert len(fee) == 1 and fee[0]["amount_paise"] == 3500000


def test_inflated_printed_total_breaks_the_sum(tmp_path):
    ev = _ev(tmp_path, amount=4800000, shown_total_paise=8400000)
    assert sum(r["amount_paise"] for r in item_lines(ev.text)) == 4800000
    assert printed_total_paise(ev.text) == 8400000


def test_governor_evidence_carries_the_new_fields(tmp_path):
    g = governor_evidence(_ev(tmp_path), BUYER_GSTIN)
    assert g["printed_total_paise"] == 4720000 and len(g["item_lines"]) == 2


def test_no_table_means_no_rows():
    assert item_lines("just a photo caption, nothing tabular") == []
    assert printed_total_paise("nothing") is None
