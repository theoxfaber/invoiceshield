"""Column-header item-table parser (assembler/extract.py::header_table_lines), 2026-10-31.

Synthetic texts only (fictional vendors); the layout mirrors a common Indian GST table with one
cell per text line, which is how the signed text layer of such PDFs comes out."""
from assembler.extract import header_table_lines, item_lines

HDR = "Sl\nDescription\nHSN/SAC\nQty\nUoM\nRate\nGST%\nTaxable Value\n"


def doc(rows: str, summary: str) -> str:
    return "TAX INVOICE\nInvoice No.\nX/1\n" + HDR + rows + "Tax summary\n@18%: taxable 9,000.00 | IGST 18% 1,620.00\n" + summary


ROWS = ("1\nCorrugated box 5-ply\n4819\n100\nNOS\n50.00\n18\n5,000.00\n"
        "2\nPrinted label A6\n4821\n1000\nNOS\n4.00\n18\n4,000.00\n")
SUMMARY = "Taxable Total\n₹ 9,000.00\nIGST\n₹ 1,620.00\nGRAND TOTAL\n₹ 10,620.00\nAmount in words: Rupees Ten Thousand Six Hundred Twenty Only\n"


def test_reads_rows_taxes_and_rates():
    got = header_table_lines(doc(ROWS, SUMMARY))
    assert [(r["description"], r["amount_paise"], r["is_tax"]) for r in got] == [
        ("Corrugated box 5-ply", 500000, False), ("Printed label A6", 400000, False), ("IGST", 162000, True)]
    assert got[0]["qty"] == 100 and got[0]["unit_price_minor"] == 5000
    assert item_lines(doc(ROWS, SUMMARY)) == got  # used when the original parser reads nothing


def test_fee_in_the_summary_block_is_a_charge_line():
    s = "Taxable Total\n₹ 9,000.00\nHandling charges\n₹ 500.00\nIGST\n₹ 1,620.00\nGRAND TOTAL\n₹ 11,120.00\n"
    got = header_table_lines(doc(ROWS, s))
    fee = [r for r in got if r["description"] == "Handling charges"]
    assert fee and fee[0]["amount_paise"] == 50000 and not fee[0]["is_tax"]


def test_lines_not_adding_up_to_the_total_read_nothing():
    s = SUMMARY.replace("10,620.00", "11,620.00").replace("Ten Thousand", "Eleven Thousand")
    assert header_table_lines(doc(ROWS, s)) == []


def test_skipped_note_that_is_a_real_charge_fails_closed():
    rows = ROWS + "Freight extra: 700.00\n"
    s = SUMMARY.replace("10,620.00", "11,320.00").replace("Ten Thousand Six Hundred Twenty", "Eleven Thousand Three Hundred Twenty")
    assert header_table_lines(doc(rows, s)) == []


def test_unpaired_summary_line_reads_nothing():
    s = "Taxable Total\n₹ 9,000.00\nIGST\nsee annexure\n₹ 1,620.00\nGRAND TOTAL\n₹ 10,620.00\n"
    assert header_table_lines(doc(ROWS, s)) == []


def test_subtotal_must_match_the_rows():
    s = SUMMARY.replace("Taxable Total\n₹ 9,000.00", "Taxable Total\n₹ 8,000.00")
    assert header_table_lines(doc(ROWS, s)) == []


def test_page_break_with_repeated_header():
    rows = ("1\nCorrugated box 5-ply\n4819\n100\nNOS\n50.00\n18\n5,000.00\n... continued on next page\nPage 2 of 2\n"
            "X/1 - continued from page 1\n" + HDR + "2\nPrinted label A6\n4821\n1000\nNOS\n4.00\n18\n4,000.00\n")
    got = header_table_lines(doc(rows, SUMMARY))
    assert [r["amount_paise"] for r in got] == [500000, 400000, 162000]


def test_no_total_or_no_header_reads_nothing():
    assert header_table_lines("Sl\nDescription\nAmount\n1\nWidget\n100.00\n") == []
    assert header_table_lines("TAX INVOICE\nGRAND TOTAL\n₹ 100.00\n") == []
