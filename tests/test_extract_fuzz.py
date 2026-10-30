"""Property-based fuzzing (hypothesis) of the deterministic extraction functions and of the
sandboxed extractor on mutated PDFs. CI-sized by default; HYPOTHESIS_PROFILE=long for more."""
import os
import pathlib
import re

from hypothesis import HealthCheck, given, settings, strategies as st

from assembler.evidence import Evidence
from assembler import extract as X
from extractor.sandbox import run_sandboxed

settings.register_profile("ci", max_examples=150, deadline=None, suppress_health_check=[HealthCheck.too_slow])
settings.register_profile("long", max_examples=5000, deadline=None, suppress_health_check=[HealthCheck.too_slow])
settings.load_profile(os.environ.get("HYPOTHESIS_PROFILE", "ci"))

ROOT = pathlib.Path(__file__).resolve().parents[1]
C01 = (ROOT / "dataset" / "clean" / "C01.pdf").read_bytes()

# Invoice-flavoured text: real labels, digit runs, IFSC/GSTIN shapes, unicode noise.
TOKENS = st.sampled_from([
    "A/c No:", "Account Number", "IFSC:", "IFSC Code", "Bank:", "Beneficiary", "Ph:", "GSTIN:", "PO", "Invoice No",
    "Ref", "UTR", "Total Payable", "CGST", "SGST", "Round off", "₹", "Rs.", "USD", "$", "INR", "\n", "  ", ":", "|",
    "BARB0MAKVAD", "CNRB0002778", "24ZZACS4821A1ZX", "\u200b", "\u00a0", "SWIFT", "pay to", "NEFT",
])
DIGITS = st.from_regex(r"\d{1,20}([.,]\d{1,2})?", fullmatch=True)
TEXT = st.lists(st.one_of(TOKENS, DIGITS, st.text(max_size=12)), max_size=60).map(" ".join)


@given(TEXT)
def test_printed_accounts_total_and_sound(text):
    accts = X.printed_accounts(text)
    assert isinstance(accts, list) and len(accts) == len(set(accts))
    for a in accts:
        assert re.fullmatch(r"\d{9,18}", a), a
        # printed as one run, or digit-grouped after an account label ("2327 6810 6955 53")
        assert a in text or re.search(r"\s*".join(a), text), a


@given(TEXT)
def test_amount_parsers_never_raise(text):
    t = X.printed_total_paise(text)
    assert t is None or isinstance(t, int)
    for line in X.item_lines(text):
        assert isinstance(line["description"], str) and isinstance(line["amount_paise"], int) and isinstance(line["is_tax"], bool)


@given(st.text(max_size=80))
def test_sender_domain_never_raises(s):
    d = X.sender_domain(s)
    assert d is None or (d == d.lower() and "@" not in d)


@given(TEXT, st.text(max_size=40))
def test_governor_evidence_matches_rust_schema(text, sender):
    g = X.governor_evidence(Evidence("h", 1, text, email_from=sender), "24ZZACB7319B1Z5")
    assert set(g) >= {"has_text_layer", "text_source", "text_layer_gstin", "sender_domain", "printed_bank_account",
                      "printed_ifsc", "all_printed_accounts", "all_printed_ifscs", "printed_total_paise", "item_lines"}
    assert g["text_layer_gstin"] != "24ZZACB7319B1Z5"


@settings(max_examples=int(os.environ.get("SANDBOX_FUZZ_EXAMPLES", "12")), deadline=None)
@given(st.lists(st.tuples(st.integers(0, len(C01) - 1), st.integers(0, 255)), min_size=1, max_size=40),
       st.integers(len(C01) // 3, len(C01)))
def test_sandbox_survives_mutated_pdfs(flips, cut):
    """Byte flips + truncation of a real invoice: always a result, never an exception or hang."""
    b = bytearray(C01[:cut])
    for i, v in flips:
        if i < len(b):
            b[i] = v
    r = run_sandboxed(bytes(b), {}, "24ZZACB7319B1Z5")
    assert r.ok or r.failure
    assert r.info.get("wall_s", 0) < 65
    if r.ok:
        assert isinstance(r.evidence, dict)
