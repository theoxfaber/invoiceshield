"""Red-team R6: look-alike characters in bank fields reach the governor as printed (DECISIONS 22:10 IST)."""
from assembler.evidence import Evidence
from assembler.extract import confusable_bank_tokens, fold_confusables, governor_evidence

BUYER = "24ZZACK0000K1Z0"


def test_cyrillic_ifsc_is_reported_as_printed():
    text = "Bank: Bank of Baroda\nA/c No: 23276810695553\nIFSC: BARB0MAKVAD\nIFSC: \u0412ARB0MAKVAD\n"
    ifscs, accts = confusable_bank_tokens(text)
    assert ifscs == ["\u0412ARB0MAKVAD"]
    assert accts == []
    ev = governor_evidence(Evidence("", 1, text, {"text_source": "text_layer"}), BUYER)
    assert "\u0412ARB0MAKVAD" in ev["all_printed_ifscs"]
    assert "BARB0MAKVAD" in ev["all_printed_ifscs"]


def test_cyrillic_o_in_zero_slot_is_an_ifsc():
    ifscs, _ = confusable_bank_tokens("IFSC Code: SBIN\u041e001234")
    assert ifscs == ["SBIN\u041e001234"]


def test_fullwidth_account_in_bank_context_is_reported():
    _, accts = confusable_bank_tokens("Account No: \uff12\uff13\uff12\uff17\uff16\uff18\uff11\uff10\uff16\uff19\uff15\uff15\uff15\uff13")
    assert len(accts) == 1 and not accts[0].isascii()


def test_fullwidth_phone_is_not_an_account():
    _, accts = confusable_bank_tokens("Ph: \uff19\uff18\uff17\uff16\uff15\uff14\uff13\uff12\uff11\uff10")
    assert accts == []


def test_plain_ascii_text_is_unchanged():
    text = "A/c No: 23276810695553\nIFSC: BARB0MAKVAD\nTotal Payable: 23600.00"
    assert confusable_bank_tokens(text) == ([], [])


def test_fold_is_never_used_as_a_value():
    # the fold only decides the shape; the evidence carries the original characters
    assert fold_confusables("\u0412ARB0MAKVAD") == "BARB0MAKVAD"
    ev = governor_evidence(Evidence("", 1, "IFSC: \u0412ARB0MAKVAD", {"text_source": "text_layer"}), BUYER)
    assert ev["all_printed_ifscs"] == ["\u0412ARB0MAKVAD"]
