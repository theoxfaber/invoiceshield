"""Day 1 shield-ON arm on the same 24-case dev risk set as the OFF baseline.

    python3 -m eval.run_day1_on [--k 3] [--workers 8] [--only B01,L01,X04] [--offline]

Same cases, same input text as eval/run_day1.py. Model calls (agent + quarantined checker) run
in parallel; governor calls run serially with a ledger reset per sample. Output:
eval/results/day1_on.jsonl. This is a development set, not the frozen headline set.
"""
from __future__ import annotations

import argparse
import json
import pathlib
from concurrent.futures import ThreadPoolExecutor

from agent.rail import MockRail
from dataset.gen.make_riskset import build
from eval.run_day1 import harmful, result_name
from providers.llm import get_provider
from shield.client import GovernorProc
from shield.pipeline import gate, load_master, model_phase

ROOT = pathlib.Path(__file__).resolve().parent
RESULTS = ROOT / "results"


def summarize(rows):
    atk = [r for r in rows if not r["control"]]
    ctl = [r for r in rows if r["control"]]
    c = lambda rs, o: sum(1 for r in rs if r["outcome"] == o)
    return {
        "attack_samples": len(atk), "attack_harmful_paid": sum(1 for r in atk if r.get("harmful")),
        "attack_paid_not_harmful": sum(1 for r in atk if r["outcome"] == "paid" and not r.get("harmful")),
        "attack_blocked_by_governor": c(atk, "block"), "attack_review_by_governor": c(atk, "review"),
        "attack_agent_hold": c(atk, "agent_hold"), "attack_agent_reject": c(atk, "agent_reject"),
        "attack_agent_none": c(atk, "agent_none"),
        "control_samples": len(ctl), "control_paid": c(ctl, "paid"), "control_review": c(ctl, "review"),
        "control_block": c(ctl, "block"), "control_agent_hold": c(ctl, "agent_hold"),
        "control_agent_reject": c(ctl, "agent_reject"), "control_agent_none": c(ctl, "agent_none"),
    }


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--k", type=int, default=3)
    ap.add_argument("--workers", type=int, default=8)
    ap.add_argument("--only", default="")
    ap.add_argument("--offline", action="store_true")
    ap.add_argument("--provider", default=None,
                    help="spark (Muse Spark Open, Baseten; default) or glimmer (Muse Glimmer 30B, NIM); env LLM_PROVIDER")
    ap.add_argument("--out", default=None)
    a = ap.parse_args()
    cases = build(ROOT / "riskset")
    if a.only:
        keep = set(a.only.split(","))
        cases = [c for c in cases if c["id"] in keep]
    master = load_master()
    vendors = {v["vendor_id"]: v for v in master["vendors"]}
    provider = get_provider(a.provider, offline=a.offline, timeout_s=200.0)
    jobs = [(c, s) for c in cases for s in range(a.k)]
    with ThreadPoolExecutor(a.workers) as ex:
        phases = list(ex.map(lambda j: model_phase(provider, j[0], vendors, j[1]), jobs))
    rows = []
    with GovernorProc() as gov:
        for (case, s), mp in zip(jobs, phases):
            rail = MockRail()
            r = gate(gov, mp, master, rail)
            row = {"id": case["id"], "klass": case["klass"], "control": bool(case.get("control")), "sample": s,
                   "outcome": r.outcome, "reasons": r.reasons, "agent_kind": mp.proposal.kind,
                   "agent_error": mp.proposal.error, "agent_args": mp.proposal.args,
                   "checker_ok": mp.checker.ok if mp.checker else None,
                   "checker_signals": mp.checker.n_signals if mp.checker else None,
                   "checker_error": mp.checker.error if mp.checker else None,
                   "checker_raw": mp.checker.raw_for_governor if mp.checker else None,
                   "latency_s": mp.proposal.latency_s + (mp.checker.latency_s if mp.checker else 0.0)}
            if r.transfer:
                h, why = harmful(case, r.transfer)
                row.update(harmful=h, harm_why=why, paid_to=r.transfer.account_number, paid_amount_paise=r.transfer.amount_paise)
            rows.append(row)
        audit = gov.audit()
    RESULTS.mkdir(exist_ok=True)
    for r in rows:
        r["provider"], r["model"] = provider.name, provider.model
    (RESULTS / (a.out or result_name("day1_on", provider))).write_text("\n".join(json.dumps(r, ensure_ascii=False) for r in rows) + "\n")
    print(json.dumps(summarize(rows), indent=1))
    by = {}
    for r in rows:
        by.setdefault(r["id"], []).append(r["outcome"] + ("*" if r.get("harmful") else ""))
    for k, v in by.items():
        print(k, v)


if __name__ == "__main__":
    main()
