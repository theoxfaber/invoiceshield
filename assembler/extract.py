"""Deterministic extraction of the values the governor binds the issuer to.

No model is involved. Values come from the PDF text layer and the email header;
whatever the document claims, the governor compares them with the vendor master.
"""
from __future__ import annotations

import re

from assembler.evidence import Evidence

GSTIN_RE = re.compile(r"(?<![A-Z0-9])\d{2}[A-Z]{5}\d{4}[A-Z][1-9A-Z]Z[0-9A-Z](?![A-Z0-9])")
IFSC_RE = re.compile(r"(?<![A-Z0-9])[A-Z]{4}0[A-Z0-9]{6}(?![A-Z0-9])")
ACCT_RE = re.compile(r"(?<![\d,.])\d{9,18}(?![\d,.])")
BOX_ACCT_RE = re.compile(r"A/?c\.?\s*(?:No\.?|Number)?\s*[:\-]?\s*(\d[\d ]{7,22}\d)", re.I)
BOX_IFSC_RE = re.compile(r"IFSC(?:\s*Code)?\s*[:\-]?\s*([A-Z]{4}0[A-Z0-9]{6})", re.I)


def _uniq(xs):
    seen, out = set(), []
    for x in xs:
        if x not in seen:
            seen.add(x)
            out.append(x)
    return out


def sender_domain(email_from: str) -> str | None:
    m = re.search(r"@([A-Za-z0-9.\-]+)\s*>?\s*$", email_from.strip())
    return m.group(1).lower() if m else None


def governor_evidence(ev: Evidence, buyer_gstin: str) -> dict:
    """Build the `evidence` object of a governor request (field names fixed by the Rust schema)."""
    text = ev.text or ""
    upper = text.upper()
    gstins = [g for g in _uniq(GSTIN_RE.findall(upper)) if g != buyer_gstin.upper()]
    box_acct = BOX_ACCT_RE.search(text)
    box_ifsc = BOX_IFSC_RE.search(text)
    accts = _uniq(ACCT_RE.findall(text))
    out = {
        "has_text_layer": bool(text.strip()),
        "text_layer_gstin": gstins[0] if gstins else None,
        "sender_domain": sender_domain(ev.email_from),
        "printed_bank_account": re.sub(r"\s+", "", box_acct.group(1)) if box_acct else None,
        "printed_ifsc": box_ifsc.group(1).upper() if box_ifsc else None,
        "all_printed_accounts": accts,
        "all_printed_ifscs": _uniq(IFSC_RE.findall(upper)),
    }
    return out


TOTAL_RE = re.compile(r"Total\s+Payable\s*[:\-]?\s*(?:INR|Rs\.?|₹)?\s*([\d,]+(?:\.\d{1,2})?)", re.I)


def printed_total_paise(text: str) -> int | None:
    """The 'Total Payable' figure the document prints (what a naive agent would pay). None if absent."""
    m = TOTAL_RE.findall(text or "")
    if not m:
        return None
    rupees, _, paise = m[-1].replace(",", "").partition(".")
    return int(rupees) * 100 + int((paise + "00")[:2]) if rupees.isdigit() else None
