"""Shared helpers: money formatting, approved-payment records, vendor-master diffing."""
from __future__ import annotations

import datetime as _dt
from dataclasses import dataclass, field

IST = _dt.timezone(_dt.timedelta(hours=5, minutes=30))


def minor_to_decimal_str(minor: int) -> str:
    """12345 -> '123.45', -5 -> '-0.05'. Integer arithmetic only (no floats)."""
    if not isinstance(minor, int) or isinstance(minor, bool):
        raise TypeError("amount must be integer minor units")
    sign = "-" if minor < 0 else ""
    q, r = divmod(abs(minor), 100)
    return f"{sign}{q}.{r:02d}"


def decimal_str_to_minor(s: str) -> int:
    """'1,23,456.7' -> 12345670. Rejects more than two decimals and anything non-numeric."""
    t = str(s).strip().replace(",", "")
    neg = t.startswith("-")
    if t and t[0] in "+-":
        t = t[1:]
    whole, _, frac = t.partition(".")
    if not whole.isdigit() or (frac and not frac.isdigit()) or len(frac) > 2:
        raise ValueError(f"not a 2-decimal amount: {s!r}")
    v = int(whole) * 100 + int((frac + "00")[:2])
    return -v if neg else v


@dataclass(frozen=True)
class ApprovedPayment:
    """A payment the governor approved: ALLOW at decision time, or a REVIEW an approver resolved
    with approve=true. Read from the governor ledger (status 'paid'), never from an agent."""
    decision_id: str
    vendor_id: str
    invoice_key: str
    amount_minor: int
    currency: str
    po_ref: str
    ts: int
    beneficiary_id: str | None = None
    approver: str | None = None

    def date_ist(self) -> _dt.date:
        return _dt.datetime.fromtimestamp(self.ts, IST).date()

    @classmethod
    def from_dict(cls, d: dict) -> "ApprovedPayment":
        amt = d.get("amount_minor", d.get("amount_paise"))
        if not isinstance(amt, int) or isinstance(amt, bool) or amt <= 0:
            raise ValueError(f"{d.get('decision_id')}: amount must be a positive integer of minor units")
        return cls(decision_id=str(d["decision_id"]), vendor_id=str(d["vendor_id"]), invoice_key=str(d["invoice_key"]),
                   amount_minor=amt, currency=str(d.get("currency", "INR")).upper(), po_ref=str(d.get("po_ref", "")),
                   ts=int(d["ts"]), beneficiary_id=d.get("beneficiary_id"), approver=d.get("approver"))


@dataclass
class MasterDiff:
    """What an import found, relative to the current vendor master. Nothing here is applied."""
    unchanged: list[str] = field(default_factory=list)
    # vendor_ids present in the accounting system but not in the master: onboarding candidates
    new_vendors: list[dict] = field(default_factory=list)
    # proposed POST /v1/master-changes bodies (maker-checker workflow); never applied directly
    bank_change_requests: list[dict] = field(default_factory=list)
    # non-bank differences (GSTIN, PAN, legal name) for a human to look at
    field_mismatches: list[dict] = field(default_factory=list)
    errors: list[str] = field(default_factory=list)

    def as_dict(self) -> dict:
        return {"unchanged": self.unchanged, "new_vendors": self.new_vendors, "bank_change_requests": self.bank_change_requests,
                "field_mismatches": self.field_mismatches, "errors": self.errors, "applied": False}


def _norm(s: str | None) -> str:
    return "".join((s or "").split()).upper()


def diff_against_master(master: dict, incoming: list[dict], source: str) -> MasterDiff:
    """`incoming` items: vendor_id plus any of legal_name, gstin, pan, bank_account, ifsc, bank_name.
    A missing field (None) means 'the source does not carry it', not 'cleared'."""
    by_id = {v["vendor_id"]: v for v in master.get("vendors", [])}
    out = MasterDiff()
    seen: set[str] = set()
    for inc in incoming:
        vid = inc.get("vendor_id")
        if not vid:
            out.errors.append(f"{source}: record without a vendor_id mapping: {inc.get('legal_name')!r}")
            continue
        if vid in seen:
            out.errors.append(f"{source}: {vid} appears twice; skipped the second record")
            continue
        seen.add(vid)
        cur = by_id.get(vid)
        if cur is None:
            out.new_vendors.append({**inc, "source": source, "note": "onboard through the normal vendor process; bank details are not trusted from this import"})
            continue
        changed = False
        for f in ("legal_name", "gstin", "pan"):
            if inc.get(f) is not None and _norm(inc[f]) != _norm(cur.get(f)):
                out.field_mismatches.append({"vendor_id": vid, "field": f, "master": cur.get(f), source: inc[f]})
                changed = True
        acct, ifsc = inc.get("bank_account"), inc.get("ifsc")
        if acct is not None or ifsc is not None:
            if _norm(acct) != _norm(cur.get("bank_account")) or _norm(ifsc) != _norm(cur.get("ifsc")):
                out.bank_change_requests.append({
                    "vendor_id": vid, "new_account": _norm(acct), "new_ifsc": _norm(ifsc),
                    "new_bank_name": inc.get("bank_name") or "",
                    # The beneficiary must be registered with the bank/rail first; the operator fills it in.
                    "new_beneficiary_id": None,
                    "_source": source,
                    "_note": "proposal only: submit as POST /v1/master-changes after confirming with the vendor out of band",
                })
                changed = True
        if not changed:
            out.unchanged.append(vid)
    return out
