"""Extraction changes driven by the public real-invoice run (docs/FINDINGS_REAL_INVOICES.md).

Every identifier here is fictional (ZZ PANs, checksums computed with structural.gstin); the
patterns copy what OCR'd real invoices look like, not their data.
"""
from assembler.extract import (classify_number, issuer_gstins, printed_accounts, printed_total_paise,
                               words_confirmed_total_paise, words_to_int)
from assembler.fields import printed_currency, printed_invoice_no
from eval.run_real import wilson
from structural.gstin import repair_ocr, validate

SUP = "27ZZAFQ1234K1Z9"   # supplier (valid checksum)
BUY = "27ZZBFR5678L1ZH"   # buyer (valid checksum)


def test_fixtures_are_valid():
    assert validate(SUP) == [] and validate(BUY) == []


def test_repair_maps_class_impossible_characters():
    assert repair_ocr("27ZZAFQ1234K1Z9") == SUP           # valid: unchanged
    assert repair_ocr("27ZZAFQ12J4K1Z9") is None           # J in a digit slot has no mapping
    assert repair_ocr("27ZZAFQI234K1Z9") == SUP           # I in a digit slot -> 1
    assert repair_ocr("27ZZAFQ1234KIZ9") == SUP           # entity slot I/1 flipped only to fix the checksum
    assert repair_ocr("27ZZAFQ1234K1Z8") is None           # wrong check character stays wrong


def test_ocr_copy_of_the_buyer_is_not_taken_for_the_issuer():
    text = f"Buyer GSTIN {BUY[:12]}I{BUY[13:]} Seller GSTIN {SUP}"
    assert issuer_gstins(text, BUY) == [SUP]


def test_gstin_split_after_state_code_and_ocr_slips_are_read():
    assert issuer_gstins(f"GSTIN No . : 27 {SUP[2:]}", BUY) == [SUP]
    assert issuer_gstins("GST No . : 27ZZAFQ1234KIZ9", BUY) == [SUP]


def test_strict_gstin_with_bad_checksum_is_still_reported():
    # a forged/mistyped GSTIN that no repair can fix is kept as printed, so the governor still
    # sees a GSTIN that differs from the master (BLOCK), exactly as before
    assert issuer_gstins("GSTIN 27ZZAFQ1234K1Z8", BUY) == ["27ZZAFQ1234K1Z8"]


def test_words_to_int_indian_scale_and_common_misspellings():
    assert words_to_int("Two Lakh Thirty Three Thousand Four Hundred Four") == 233404
    assert words_to_int("One Lakh Ninty Nine Thousand Four Hundred Twenty") == 199420
    assert words_to_int("Twenty Nine Thousand Seven Hundred Fifty Six") == 29756


def test_total_from_words_needs_matching_figures():
    t = "Bill Amount : Two Lakh Thirty Three Thousand Four Hundred Four Only Grand Total : 2,33,404.00"
    assert printed_total_paise(t) == 23340400
    assert words_confirmed_total_paise("Amount in words Two Lakh Only. Grand Total 1,99,999.00") is None


def test_total_from_words_skips_paise_lines_and_prefers_the_largest():
    t = ("Tax Amount ( in words ) : Indian Rupees Four Thousand Five Hundred Thirty Nine and Eight paise Only "
         "Amount Chargeable ( in words ) Indian Rupees Twenty Nine Thousand Seven Hundred Fifty Six Only "
         "4,539.08 29,756 Total GST : Four Thousand Five Hundred Thirty Nine Only 4539")
    assert printed_total_paise(t) == 2975600


def test_total_payable_still_wins():
    t = "Description Amount\nWidget 100.00\nTotal Payable: INR 100.00\nRupees Two Hundred Only 200.00"
    assert printed_total_paise(t) == 10000


def test_mobile_next_to_an_address_bank_word_is_not_an_account():
    t = "Sr . No;-8 Near Bank of Maharashtra Mayur Colony Kothrud Pune.9876501234 9123456780.Po No;-605"
    assert printed_accounts(t) == []


def test_labelled_or_ifsc_backed_mobile_shaped_number_is_still_an_account():
    assert printed_accounts("Please pay to 9876501234 today") == ["9876501234"]
    assert printed_accounts("Beneficiary 9876501234 IFSC HDFC0001234") == ["9876501234"]


def test_vat_tin_and_cst_numbers_are_not_accounts():
    t = "Bank Name : KOTAK MAHINDRA BANK Company 's VAT TIN : 27110000018 V Buyer 's CST No . : 27950000079C"
    assert printed_accounts(t) == []
    s = "Company 's VAT TIN : 27110000018"
    i = s.index("27110000018")
    assert classify_number(s, i, i + 11) == "other"


def test_invoice_number_reader():
    assert printed_invoice_no("Invoice No . : TI-113/19 - 20 Date : 05-04-2019") == "TI-113/19-20"
    assert printed_invoice_no("Invoice No . SM/19 - 20/001000 Delivery Note") == "SM/19-20/001000"
    assert printed_invoice_no("Invoice No . 03459/17 - 18 / GST Ref . No .") == "03459/17-18/GST"
    assert printed_invoice_no("Invoice no: 97159829\nDate of issue") == "97159829"
    assert printed_invoice_no("Invoice No Invoice Date Order No") is None


def test_currency_reader_needs_two_markers():
    assert printed_currency("Total $ 889,20 $ 88,92 $ 978,12") == "USD"
    assert printed_currency("In INR 27,950 ... Rupees Only") == "INR"
    assert printed_currency("RECEIVED SIGN : G DATE:- $ 18119") is None   # OCR noise from a stamp
    assert printed_currency("no currency here") is None


def test_wilson_interval():
    lo, hi = wilson(0, 60)
    assert lo == 0.0 and 0.05 < hi < 0.07
    lo, hi = wilson(29, 578)
    assert lo < 29 / 578 < hi
