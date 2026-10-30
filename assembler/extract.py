"""Deterministic extraction of the values the governor binds the issuer to.

No model is involved. Values come from the PDF text layer and the email header;
whatever the document claims, the governor compares them with the vendor master.
"""
from __future__ import annotations

import re

from assembler.evidence import Evidence

GSTIN_RE = re.compile(r"(?<![A-Z0-9])\d{2}[A-Z]{5}\d{4}[A-Z][1-9A-Z]Z[0-9A-Z](?![A-Z0-9])")
IFSC_RE = re.compile(r"(?<![A-Z0-9])[A-Z]{4}0[A-Z0-9]{6}(?![A-Z0-9])")
# "A/c No", "Current A/c No", "Account No.", "Account Number"; digits may be printed in groups.
BOX_ACCT_RE = re.compile(r"\b(?:A/?c|Account)\.?\s*(?:No\.?|Number)?\s*[:\-]?\s*(\d[\d ]{7,22}\d)", re.I)
BOX_IFSC_RE = re.compile(r"IFSC(?:\s*Code)?\s*[:\-]?\s*([A-Z]{4}0[A-Z0-9]{6})", re.I)


def _uniq(xs):
    seen, out = set(), []
    for x in xs:
        if x not in seen:
            seen.add(x)
            out.append(x)
    return out


# --- Account numbers: context-aware ------------------------------------------------------------
# A bare run of 9-18 digits is not evidence of a bank account: invoices print phone numbers,
# IRN/Ack numbers, PO and order references, pincodes and UTRs in the same shape. A digit run
# counts as an account only when the text around it says so:
#   * the nearest label before it (same line, or the previous line when the number opens the
#     line) is a bank label (A/c, Account, Acct, Beneficiary, Bank, IFSC, SWIFT, remit, pay to...);
#   * or the nearest label is a non-bank one (Ph, GSTIN, IRN, Ack, PO, Invoice No, Ref, UTR...) but
#     an IFSC/SWIFT code follows on the same line ("Ref 3269010273503 IFSC CNRB0002778" is a
#     payee in disguise, not a reference);
#   * or there is no label at all and a bank/payment word or an IFSC/SWIFT code is close by.
# The bank box is labelled, so it is always captured; text pasted anywhere else that names an
# account in bank words, or next to an IFSC, is captured too, so the tamper alert for injected
# payees keeps working. Residual gap (documented): a pasted account disguised under a non-bank
# label with no IFSC beside it raises no alert; it still cannot be paid, because the payee
# comes only from the vendor master.
ACCT_RE = re.compile(r"(?<![0-9A-Za-z])(?<!\d[.,])\d{9,18}(?![0-9A-Za-z])(?![.,]\d)")
# OCR reads "A/c" as "Alc", "A1c", "A|c", so the slash slot is loose but never empty
# (a plain "AC" is usually air conditioning).
_BANK_LABEL = (r"\b(?:a\s?[/\\l1|]\s?c\b\.?|acct\b|accnt\b|account(?:[_\s]?(?:no|number|num))?\b|beneficiary|bank\b"
               r"|ifsc|swift|bic\b|iban|remit\w*|pay(?:\s+to)?\b|payment\s+to|transfer\b|credit\s+(?:to|the)\b"
               r"|deposit|neft|rtgs|imps|wire\b)")
_OTHER_LABEL = (r"\b(?:ph\b\.?|phone|tel\b\.?|telephone|mob\b\.?|mobile|cell\b|fax|whats\s?app|contact|call\b|toll[\s-]?free|helpline"
                r"|gstin|gst\s*no|pan\b|cin\b|tan\b|udyam|msme|irn\b|ack\w*|e-?way|ewb\b|lut\b|hsn\b|sac\b"
                r"|p\.?\s?o\b\.?|purchase\s+order|order\b|invoice|inv\b\.?|bill\s*no|challan|dc\s*no"
                r"|ref\w*|utr\b|txn\b|transaction|cheque|chq\b|dd\s*no|receipt|voucher|grn\b|docket|awb\b|lr\s*no|tracking|consignment"
                r"|pin\b|pin\s*code|pincode|zip\b|postal|routing|aba\b|micr\b|sort\s*code|branch\s*code|serial|s/?n\b|imei|licen[cs]e|ticket|case\b)")
_LABEL_RE = re.compile(rf"(?P<bank>{_BANK_LABEL})|(?P<other>{_OTHER_LABEL})", re.I)
_BANK_WORD_RE = re.compile(_BANK_LABEL, re.I)
# "<label words> [No|ID|Code|Number|#]:" right before the number, on its own line or after a gap.
_FIELD_RE = re.compile(r"(?:^|\s{2,}|[|;,]\s*)([A-Za-z][A-Za-z0-9 .&/()'#-]{0,40}?)\s*[:#]\s*$")
_SWIFT_RE = re.compile(r"\b(?:SWIFT|BIC)\s*(?:code)?\s*[:\-]?\s*[A-Z]{6}[A-Z0-9]{2}(?:[A-Z0-9]{3})?\b", re.I)


def _line_start(text: str, i: int) -> int:
    return text.rfind("\n", 0, i) + 1


def _bank_code_in(seg: str) -> bool:
    return bool(IFSC_RE.search(seg.upper()) or _SWIFT_RE.search(seg))


def classify_number(text: str, start: int, end: int) -> str:
    """'account' or 'other' for the digit run text[start:end] (rules in the block comment above)."""
    ls = _line_start(text, start)
    if not text[ls:start].strip() and ls > 0:
        ls = _line_start(text, ls - 1)  # a number that opens its line takes the previous line's label
    le = text.find("\n", end)
    code_after = _bank_code_in(text[end:(le if le != -1 else len(text))][:40])
    # A number printed as the value of its own field ("Batch No: ...", "Customer ID: ...") takes
    # that field's label only, whatever words it uses; bank words inside the label still win.
    field = _FIELD_RE.search(text[ls:start].replace("\n", " "))
    if field is not None:
        labels = list(_LABEL_RE.finditer(field.group(0)))
        if labels and labels[-1].group("bank"):
            return "account"
        return "account" if code_after else "other"
    labels = list(_LABEL_RE.finditer(text[max(ls, start - 48):start]))
    nearest = labels[-1] if labels else None
    if nearest is not None and nearest.group("bank"):
        return "account"
    if nearest is not None:
        return "account" if code_after else "other"
    if _BANK_WORD_RE.search(text[max(0, start - 80):start]) or _bank_code_in(text[max(0, start - 40):end + 40]):
        return "account"
    return "other"


def printed_accounts(text: str) -> list[str]:
    """Every account number the document prints in a bank context (bank box or anywhere else)."""
    text = text or ""
    found = [m.group() for m in ACCT_RE.finditer(text) if classify_number(text, m.start(), m.end()) == "account"]
    # grouped digits ("2327 6810 6955 53") after an explicit account label anywhere in the text
    # (joined, it must still be an account-length number: 9-18 digits, like ACCT_RE)
    found += [j for m in BOX_ACCT_RE.finditer(text) if " " in m.group(1).strip()
              for j in [re.sub(r"\s+", "", m.group(1))] if 9 <= len(j) <= 18]
    return _uniq(found)


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
    accts = printed_accounts(text)
    out = {
        "has_text_layer": bool(text.strip()),
        "text_source": (ev.metadata or {}).get("text_source"),
        "text_layer_gstin": gstins[0] if gstins else None,
        "sender_domain": sender_domain(ev.email_from),
        "printed_bank_account": re.sub(r"\s+", "", box_acct.group(1)) if box_acct else None,
        "printed_ifsc": box_ifsc.group(1).upper() if box_ifsc else None,
        "all_printed_accounts": accts,
        "all_printed_ifscs": _uniq(IFSC_RE.findall(upper)),
        "printed_total_paise": printed_total_paise(text),
        "item_lines": item_lines(text),
        "currency": doc_currency(text),
        "all_currencies": currencies_seen(text),
    }
    return out


# --- Currency ------------------------------------------------------------------------------------
# Every marker maps to an ISO 4217 code. The governor compares the document's currency with the
# proposal's and the PO's, so a dollar invoice can never be read as paise against a rupee PO.
_CUR_MARKERS = [
    ("INR", r"₹|\bRs\.?(?![A-Za-z])|\bINR\b|\bRupees?\b"),
    ("USD", r"US\$|\bUSD\b|\bUS\s+dollars?\b|(?<![A-Za-z])\$"),
    ("EUR", r"€|\bEUR\b|\beuros?\b"),
    ("GBP", r"£|\bGBP\b|\bpounds?\s+sterling\b"),
    ("AED", r"\bAED\b|\bdirhams?\b"),
    ("SGD", r"S\$|\bSGD\b"),
]
_CUR_RES = [(c, re.compile(rx, re.I)) for c, rx in _CUR_MARKERS]
# Where a document states the currency of its amounts outright.
_CUR_HEADER_RE = re.compile(r"(?:Amount|Total\s+Payable)[^\n]{0,12}?(₹|Rs\.?|INR|USD|US\$|\$|EUR|€|GBP|£|AED|SGD|S\$)", re.I)


def _code(marker: str) -> str | None:
    for code, rx in _CUR_RES:
        if rx.fullmatch(marker.strip()) or rx.match(marker.strip()):
            return code
    return None


def currencies_seen(text: str) -> list[str]:
    """Every currency with a marker anywhere in the document text (sorted, unique)."""
    text = text or ""
    # "US$" also contains "$" and "S$" patterns; strip the longer markers first so each counts once.
    seen = set()
    for code, rx in _CUR_RES:
        if code == "SGD":
            if re.search(r"(?<![U])S\$|\bSGD\b", text):
                seen.add(code)
            continue
        if rx.search(text):
            seen.add(code)
    return sorted(seen)


def doc_currency(text: str) -> str | None:
    """The currency the document's amounts are in: the amount-column / total header when it names
    one, else the only currency marked anywhere, else None (the governor then assumes INR)."""
    m = _CUR_HEADER_RE.search(text or "")
    if m:
        c = _code(m.group(1))
        if c:
            return c
    seen = currencies_seen(text)
    return seen[0] if len(seen) == 1 else None


TOTAL_RE = re.compile(r"Total\s+Payable\s*[:\-]?\s*(?:INR|USD|US\$|\$|Rs\.?|₹)?\s*([\d,]+(?:\.\d{1,2})?)", re.I)


def printed_total_paise(text: str) -> int | None:
    """The 'Total Payable' figure the document prints (what a naive agent would pay). None if absent."""
    m = TOTAL_RE.findall(text or "")
    if not m:
        return None
    rupees, _, paise = m[-1].replace(",", "").partition(".")
    return int(rupees) * 100 + int((paise + "00")[:2]) if rupees.isdigit() else None


AMOUNT_ONLY_RE = re.compile(r"^(?:INR|USD|US\$|\$|Rs\.?|₹)?\s*(-?[\d,]+\.\d{2})$")
AMOUNT_TAIL_RE = re.compile(r"^(.*\S)\s+(?:INR|USD|US\$|\$|Rs\.?|₹)?\s*(-?[\d,]+\.\d{2})$")
HSN_CELL_RE = re.compile(r"^\d{4,8}$")
QTY_CELL_RE = re.compile(r"^(?:(\d{1,6})\s*[x×]\s*(?:INR|USD|US\$|\$|Rs\.?|₹)?\s*([\d,]+\.\d{2})|1\s+lot)$", re.I)
TAX_RE = re.compile(r"\b(CGST|SGST|IGST|UTGST|GST|CESS)\b", re.I)


def _paise(s: str) -> int:
    s = s.replace(",", "")
    neg = s.startswith("-")
    rupees, _, paise = s.lstrip("-").partition(".")
    v = int(rupees) * 100 + int((paise + "00")[:2])
    return -v if neg else v


def item_lines(text: str) -> list[dict]:
    """Rows of the item table: everything between the 'Description ... Amount' header and 'Total Payable'.

    The amount column closes a row. A row is a description followed by any other cells (HSN code,
    qty x rate) and then an amount; the text layer may put the cells on one line or on several
    (a multi-column table gives one line per cell). A line that ends in an amount while a
    description is open is a middle cell or a wrapped description line; its amount is used only
    when no amount-only line follows. Tax rows are flagged; every other row is a charge."""
    lines = [l.strip() for l in (text or "").splitlines() if l.strip()]
    start = next((i for i, l in enumerate(lines) if l.lower().startswith("description")), None)
    end = next((i for i, l in enumerate(lines) if l.lower().startswith("total payable")), None)
    if start is None or end is None or end <= start:
        return []
    body = lines[start + 1:end]
    # skip the rest of the header row (e.g. "HSN/SAC", "Qty x Rate") up to the "Amount" header cell
    hdr = next((i for i, l in enumerate(body[:6]) if l.lower().startswith("amount")), None)
    if hdr is not None:
        body = body[hdr + 1:]
    rows, desc, tail, qty = [], [], None, None

    def close(amount):
        nonlocal desc, tail, qty
        rows.append((" ".join(desc), amount, qty))
        desc, tail, qty = [], None, None

    for l in body:
        if l.lower().startswith("amount"):
            continue
        if desc and HSN_CELL_RE.match(l):  # HSN/SAC cell of the open row
            continue
        m = QTY_CELL_RE.match(l)
        if desc and m:  # "12 x 126.52" or "1 lot": quantity/rate cell of the open row
            qty = (int(m.group(1)), _paise(m.group(2))) if m.group(1) else (1, None)
            continue
        m = AMOUNT_ONLY_RE.match(l)
        if m:
            if desc:
                close(_paise(m.group(1)))
            continue
        m = AMOUNT_TAIL_RE.match(l)
        if m:
            if desc:
                desc.append(m.group(1))
                tail = _paise(m.group(2))
            else:
                rows.append((m.group(1), _paise(m.group(2)), None))
            continue
        if desc and tail is not None:
            close(tail)
        desc.append(l)
    if desc and tail is not None:
        close(tail)
    out = []
    for d, a, q in rows:
        row = {"description": d, "amount_paise": a, "is_tax": bool(TAX_RE.search(d))}
        if q is not None:
            row["qty"] = q[0]
            row["unit_price_minor"] = q[1] if q[1] is not None else a
        out.append(row)
    return out
