"""False-positive measurement: shield ON over the 60 genuine invoices in dataset/clean/.

    python3 -m eval.run_clean                 # gate path only (no model calls)
    python3 -m eval.run_clean --model         # + full shield-ON model path (agent + checker), k=1
    python3 -m eval.run_clean --model --offline   # replay the model path from eval/cache/llm
    python3 -m eval.run_clean --seed 20261101     # a fresh set with another seed (re-check after a fix)

Two paths, same governor, same ledger handling:
- gate: the agent proposes exactly the genuine payment (vendor, invoice no, printed total, PO)
  and the checker reports nothing. What is measured is the deterministic governor on the
  extracted evidence: any REVIEW/BLOCK is a governor false positive.
- model: the real shield-ON pipeline (ON agent proposes, quarantined checker reads the
  document, governor decides). Agent holds/declines count as not paid.

The ledger is reset before every independent invoice; the four part-payment sequences run in
order on one ledger with one invoice a month (the clock comes from the manifest). Nothing here
is tuned to these results; see eval/EVAL_LOG.md.
"""
from __future__ import annotations

import argparse
import collections
import datetime as dt
import json
import pathlib
from concurrent.futures import ThreadPoolExecutor

from assembler.evidence import assemble
from assembler.extract import governor_evidence
from dataset.gen.make_clean import SEED, build, manifest_hash, seed_dir
from dataset.gen.make_invoice import resolve_repo_path
from shield.client import GovernorProc
from shield.pipeline import BUYER_GSTIN, load_master, model_phase

ROOT = pathlib.Path(__file__).resolve().parent
CLEAR = json.dumps({"signals": []})


def _fresh(case, prev_group):
    """Reset the ledger unless this invoice continues the previous one's sequence."""
    return not (case["group"] and case["group"] == prev_group)


def run_gate(cases, gov):
    rows, prev = [], None
    for c in cases:
        if _fresh(c, prev):
            gov.reset()
        prev = c["group"]
        ev = assemble(str(resolve_repo_path(c["pdf"])), c["cover_email"])
        gev = governor_evidence(ev, BUYER_GSTIN)
        prop = {"vendor_id": c["vendor_id"], "invoice_no": c["invoice_no"], "amount_paise": c["amount_paise"], "po_ref": c["po_ref"],
                "currency": c.get("currency", "INR")}
        d = gov.evaluate(prop, gev, CLEAR, now=c["now"])
        rows.append({"path": "gate", "id": c["id"], "layout": c["layout"], "vendor_id": c["vendor_id"], "group": c["group"],
                     "outcome": "paid" if d["verdict"] == "allow" else d["verdict"],
                     "reasons": [f"{r['rule']}:{r['verdict']}" for r in d["reasons"]],
                     "reason_detail": [r.get("detail", "") for r in d["reasons"]],
                     "tamper_alert": d["tamper_alert"],
                     "extracted": {k: gev.get(k) for k in ("printed_bank_account", "all_printed_accounts", "all_printed_ifscs", "printed_total_paise", "currency", "all_currencies")},
                     "n_item_lines": len(gev["item_lines"])})
    return rows


def run_model(cases, gov, provider, workers):
    master = load_master()
    vendors = {v["vendor_id"]: v for v in master["vendors"]}
    abs_cases = [{**c, "pdf": str(resolve_repo_path(c["pdf"]))} for c in cases]
    with ThreadPoolExecutor(workers) as ex:
        phases = list(ex.map(lambda c: model_phase(provider, c, vendors, 0), abs_cases))
    rows, prev = [], None
    for c, mp in zip(cases, phases):
        if _fresh(c, prev):
            gov.reset()
        prev = c["group"]
        p = mp.proposal
        row = {"path": "model", "id": c["id"], "layout": c["layout"], "vendor_id": c["vendor_id"], "group": c["group"],
               "agent_kind": p.kind, "agent_error": p.error, "agent_args": p.args,
               "checker_ok": mp.checker.ok if mp.checker else None,
               "checker_error": mp.checker.error if mp.checker else None,
               "checker_raw": mp.checker.raw_for_governor if mp.checker else None,
               "latency_s": p.latency_s + (mp.checker.latency_s if mp.checker else 0.0),
               "provider": provider.name, "model": provider.model}
        if p.kind != "propose":
            row.update(outcome={"hold": "agent_hold", "reject": "agent_reject", "none": "agent_none"}[p.kind], reasons=[])
        else:
            proposal = {k: v for k, v in p.args.items() if k != "reason"}
            d = gov.evaluate(proposal, mp.evidence, mp.checker.raw_for_governor if mp.checker else "", now=c["now"])
            row.update(outcome="paid" if d["verdict"] == "allow" else d["verdict"],
                       reasons=[f"{r['rule']}:{r['verdict']}" for r in d["reasons"]] + [f"signal:{s['kind']}:{s['severity']}" for s in d["signals"]],
                       reason_detail=[r.get("detail", "") for r in d["reasons"]] + [s.get("detail", "") for s in d["signals"]],
                       proposal_matches_invoice=(proposal.get("vendor_id"), proposal.get("invoice_no"), proposal.get("amount_paise"), proposal.get("po_ref"))
                       == (c["vendor_id"], c["invoice_no"], c["amount_paise"], c["po_ref"]))
        rows.append(row)
    return rows


def summary(rows):
    return dict(collections.Counter(r["outcome"] for r in rows))


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--model", action="store_true")
    ap.add_argument("--offline", action="store_true")
    ap.add_argument("--provider", default="spark")
    ap.add_argument("--workers", type=int, default=6)
    ap.add_argument("--out", default=None)
    ap.add_argument("--seed", type=int, default=SEED, help="another seed renders a fresh 60-invoice set (dataset/clean/seed-<n>/)")
    a = ap.parse_args()
    cases = build(seed_dir(a.seed), a.seed)
    print("clean set", len(cases), "seed", a.seed, "manifest", manifest_hash(cases))
    rows = []
    with GovernorProc() as gov:
        rows += run_gate(cases, gov)
        if a.model:
            from providers.llm import get_provider
            provider = get_provider(a.provider, offline=a.offline, timeout_s=200.0)
            rows += run_model(cases, gov, provider, a.workers)
    date = dt.datetime.now(dt.timezone(dt.timedelta(hours=5, minutes=30))).date().isoformat()
    suffix = "" if a.seed == SEED else f"_seed{a.seed}"
    out = ROOT / "results" / (a.out or f"clean_fp_{date}{suffix}.jsonl")
    out.write_text("\n".join(json.dumps(r, ensure_ascii=False) for r in rows) + "\n")
    print("wrote", out.relative_to(ROOT.parent))
    for path in ("gate", "model"):
        rs = [r for r in rows if r["path"] == path]
        if not rs:
            continue
        print(path, len(rs), summary(rs))
        for r in rs:
            if r["outcome"] != "paid":
                print("  ", r["id"], r["layout"], r["outcome"].upper(), r["reasons"], r.get("agent_error") or "")


if __name__ == "__main__":
    main()
