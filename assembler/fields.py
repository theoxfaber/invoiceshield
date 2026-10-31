"""Candidate readers for fields the governor does not consume yet: invoice number and currency.

They are measured in eval/run_real.py on public invoices; the governor's evidence schema is
unchanged (deny_unknown_fields), so nothing here can change a verdict. The agent still proposes
the invoice number, and currency on proposal/PO/evidence is Phase 1's work (docs/HARDENING.md).
"""
from __future__ import annotations

import re

_INV_LABEL = re.compile(r"\b(?:Tax\s+)?(?:Invoice|Inv|Bill|Document|Doc|Credit\s+Note|Debit\s+Note)\s*(?:No|Number|Num|#)(?![A-Za-z])\s*\.?\s*[:#.]?\s*", re.I)
_INV_LABEL_EOL = re.compile(r"\b(?:Tax\s+)?(?:Invoice|Inv|Bill|Document|Doc|Credit\s+Note|Debit\s+Note)\s*(?:No|Number|Num|#)(?![A-Za-z])[ \t]*[.:#]?[ \t]*\n", re.I)
# Zoho-style header: "TAX INVOICE" ... a line "# DSF-1224"; Tally-style: "Invoice No." / "Dated" / value / date
_HASH_LINE = re.compile(r"^\s*#\s*([A-Za-z0-9][A-Za-z0-9/\-_.]*\d[A-Za-z0-9/\-_.]*)\s*$", re.M)
_DATEISH = re.compile(r"^\d{1,2}[-/. ](?:\d{1,2}|[A-Za-z]{3})[-/. ]\d{2,4}$")
_SEP = {"-", "/", "\\", "–"}
_STOP = re.compile(r"^(?:date[d]?|dt|po|ref|order|e-?way|buyer|gstin|place|party|m/s|challan|due)\b", re.I)


def printed_invoice_no(text: str) -> str | None:
    """The value printed after the first 'Invoice No' label: a token with a digit, joined with
    following separator-linked tokens ('TI-113/19 - 20' -> 'TI-113/19-20'). None when the label is
    followed by another label (column-header layouts) or nothing number-like."""
    for m in _INV_LABEL.finditer(text or ""):
        toks = text[m.end():m.end() + 80].split()
        while toks and toks[0] in {".", ":", "#", "-"}:
            toks.pop(0)
        if not toks or not re.search(r"\d", toks[0]) or _STOP.match(toks[0]):
            continue
        out = [toks[0].rstrip(".,;")]
        i = 1
        while i < len(toks):
            t = toks[i]
            if t in _SEP and i + 1 < len(toks) and not _STOP.match(toks[i + 1]) and len(toks[i + 1]) <= 12:
                out += [t, toks[i + 1].rstrip(".,;")]
                i += 2
                continue
            if out[-1][-1:] in _SEP and len(t) <= 12 and not _STOP.match(t):
                out.append(t.rstrip(".,;"))
                i += 1
                continue
            break
        return "".join(out)
    # Tally layout: the label line, then other label lines ("Dated"), then the values in order
    for m in _INV_LABEL_EOL.finditer(text or ""):
        rest = text[m.end():].split("\n")
        for ln in rest[:4]:
            t = ln.strip()
            if not t or not re.search(r"\d", t):
                continue
            if _DATEISH.match(t) or len(t.split()) > 1:
                break
            return t.rstrip(".,;")
    m = _HASH_LINE.search(text or "")
    if m and re.search(r"(?i)tax\s+invoice|invoice", text[:m.start()]):
        return m.group(1)
    return None


_CUR = {
    "INR": re.compile(r"(?:\bINR\b|₹|\bRs\b\.?|\bRupees?\b)", re.I),
    "USD": re.compile(r"(?:\bUSD\b|US\$|\$)"),
    "EUR": re.compile(r"(?:\bEUR\b|€)"),
    "GBP": re.compile(r"(?:\bGBP\b|£)"),
}


def printed_currency(text: str) -> str | None:
    """The currency the document is written in, by counting currency codes/symbols/words.
    None when fewer than two markers are printed or two currencies tie (a human should look)."""
    counts = {c: len(r.findall(text or "")) for c, r in _CUR.items()}
    best = sorted(counts.items(), key=lambda kv: -kv[1])
    # One marker is not enough: OCR turns stamps and signatures into a stray '$' (seen on real
    # Indian invoices that print no currency at all), so a currency needs two or more markers.
    if best[0][1] < 2 or best[1][1] == best[0][1]:
        return None
    return best[0][0]
