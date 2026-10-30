"""Shield-ON pipeline: agent proposal + quarantined checker -> governor -> rail.

Two phases so eval stays correct and fast: model calls run in parallel (slow, cached);
governor calls run serially with a ledger reset before each sample (so three samples of the
same invoice do not trip the duplicate rules).
"""
from __future__ import annotations

import json
from dataclasses import dataclass, field

from agent.on_agent import OnProposal, run_on_agent
from agent.rail import MockRail, Transfer
from assembler.evidence import assemble
from assembler.extract import governor_evidence
from checker.checker import CheckerOut, run_checker
from providers.llm import LLMProvider

def load_master(path=None) -> dict:
    import pathlib
    path = path or pathlib.Path(__file__).resolve().parents[1] / "dataset" / "fixtures" / "master.json"
    return json.loads(pathlib.Path(path).read_text())


BUYER_GSTIN = "24ZZACB7319B1Z5"


@dataclass
class ModelPhase:
    case_id: str
    sample: int
    evidence: dict               # local copy for reports only; the governor never receives it
    proposal: OnProposal
    checker: CheckerOut | None
    pdf: str | None = None       # the invoice document; the gate sends it to the extraction service
    cover_email: dict | None = None


def model_phase(provider: LLMProvider, case: dict, vendors: dict, sample: int, *, reasoning: str = "low") -> ModelPhase:
    ev = assemble(case["pdf"], case["cover_email"])
    gev = governor_evidence(ev, BUYER_GSTIN)
    prop = run_on_agent(provider, ev, vendors, sample=sample, reasoning=reasoning)
    chk = run_checker(provider, ev, sample=sample, reasoning=reasoning) if prop.kind == "propose" else None
    return ModelPhase(case["id"], sample, gev, prop, chk, str(case["pdf"]), case["cover_email"])


@dataclass
class ShieldResult:
    outcome: str                 # "paid" | "review" | "block" | "agent_hold" | "agent_reject" | "agent_none"
    decision: dict | None = None
    transfer: Transfer | None = None
    reasons: list[str] = field(default_factory=list)


def gate(gov, mp: ModelPhase, master: dict, rail: MockRail) -> ShieldResult:
    p = mp.proposal
    if p.kind != "propose":
        return ShieldResult({"hold": "agent_hold", "reject": "agent_reject", "none": "agent_none"}[p.kind])
    proposal = {k: v for k, v in p.args.items() if k != "reason"}
    gov.reset()
    checker_raw = mp.checker.raw_for_governor if mp.checker else ""
    if mp.pdf and getattr(gov, "extractor", None) is not None:
        # Signed path: the extraction service reads the document and signs the evidence; the
        # proposal names the document by sha256. Nothing the agent wrote reaches `evidence`.
        d = gov.evaluate_document(proposal, mp.pdf, mp.cover_email, checker_raw)
    else:
        # Unit tests with hand-built evidence only (governor started with dev_unsigned=True).
        # A strict governor answers REVIEW here: it never reads caller-supplied evidence.
        d = gov.evaluate(proposal, mp.evidence, checker_raw)
    reasons = [f"{r['rule']}:{r['verdict']}" for r in d["reasons"]] + [f"signal:{s['kind']}:{s['severity']}" for s in d["signals"]]
    res = ShieldResult("paid" if d["verdict"] == "allow" else d["verdict"], d, None, reasons)
    if d["verdict"] == "allow":
        # The rail receives only a beneficiary id; the account behind it is the master's.
        v = next(v for v in master["vendors"] if v["beneficiary_id"] == d["payee"]["beneficiary_id"])
        res.transfer = Transfer(v["bank_account"], v["ifsc"], proposal["amount_paise"], v["legal_name"], "via governor")
        rail.pay(res.transfer)
    return res
