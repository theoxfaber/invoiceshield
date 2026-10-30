"""Review item 3 (2026-10-31): extractor reads GST heads and credit-note titles."""
from assembler.extract import document_kind, tax_kinds


def test_tax_heads_need_a_nonzero_amount():
    assert tax_kinds("CGST 9%\n₹ 4,365.00\nSGST 9%\n₹ 4,365.00\nIGST\n0.00") == ["CGST", "SGST"]
    assert tax_kinds("IGST @18%: 8,730.00") == ["IGST"]
    assert tax_kinds("IGST not applicable (intra-state)") == []
    assert tax_kinds("UTGST 9% 450.00 CGST 9% 450.00") == ["CGST", "UTGST"]
    assert tax_kinds("") == []


def test_percent_is_not_an_amount():
    assert tax_kinds("IGST 18.00 %") == []


def test_credit_note_title():
    assert document_kind("CREDIT NOTE\nOriginal for recipient") == "credit_note"
    assert document_kind("  Credit Note No. CN/19\n") == "credit_note"
    assert document_kind("TAX INVOICE\nRef: credit note CN/1 adjusted") == "invoice"
    assert document_kind("") == "invoice"
