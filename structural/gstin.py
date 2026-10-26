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
