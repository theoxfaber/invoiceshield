"""What the model arm changes, from the saved regression files only (no runs, no model calls).

    python -m tools.model_marginal eval/results/regress-after-d7cdfb6 > eval/results/model_marginal.json

For v1 and v2 (re-runs after fixes on sets we have already seen; NOT blind) it lists every case
where the `both` arm (rules + quarantined checker + model line proposals) reaches a different
verdict from `rules` alone, says whether the change moved the verdict toward or away from the
label, and gives the genuine-invoice pay rate per arm ("genuine" = benign cases whose expected
verdict is PAY). Labels come from the case sets themselves (same loaders as tools.heldout_eval).
For the dev ablation and the clean set it copies the aggregate counts from the saved summaries
(those files have no per-case rules row to pair with).
"""
from __future__ import annotations

import json
import pathlib
import sys
from collections import Counter

REPO = pathlib.Path(__file__).resolve().parents[1]
ARMS = ("rules", "checker_only", "both")


def rows(p: pathlib.Path) -> list[dict]:
    return [json.loads(x) for x in p.read_text().splitlines() if x.strip()]


def labels(which: str) -> dict:
    from tools.heldout_eval import ROOTS

    out = {}
    if which == "v2":
        for lab in rows(ROOTS["v2"] / "labels" / "labels.jsonl"):
            out[lab["case_id"]] = {"expected": lab["expected"], "acceptable": lab["acceptable"], "kind": lab["kind"],
                                   "category": lab.get("category")}
    else:
        for c in rows(ROOTS["v1"] / "cases.jsonl"):
            out[c["id"]] = {"expected": c["expected_verdict"], "acceptable": c["acceptable_verdicts"], "kind": c["kind"],
                            "category": c["category"]}
    return out


def grade(v: str, lab: dict) -> int:
    """2 = expected verdict, 1 = acceptable only, 0 = not acceptable."""
    return 2 if v == lab["expected"] else 1 if v in lab["acceptable"] else 0


def heldout(d: pathlib.Path, which: str) -> dict:
    labs = labels(which)
    rr = rows(d / f"{which}.jsonl")
    changed, pay = [], {a: Counter() for a in ARMS}
    for r in rr:
        lab = labs[r["id"]]
        v = {a: r.get(a, {}).get("verdict", "HARNESS_ERROR") for a in ARMS}
        if lab["kind"] == "benign" and lab["expected"] == "PAY":
            for a in ARMS:
                pay[a]["genuine"] += 1
                pay[a]["paid"] += v[a] == "PAY"
        if v["both"] != v["rules"]:
            gr, gb = grade(v["rules"], lab), grade(v["both"], lab)
            changed.append({
                "id": r["id"], "kind": lab["kind"], "category": lab["category"], "expected": lab["expected"],
                "acceptable": lab["acceptable"], "rules": v["rules"], "both": v["both"],
                "effect": "better" if gb > gr else "worse" if gb < gr else "same_grade",
                "attack_payment_stopped": lab["kind"] == "attack" and v["rules"] == "PAY" and v["both"] != "PAY",
                "genuine_payment_lost": lab["kind"] == "benign" and lab["expected"] == "PAY" and v["rules"] == "PAY" and v["both"] != "PAY",
                "both_reasons": r.get("both", {}).get("reasons"),
                "both_signals": r.get("both", {}).get("signals"),
                "model_lines_offered": r.get("both", {}).get("model_lines_offered"),
            })
    eff = Counter(c["effect"] for c in changed)
    return {
        "source": str((d / f"{which}.jsonl").relative_to(REPO)),
        "not_blind": True,
        "cases": len(rr),
        "cases_where_model_changes_verdict": len(changed),
        "better": eff["better"], "worse": eff["worse"], "same_grade": eff["same_grade"],
        "attack_payments_stopped_by_model": sum(c["attack_payment_stopped"] for c in changed),
        "genuine_payments_lost_to_model": sum(c["genuine_payment_lost"] for c in changed),
        "genuine_pay_rate": {a: f"{pay[a]['paid']}/{pay[a]['genuine']}" for a in ARMS},
        "changed": changed,
    }


def main(d: str) -> int:
    d = pathlib.Path(d).resolve()
    abl = json.loads((d / "ablation_dev.json").read_text())
    clean = Counter((r["path"], r["outcome"]) for r in rows(d / "clean_arms.jsonl"))
    paths = sorted({p for p, _ in clean})
    out = {
        "generated_by": "python -m tools.model_marginal " + str(d.relative_to(REPO)),
        "note": "v1/v2 are re-runs after fixes on sets we have already seen: NOT blind. 'both' = rules + quarantined checker + model line proposals.",
        "v1": heldout(d, "v1"),
        "v2": heldout(d, "v2"),
        "dev_ablation": {
            "source": str((d / "ablation_dev.json").relative_to(REPO)),
            "per_case_pairing": False,
            **{a: {k: abl[a][k] for k in ("attack_rows", "attacks_passed", "attack_cases_passed_any_sample", "control_rows", "controls_paid")}
               for a in ARMS},
        },
        "clean": {
            "source": str((d / "clean_arms.jsonl").relative_to(REPO)),
            "per_path": {p: {o: n for (pp, o), n in sorted(clean.items()) if pp == p} for p in paths},
        },
    }
    print(json.dumps(out, indent=1, ensure_ascii=False))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1] if len(sys.argv) > 1 else str(REPO / "eval" / "results" / "regress-after-d7cdfb6")))
