"""v3-prep assembler fixes (commit 75d5863, 2026-10-31).

Tax-column parsing (v2 case_006 root cause): a "Taxable | CGST | SGST | IGST | Total" summary table
read in text order puts the next figure after "IGST" in the wrong column, so an intra-state invoice
(CGST+SGST charged, IGST 0.00) was read as charging IGST and REVIEWed as tax_type_mismatch. The word
boxes give each head the figure in its own column. Also: invoice-number labels from more layouts.
"""
from types import SimpleNamespace

from assembler.extract import _tax_kinds, tax_kinds
from assembler.fields import printed_invoice_no
from assembler.geometry import tax_heads_words


def W(x, y, t, p=0):
    return (p, x, y, x + len(t) * 5, y + 10, t)


SUMMARY_INTRA = [W(50, 100, "Taxable"), W(150, 100, "CGST"), W(250, 100, "SGST"), W(350, 100, "IGST"), W(450, 100, "Total"),
                 W(50, 115, "10,000.00"), W(150, 115, "900.00"), W(250, 115, "900.00"), W(350, 115, "0.00"), W(450, 115, "11,800.00")]
SUMMARY_TEXT = "Taxable CGST SGST IGST Total\n10,000.00 900.00 900.00 0.00 11,800.00"


def test_text_order_heuristic_misreads_the_column_table():
    # the bug the word-box pass corrects: text order reads IGST as charged
    assert "IGST" in tax_kinds(SUMMARY_TEXT)


def test_word_boxes_read_each_head_in_its_column():
    zero, charged = tax_heads_words(SUMMARY_INTRA)
    assert zero == {"IGST"}
    assert charged == {"CGST", "SGST"}


def test_inter_state_column_table():
    ws = [W(50, 100, "Taxable"), W(150, 100, "CGST"), W(250, 100, "SGST"), W(350, 100, "IGST"),
          W(50, 115, "10,000.00"), W(150, 115, "0.00"), W(250, 115, "0.00"), W(350, 115, "1,800.00")]
    zero, charged = tax_heads_words(ws)
    assert charged == {"IGST"} and zero == {"CGST", "SGST"}


def test_inline_head_with_rate():
    assert tax_heads_words([W(50, 100, "IGST"), W(100, 100, "@"), W(120, 100, "18%"), W(200, 100, "1,800.00")]) == (set(), {"IGST"})


def test_tax_kinds_uses_word_boxes_when_present():
    ev = SimpleNamespace(words=SUMMARY_INTRA)
    assert _tax_kinds(ev, SUMMARY_TEXT) == ["CGST", "SGST"]
    # no word boxes (e.g. OCR text only): the text heuristic is used unchanged
    assert _tax_kinds(SimpleNamespace(words=None), SUMMARY_TEXT) == tax_kinds(SUMMARY_TEXT)


def test_head_followed_by_letters_is_not_a_head():
    assert tax_kinds("IGSTIN 1,800.00") == []


def test_invoice_number_labels():
    assert printed_invoice_no("Bill No: AB/26/0099\nDate 01-10-2026") == "AB/26/0099"
    assert printed_invoice_no("Inv # 4471\n") == "4471"
    assert printed_invoice_no("TAX INVOICE\nAcme Pvt Ltd\n# DSF-1224\nBalance Due") == "DSF-1224"
    # Tally: label line, another label line, then the values in order
    assert printed_invoice_no("Invoice No.\nDated\nTL/118\n12-Oct-2026\n") == "TL/118"
