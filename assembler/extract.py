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
                r"|gstin|gst\s*no|pan\b|cin\b|tan\b|tin\b|vat\b|cst\b|udyam|msme|irn\b|ack\w*|e-?way|ewb\b|lut\b|hsn\b|sac\b"
                r"|p\.?\s?o\b\.?|purchase\s+order|order\b|invoice|inv\b\.?|bill\s*no|challan|dc\s*no"
                r"|ref\w*|utr\b|txn\b|transaction|cheque|chq\b|dd\s*no|receipt|voucher|grn\b|docket|awb\b|lr\s*no|tracking|consignment"
                r"|pin\b|pin\s*code|pincode|zip\b|postal|routing|aba\b|micr\b|sort\s*code|branch\s*code|serial|s/?n\b|imei|licen[cs]e|ticket|case\b)")
_LABEL_RE = re.compile(rf"(?P<bank>{_BANK_LABEL})|(?P<other>{_OTHER_LABEL})", re.I)
_BANK_WORD_RE = re.compile(_BANK_LABEL, re.I)
# "<label words> [No|ID|Code|Number|#]:" right before the number, on its own line or after a gap.
_FIELD_RE = re.compile(r"(?:^|\s{2,}|[|;,]\s*)([A-Za-z][A-Za-z0-9 .&/()'#-]{0,40}?)\s*[:#]\s*$")
_SWIFT_RE = re.compile(r"\b(?:SWIFT|BIC)\s*(?:code)?\s*[:\-]?\s*[A-Z]{6}[A-Z0-9]{2}(?:[A-Z0-9]{3})?\b", re.I)


_MOBILE_RE = re.compile(r"[6-9]\d{9}")


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
        # "Near Bank of Maharashtra ... Pune.98xxxxxxxx": a bare "bank" word (an address, a bank
        # name) does not make a mobile-shaped number an account; A/c, Account, IFSC... still do.
        if re.fullmatch(r"bank", nearest.group(0).strip(), re.I) and _MOBILE_RE.fullmatch(text[start:end]) and not code_after:
            return "other"
        return "account"
    if nearest is not None:
        return "account" if code_after else "other"
    if _bank_code_in(text[max(0, start - 40):end + 40]):
        return "account"
    # A bank word nearby is weak evidence: addresses say "Near Bank of Maharashtra". An unlabelled
    # number shaped like an Indian mobile (10 digits, 6-9 first) needs a real label or an IFSC.
    if _BANK_WORD_RE.search(text[max(0, start - 80):start]) and not _MOBILE_RE.fullmatch(text[start:end]):
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


# OCR-tolerant GSTIN candidates: 15 characters starting with two digits, possibly split once
# after the state code ("27 AARFA8194J1ZP"), with OCR slips in class-impossible slots.
_GSTIN_LOOSE_RE = re.compile(r"(?<![A-Z0-9$|])(\d{2}) ?([A-Z0-9$|]{13})(?![A-Z0-9$|])")


def issuer_gstins(upper: str, buyer_gstin: str) -> list[str]:
    """GSTINs printed in the text, in order, minus the buyer's own.

    A strict-format match is kept as printed unless it fails the mod-36 check and an OCR repair
    (structural.gstin.repair_ocr: slot-class confusions, checksum-gated) turns it into a valid one;
    a loose 15-character token is kept only when such a repair succeeds. The buyer is recognised
    after repair too, so an OCR'd copy of the buyer's GSTIN ('...C1ZY' read as '...CIZY') is not
    taken for the issuer. Found on real invoices (docs/FINDINGS_REAL_INVOICES.md)."""
    from structural.gstin import repair_ocr, validate
    buyer = (buyer_gstin or "").upper()
    found = []
    for m in _GSTIN_LOOSE_RE.finditer(upper):
        tok = m.group(1) + m.group(2)
        strict = bool(GSTIN_RE.fullmatch(tok))
        if strict and not validate(tok):
            g = tok
        else:
            g = repair_ocr(tok) or (tok if strict else None)
        if g is None:
            continue
        if g == buyer or (repair_ocr(tok) or tok) == buyer:
            continue
        found.append((m.start(), g))
    return _uniq(g for _, g in sorted(found))


# --- Look-alike bank fields (red-team R6) -------------------------------------------------------
# IFSC_RE and ACCT_RE are ASCII-only, so an IFSC printed with a Cyrillic "В" or an account number
# in fullwidth digits used to be invisible: no value, no tamper alert. A token that is not plain
# ASCII but folds (NFKC + Cyrillic/Greek look-alike map) into an IFSC or account shape is reported
# AS PRINTED, so the governor sees the non-ASCII value and raises its tamper alert. Nothing is
# ever "repaired" into a payable value; the payee still comes only from the vendor master.
_CONFUSABLE = str.maketrans({
    # Cyrillic
    "А": "A", "В": "B", "Е": "E", "К": "K", "М": "M", "Н": "H", "О": "O", "Р": "P", "С": "C",
    "Т": "T", "Х": "X", "У": "Y", "І": "I", "Ј": "J", "Ѕ": "S", "Ԁ": "D", "Ԛ": "Q", "Ԝ": "W",
    "а": "a", "е": "e", "о": "o", "р": "p", "с": "c", "у": "y", "х": "x", "і": "i", "ј": "j", "ѕ": "s",
    # Greek
    "Α": "A", "Β": "B", "Ε": "E", "Ζ": "Z", "Η": "H", "Ι": "I", "Κ": "K", "Μ": "M", "Ν": "N",
    "Ο": "O", "Ρ": "P", "Τ": "T", "Υ": "Y", "Χ": "X", "ο": "o",
})
_TOKEN_RE = re.compile(r"[^\s:;,|()\[\]{}<>\"']+")
_INVISIBLE_RE = re.compile("[\u200b-\u200f\u2060-\u2064\ufeff\u00ad\u202a-\u202e]")


def fold_confusables(tok: str) -> str:
    import unicodedata
    return _INVISIBLE_RE.sub("", unicodedata.normalize("NFKC", tok)).translate(_CONFUSABLE)


def confusable_bank_tokens(text: str) -> tuple[list[str], list[str]]:
    """(ifscs, accounts) printed with non-ASCII characters that fold into a bank-field shape."""
    text = text or ""
    ifscs, accts = [], []
    for m in _TOKEN_RE.finditer(text):
        tok = m.group().rstrip(".")
        if tok.isascii():
            continue
        f = fold_confusables(tok).upper()
        f0 = f[:4] + "0" + f[5:] if len(f) == 11 and f[4] == "O" else f  # Cyrillic О where the 0 goes
        if IFSC_RE.fullmatch(f) or IFSC_RE.fullmatch(f0):
            ifscs.append(tok)
        elif re.fullmatch(r"\d{9,18}", f) and classify_number(text, m.start(), m.start() + len(tok)) == "account":
            accts.append(tok)
    return _uniq(ifscs), _uniq(accts)


def sender_domain(email_from: str) -> str | None:
    m = re.search(r"@([A-Za-z0-9.\-]+)\s*>?\s*$", email_from.strip())
    return m.group(1).lower() if m else None


TEXT_LAYER_CAP = 100_000


def governor_evidence(ev: Evidence, buyer_gstin: str) -> dict:
    """Build the `evidence` object of a governor request (field names fixed by the Rust schema)."""
    text = ev.text or ""
    upper = text.upper()
    gstins = issuer_gstins(upper, buyer_gstin)
    box_acct = BOX_ACCT_RE.search(text)
    box_ifsc = BOX_IFSC_RE.search(text)
    accts = printed_accounts(text)
    odd_ifscs, odd_accts = confusable_bank_tokens(text)
    out = {
        "has_text_layer": bool(text.strip()),
        "text_source": (ev.metadata or {}).get("text_source"),
        "text_layer_gstin": gstins[0] if gstins else None,
        "sender_domain": sender_domain(ev.email_from),
        "printed_bank_account": re.sub(r"\s+", "", box_acct.group(1)) if box_acct else None,
        "printed_ifsc": box_ifsc.group(1).upper() if box_ifsc else None,
        "all_printed_accounts": _uniq(accts + odd_accts),
        "all_printed_ifscs": _uniq(IFSC_RE.findall(upper) + odd_ifscs),
        "printed_total_paise": printed_total_paise(text),
        "item_lines": item_lines(text),
        "currency": doc_currency(text),
        "all_currencies": currencies_seen(text),
        # Signed with the rest; the governor uses it only to verify model-proposed item lines
        # verbatim (governor/src/lines.rs). Capped so a huge document cannot bloat the envelope.
        "text_layer": text[:TEXT_LAYER_CAP],
        "tax_kinds": tax_kinds(text),
        "document_kind": document_kind(text),
    }
    return out


# --- GST heads and document kind (review item 3, 2026-10-31) -------------------------------------
_TAX_HEAD_RE = re.compile(r"\b(CGST|SGST|UTGST|IGST)\b", re.I)
_TAX_AMT_RE = re.compile(r"(?<![\d.,])(\d[\d,]*\.\d{2})(?![\d,])(?!\s*%)")


def tax_kinds(text: str) -> list[str]:
    """GST heads charged with a non-zero amount: the first amount printed after the head (within 60
    characters and before the next head) must be non-zero. 'IGST 0.00' or 'IGST not applicable'
    does not count; a percentage ('9%') is not an amount."""
    text = text or ""
    found = set()
    heads = list(_TAX_HEAD_RE.finditer(text))
    for i, m in enumerate(heads):
        stop = heads[i + 1].start() if i + 1 < len(heads) else len(text)
        window = text[m.end():min(stop, m.end() + 60)]
        a = _TAX_AMT_RE.search(window)
        if a and _paise(a.group(1)) != 0:
            found.add(m.group(1).upper())
    return sorted(found)


_CREDIT_NOTE_RE = re.compile(r"^\W*credit\s*note\b", re.I)


def document_kind(text: str) -> str:
    """'credit_note' when one of the first 12 non-empty lines is titled Credit Note, else 'invoice'."""
    head = [l.strip() for l in (text or "").splitlines() if l.strip()][:12]
    return "credit_note" if any(_CREDIT_NOTE_RE.match(l) for l in head) else "invoice"


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
    """The 'Total Payable' figure the document prints (what a naive agent would pay).

    Real invoices rarely say "Total Payable" ("Grand Total", "Bill Amount", "Total Invoice Amount",
    Tally's "Amount Chargeable"). Fallback: the largest amount the document writes out in WORDS
    ("Two Lakh Thirty Three Thousand Four Hundred Four Only") that it also prints in figures. Both
    must agree, so a stray number never becomes the total. None if neither is found."""
    m = TOTAL_RE.findall(text or "")
    if m:
        rupees, _, paise = m[-1].replace(",", "").partition(".")
        return int(rupees) * 100 + int((paise + "00")[:2]) if rupees.isdigit() else None
    return words_confirmed_total_paise(text or "")


_NUM_WORDS = {
    "zero": 0, "one": 1, "two": 2, "three": 3, "four": 4, "five": 5, "six": 6, "seven": 7, "eight": 8, "nine": 9,
    "ten": 10, "eleven": 11, "twelve": 12, "thirteen": 13, "fourteen": 14, "fifteen": 15, "sixteen": 16,
    "seventeen": 17, "eighteen": 18, "nineteen": 19, "twenty": 20, "thirty": 30, "forty": 40, "fourty": 40,
    "fifty": 50, "sixty": 60, "seventy": 70, "eighty": 80, "ninety": 90, "ninty": 90,
}
_SCALES = {"hundred": 100, "thousand": 1_000, "lakh": 1_00_000, "lakhs": 1_00_000, "lac": 1_00_000, "lacs": 1_00_000,
           "crore": 1_00_00_000, "crores": 1_00_00_000, "million": 1_000_000}
_WORD_RUN_RE = re.compile(r"(?:\b(?:%s|and)\b[\s-]*){2,}" % "|".join(sorted(set(_NUM_WORDS) | set(_SCALES), key=len, reverse=True)), re.I)
_FIGURE_RE = re.compile(r"(?<![\d.,])\d{1,3}(?:,\d{2,3})+(?:\.\d{1,2})?(?![\d,])|(?<![\d.,])\d+(?:\.\d{1,2})?(?![\d,])")


def words_to_int(words: str) -> int | None:
    total, cur, seen = 0, 0, False
    for w in re.findall(r"[a-z]+", words.lower()):
        if w == "and":
            continue
        if w in _NUM_WORDS:
            cur += _NUM_WORDS[w]
            seen = True
        elif w == "hundred":
            cur = max(cur, 1) * 100
        elif w in _SCALES:
            total += max(cur, 1) * _SCALES[w]
            cur = 0
            seen = True
    return total + cur if seen else None


def words_confirmed_total_paise(text: str) -> int | None:
    figures = set()
    for f in _FIGURE_RE.findall(text):
        r, _, p = f.replace(",", "").partition(".")
        figures.add(int(r) * 100 + int((p + "00")[:2]))
    best = None
    for m in _WORD_RUN_RE.finditer(text):
        tail = text[m.end():m.end() + 30].lower()
        if re.match(r"\s*(?:and\s+[a-z\s-]+?\s+)?paise", tail):
            continue  # "... and Eight paise" belongs to a tax-amount line; skip fractional words
        v = words_to_int(m.group(0))
        if v and v * 100 in figures and (best is None or v * 100 > best):
            best = v * 100
    return best


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
