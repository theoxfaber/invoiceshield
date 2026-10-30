"""GSTIN format, mod-36 check character, and PAN binding.

Checksums are public, so this catches typos and sloppy forgeries only. It is
never the reason a payment is allowed; the vendor master is.
"""
import re

ALPHABET = "0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ"
GSTIN_RE = re.compile(r"^[0-9]{2}[A-Z]{5}[0-9]{4}[A-Z][1-9A-Z]Z[0-9A-Z]$")


def check_char(first14: str) -> str:
    total = 0
    for i, ch in enumerate(first14):
        product = ALPHABET.index(ch) * (1 if i % 2 == 0 else 2)
        total += product // 36 + product % 36
    return ALPHABET[(36 - total % 36) % 36]


def with_check_char(first14: str) -> str:
    first14 = first14.upper()
    return first14 + check_char(first14)


def validate(gstin: str, master_pan: str | None = None) -> list[str]:
    """Return a list of problems (empty = looks valid)."""
    g = (gstin or "").strip().upper()
    problems = []
    if not GSTIN_RE.match(g):
        return ["gstin_bad_format"]
    if not 1 <= int(g[:2]) <= 38 and g[:2] not in ("97", "99"):
        problems.append("gstin_bad_state_code")
    if check_char(g[:14]) != g[14]:
        problems.append("gstin_bad_checksum")
    if master_pan and g[2:12] != master_pan.upper():
        problems.append("gstin_pan_mismatch")
    return problems


# OCR confusions per character class. A GSTIN is 2 digits (state), 5 letters + 4 digits +
# 1 letter (PAN), 1 entity char [1-9A-Z], the letter Z, 1 check char.
_TO_DIGIT = {"O": "0", "D": "0", "Q": "0", "I": "1", "L": "1", "|": "1", "Z": "2", "S": "5", "$": "5", "B": "8", "G": "6"}
_TO_LETTER = {"0": "O", "1": "I", "5": "S", "$": "S", "8": "B", "2": "Z", "6": "G"}
_DIGIT_POS = (0, 1, 7, 8, 9, 10)
_LETTER_POS = (2, 3, 4, 5, 6, 11)


def repair_ocr(token: str) -> str | None:
    """A checksum-valid GSTIN from a 15-character OCR token with class-impossible characters
    (e.g. '27BEMPLO248G1ZH' -> '27BEMPL0248G1ZH'). Characters impossible in their slot are
    mapped; the entity character (slot 12, where '1' and 'I' are both legal) is flipped only
    when the checksum fails ('27AAZFS3607CIZY' -> '27AAZFS3607C1ZY'). The result must pass the
    mod-36 check; returns None when no repair does. A valid token is returned unchanged.

    The checksum is public, so a repaired GSTIN is no more trustworthy than a printed one: it
    only stops an OCR slip from looking like a different issuer."""
    t = (token or "").strip().upper()
    if len(t) != 15:
        return None
    if not validate(t):
        return t
    c = list(t)
    for i in _DIGIT_POS:
        if not c[i].isdigit():
            c[i] = _TO_DIGIT.get(c[i], c[i])
    for i in _LETTER_POS:
        if not c[i].isalpha():
            c[i] = _TO_LETTER.get(c[i], c[i])
    if c[13] != "Z":
        c[13] = {"2": "Z", "7": "Z"}.get(c[13], c[13])
    g = "".join(c)
    if not validate(g):
        return g
    # slot 13 (entity number) is often '1' misread as 'I' or the reverse; try both
    for alt in ("1", "I"):
        if c[12] in "1I" and alt != c[12]:
            g2 = g[:12] + alt + g[13:]
            if not validate(g2):
                return g2
    return None
