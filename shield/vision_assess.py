"""Plain-code assessment of a Vision Witness transcription (post-freeze, feat/meta-vision).

The model only says what is visible on the page. THIS code decides what that means, by comparing the
transcription with the trusted vendor master. Every finding is an escalate-only signal for the Rust
governor (kind + severity review|block); none of them can produce a PAY.

Findings (all AI-DEPENDENT: they depend on the model reading the image correctly):

| kind                         | severity | fires when                                                          |
|------------------------------|----------|---------------------------------------------------------------------|
| vision_bank_mismatch         | block    | visible bank details (account or IFSC) differ from the master record |
|                              |          | of the vendor the page claims to be                                 |
| vision_payment_instruction   | block    | a visible note asks to change where money goes, talks to an AI,     |
|                              |          | or claims approval                                                  |
| vision_issuer_unknown        | review   | the visible issuer/GSTIN matches no vendor in the master            |
| vision_text_layer_disagrees  | block    | the page shows bank digits the signed text layer does not contain   |
|                              |          | (what the human sees is not what the parser read), or vice versa    |
| vision_unreadable            | review   | the witness failed, could not read the page, or returned nothing     |

A matching bank box is NOT a pass: the photo still goes to REVIEW (a photo is never auto-paid). The
witness only ever turns a blank REVIEW into a pinpointed BLOCK with a box a human can check.
"""
from __future__ import annotations

import json
import re
from dataclasses import asdict, dataclass

from shield.vision_witness import Witness, digits

BLOCK, REVIEW = "block", "review"
INSTRUCTION_KINDS = {"bank_change", "instruction_to_ai", "approval_claim"}


@dataclass
class Finding:
    kind: str
    severity: str
    detail: str
    box: list[int] | None = None
    text: str = ""


def fold(s: str | None) -> str:
    return re.sub(r"[^a-z0-9]+", " ", (s or "").lower()).strip()


_SUFFIXES = {"pvt", "ltd", "private", "limited", "llp", "inc", "co", "the"}


def core_name(s: str | None) -> str:
    return " ".join(w for w in fold(s).split() if w not in _SUFFIXES)


def match_vendor(master: dict, w: Witness) -> dict | None:
    """The vendor the page claims to be: exact GSTIN first, then exact core legal name. No fuzzy match:
    a look-alike name is NOT resolved to the real vendor (that is the attacker's goal)."""
    g = re.sub(r"\s+", "", (w.issuer_gstin or "")).upper()
    if g:
        for v in master["vendors"]:
            if (v.get("gstin") or "").upper() == g:
                return v
    cn = core_name(w.issuer_name)
    if cn:
        for v in master["vendors"]:
            if core_name(v["legal_name"]) == cn:
                return v
    return None


def ifsc_norm(s: str | None) -> str:
    # IFSC: 4 letters, '0', 6 alphanumerics.
    t = re.sub(r"[^A-Z0-9]", "", (s or "").upper())
    if len(t) == 11:  # positions 5-11 are the branch code; read O as 0 there on both sides
        t = t[:4] + t[4:].replace("O", "0")
    return t


def assess(w: Witness, master: dict, *, text_layer: str | None = None) -> list[Finding]:
    out: list[Finding] = []
    if not w.ok:
        return [Finding("vision_unreadable", REVIEW, f"witness failed: {w.error}"[:200])]
    if not w.readable and not w.bank_details and not w.notices:
        return [Finding("vision_unreadable", REVIEW, "witness could not read the page")]

    vend = match_vendor(master, w)
    if vend is None and (w.issuer_name or w.issuer_gstin):
        out.append(Finding("vision_issuer_unknown", REVIEW,
                           f"visible issuer {w.issuer_name!r} / GSTIN {w.issuer_gstin!r} matches no vendor on file",
                           w.issuer_box, w.issuer_name or ""))

    if vend is not None:
        m_acct, m_ifsc = digits(vend.get("bank_account")), ifsc_norm(vend.get("ifsc"))
        for b in w.bank_details:
            acct, ifsc = b.get("account") or "", ifsc_norm(b.get("ifsc"))
            bad = []
            if acct and acct != m_acct:
                bad.append(f"account ...{acct[-4:]} (on file ...{m_acct[-4:]})")
            if ifsc and ifsc != m_ifsc:
                bad.append(f"IFSC {ifsc} (on file {m_ifsc})")
            if bad:
                out.append(Finding("vision_bank_mismatch", BLOCK,
                                   f"page shows bank details that are not {vend['legal_name']}'s on file: " + ", ".join(bad),
                                   b.get("box"), b.get("text") or ""))

    for n in w.notices:
        if n["kind"] in INSTRUCTION_KINDS:
            how = "handwritten " if n.get("handwritten") else ("faint/small " if n.get("faint_or_small") else "")
            out.append(Finding("vision_payment_instruction", BLOCK,
                               f"{how}note on the page ({n['kind']}): {n.get('text', '')[:140]!r}", n.get("box"), n.get("text") or ""))

    if text_layer is not None and text_layer.strip():
        tl = digits(text_layer)
        for b in w.bank_details:
            acct = b.get("account") or ""
            if len(acct) >= 8 and acct not in tl:
                out.append(Finding("vision_text_layer_disagrees", BLOCK,
                                   f"page shows account ...{acct[-4:]} that the document's text layer does not contain",
                                   b.get("box"), b.get("text") or ""))
    return out


def verdict_of(findings: list[Finding]) -> str:
    """The escalation the findings ask for. Never 'allow': a photo is REVIEW at best."""
    return BLOCK if any(f.severity == BLOCK for f in findings) else REVIEW


def to_signals(findings: list[Finding]) -> list[dict]:
    """Governor Signal shape (verdict.rs: kind, severity review|block, detail)."""
    return [{"kind": f.kind, "severity": f.severity, "detail": f.detail[:300]} for f in findings]


def checker_raw(findings: list[Finding], text_checker_raw: str | None = None) -> str:
    """Merge witness signals into the checker's raw report (the governor reads it only from the signed
    checker envelope). If the text checker's output does not parse, it is replaced by one REVIEW
    `checker_unparseable` signal (what the governor's own parser would have produced), so the witness
    signals are never lost and nothing gets less strict."""
    sig = to_signals(findings)
    if text_checker_raw is not None:
        try:
            base = json.loads(text_checker_raw)
        except ValueError:
            base = None
        if isinstance(base, dict) and isinstance(base.get("signals"), list) and set(base) == {"signals"}:
            return json.dumps({"signals": base["signals"] + sig})
        sig = sig + [{"kind": "checker_unparseable", "severity": REVIEW, "detail": "text checker output did not parse"}]
    return json.dumps({"signals": sig})


def findings_json(findings: list[Finding]) -> list[dict]:
    return [asdict(f) for f in findings]
