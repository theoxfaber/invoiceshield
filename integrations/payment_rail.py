"""Payment-rail adapter: the interface a real bank/payout integration would implement, and a
sandbox stub. There is NO real rail here: no bank account, no API credentials, no network, no
money. (agent/rail.py is a different thing: the shield-OFF demo's inline-bank-details mock.)

The interface takes a governor decision, not bank details. A PaymentInstruction can only be built
from a decision whose verdict is ALLOW (or a REVIEW an approver resolved with approve=true), and it
pays a *beneficiary id* registered from the vendor master, so a rail implementation never sees an
account number that came from an invoice or an agent.
"""
from __future__ import annotations

import hashlib
from dataclasses import dataclass, field
from typing import Protocol, runtime_checkable


class RailRefused(ValueError):
    pass


@dataclass(frozen=True)
class PaymentInstruction:
    decision_id: str
    beneficiary_id: str
    amount_minor: int
    currency: str
    narration: str
    # idempotency key the rail must honour: one decision, at most one transfer
    idempotency_key: str

    @classmethod
    def from_decision(cls, decision: dict, amount_minor: int, currency: str = "INR", resolved_approved: bool = False,
                      narration: str = "") -> "PaymentInstruction":
        verdict = decision.get("verdict")
        if not (verdict == "allow" or (verdict == "review" and resolved_approved)):
            raise RailRefused(f"decision {decision.get('decision_id')} is {verdict!r}" + (" (unresolved)" if verdict == "review" else "") + ": not payable")
        payee = decision.get("payee") or {}
        bene = payee.get("beneficiary_id")
        if not bene:
            raise RailRefused("decision has no payee from the vendor master")
        if not isinstance(amount_minor, int) or isinstance(amount_minor, bool) or amount_minor <= 0:
            raise RailRefused("amount must be a positive integer of minor units")
        did = decision["decision_id"]
        return cls(decision_id=did, beneficiary_id=bene, amount_minor=amount_minor, currency=currency.upper(),
                   narration=(narration or f"InvoiceShield {did}")[:60], idempotency_key=f"isg-{did}")


@dataclass(frozen=True)
class RailReceipt:
    rail_ref: str
    status: str  # 'accepted' | 'settled' | 'failed'
    decision_id: str
    replayed: bool = False


@runtime_checkable
class PaymentRail(Protocol):
    name: str
    live: bool

    def submit(self, ins: PaymentInstruction) -> RailReceipt: ...
    def status(self, rail_ref: str) -> RailReceipt: ...


@dataclass
class SandboxRail:
    """In-memory stand-in. Deterministic refs, idempotent per instruction key, accepts only
    beneficiaries registered with it (as a bank would), never moves money."""
    name: str = "sandbox"
    live: bool = False
    beneficiaries: set[str] = field(default_factory=set)
    max_amount_minor: int = 10_000_000_00  # sandbox ceiling, ₹1 crore
    _by_key: dict[str, tuple[PaymentInstruction, RailReceipt]] = field(default_factory=dict)
    _by_ref: dict[str, RailReceipt] = field(default_factory=dict)

    def register_beneficiary(self, beneficiary_id: str) -> None:
        self.beneficiaries.add(beneficiary_id)

    def submit(self, ins: PaymentInstruction) -> RailReceipt:
        prior = self._by_key.get(ins.idempotency_key)
        if prior is not None:
            if prior[0] != ins:
                raise RailRefused(f"idempotency key {ins.idempotency_key} reused with a different instruction")
            r = prior[1]
            return RailReceipt(r.rail_ref, self._by_ref[r.rail_ref].status, r.decision_id, replayed=True)
        if ins.beneficiary_id not in self.beneficiaries:
            raise RailRefused(f"beneficiary {ins.beneficiary_id} not registered with the rail")
        if ins.amount_minor > self.max_amount_minor:
            raise RailRefused("above sandbox ceiling")
        ref = "SBX" + hashlib.sha256(ins.idempotency_key.encode()).hexdigest()[:16].upper()
        rec = RailReceipt(ref, "accepted", ins.decision_id)
        self._by_key[ins.idempotency_key] = (ins, rec)
        self._by_ref[ref] = rec
        return rec

    def settle(self, rail_ref: str, ok: bool = True) -> None:
        """Test hook: what a bank's webhook would report later."""
        r = self._by_ref[rail_ref]
        self._by_ref[rail_ref] = RailReceipt(r.rail_ref, "settled" if ok else "failed", r.decision_id)

    def status(self, rail_ref: str) -> RailReceipt:
        if rail_ref not in self._by_ref:
            raise KeyError(rail_ref)
        return self._by_ref[rail_ref]
