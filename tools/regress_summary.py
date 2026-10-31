"""Summarise one regression run directory (tools/regress.sh) as JSON on stdout.

Counts only; per-case rows stay in the files next to it. v1 and v2 are re-runs on sets we have
already seen (NOT blind). "genuine" = benign cases whose expected verdict is PAY.
"""
from __future__ import annotations

import json
import pathlib
import sys
from collections import Counter


def rows(p: pathlib.Path) -> list[dict]:
    return [json.loads(x) for x in p.read_text().splitlines() if x.strip()] if p.exists() else []


def heldout(d: pathlib.Path, s: str) -> dict:
    sc = d / f"{s}.score.json"
    if not sc.exists():
        return {"missing": True}
    j = json.loads(sc.read_text())
    out = {}
    for arm, v in j.items():
        g = v.get("clean_benign", 0)
        fa = v.get("clean_benign_false_alarm", 0)
        out[arm] = {k: v[k] for k in ("strict", "lenient", "attacks", "attack_paid", "benign_blocked", "clean_benign", "clean_benign_false_alarm") if k in v}
        out[arm]["genuine_paid"] = f"{g - fa}/{g}"
    rr = rows(d / f"{s}.jsonl")
    out["harness_errors"] = sum("error" in r for r in rr)
    out["parser_lines_read"] = f"{sum(1 for r in rr if (r.get('evidence_summary') or {}).get('parser_lines'))}/{len(rr)}"
    return out


def main(d: str) -> int:
    d = pathlib.Path(d)
    dev = rows(d / "dev_gate_only.jsonl")
    seq = rows(d / "sequences_gate_only.jsonl")
    ben = rows(d / "benign_gate_only.jsonl")
    var = rows(d / "variants_gate_only.jsonl")
    clean = rows(d / "clean_arms.jsonl")
    abl = json.loads((d / "ablation_dev.json").read_text()) if (d / "ablation_dev.json").exists() else None
    out = {
        "dev_gate_only": {"attacks": Counter(r["verdict"] for r in dev if not r["control"]),
                          "harmful_allowed": sum(r["harmful_if_allowed"] and r["verdict"] == "allow" for r in dev if not r["control"]),
                          "controls": Counter(r["verdict"] for r in dev if r["control"])},
        "dev_sequences": {"harmful_headline": sum(r["harmful"] and not r["deliberate_limit"] for r in seq),
                          "harmful_deliberate_limits": sum(r["harmful"] and r["deliberate_limit"] for r in seq),
                          "benign_fully_allowed": f"{sum(bool(r['benign_all_allowed']) for r in seq if r['control'])}/{sum(r['control'] for r in seq)}",
                          "last_verdicts": {r["id"]: r["last_verdict"] for r in seq}},
        "dev_benign": {"as_expected": f"{sum(r['verdict'] == r['expect'] for r in ben)}/{len(ben)}",
                       "false_block": sum(r["verdict"] == "block" and r["expect"] != "block" for r in ben)},
        "dev_variants": {"verdicts": Counter(r["verdict"] for r in var),
                         "harmful_allowed": sum(r["harmful_if_allowed"] and r["verdict"] == "allow" for r in var)} if var else None,
        "dev_ablation": {a: {k: abl[a][k] for k in ("attack_cases_passed_any_sample", "attacks_passed", "attack_rows", "controls_paid", "control_rows")}
                         for a in ("rules", "checker_only", "both")} if abl else None,
        "clean": {p: dict(Counter(r["outcome"] for r in clean if r["path"] == p)) for p in sorted({r["path"] for r in clean})},
        "v1_rerun_not_blind": heldout(d, "v1"),
        "v2_rerun_not_blind": heldout(d, "v2"),
    }
    print(json.dumps(out, indent=1, default=dict))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1]))
