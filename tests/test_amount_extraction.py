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


def test_usd_vendor_template_extracts_and_adds_up(tmp_path):
    m = make_t1("V003", "BC-INV-10077", "PO-2026-0301", 1200000, tmp_path / "u.pdf")
    ev = assemble(m["pdf"], m["cover_email"])
    assert "USD" in ev.text and "CGST" not in ev.text
    rows = item_lines(ev.text)
    assert sum(r["amount_paise"] for r in rows) == printed_total_paise(ev.text) == 1200000
    ge = governor_evidence(ev, BUYER_GSTIN)
    assert ge["text_layer_gstin"] is None and ge["printed_bank_account"] == "754155265497"


# Multi-column item tables (Description | HSN | Qty x Rate | Amount) come out of the text layer
# one cell per line. Found by the clean-invoice run (Oct 30): the old parser read "12 x 126.52"
# as a row and dropped the real amount. The amount column closes a row.
def test_multi_column_item_table():
    text = ("Description\nHSN/SAC\nQty x Rate\nAmount (INR)\n"
            "base frames welded and painted (Fabrication)\n7308\n12 x 126.52\n1,518.24\n"
            "SS 304 guard covers (Fabrication)\n7308\n1 lot\n49,089.92\n"
            "IGST @ 12%\n6,072.97\nRound off\n-0.13\nTotal Payable Rs.\n56,681.00\n")
    rows = item_lines(text)
    assert [r["amount_paise"] for r in rows] == [151824, 4908992, 607297, -13]
    assert "Fabrication" in rows[0]["description"] and "Fabrication" in rows[1]["description"]
    assert rows[2]["is_tax"] and not rows[3]["is_tax"]
    assert sum(r["amount_paise"] for r in rows) == printed_total_paise(text)


def test_wrapped_description_with_amount_on_second_line():
    text = "Description\nAmount (INR)\nFreight for Vadodara to\nVizag FTL 12,000.00\nIGST @ 18%\n2,160.00\nTotal Payable\n14,160.00\n"
    rows = item_lines(text)
    assert [(r["description"], r["amount_paise"]) for r in rows] == [("Freight for Vadodara to Vizag FTL", 1200000), ("IGST @ 18%", 216000)]
