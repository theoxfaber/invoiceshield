"""Breakdown of the one sealed v3 run (written after scoring; reads results + labels, changes nothing).
Usage: python3 -m tools.v3_breakdown  -> eval/results/sealed-v3-freeze-v3.breakdown.json"""
from __future__ import annotations
import json, pathlib
from collections import defaultdict

REPO = pathlib.Path(__file__).resolve().parents[1]
RES = REPO / "eval" / "results" / "sealed-v3-freeze-v3.jsonl"
LAB = pathlib.Path("/workspace/blind-cases-v3/labels/labels.jsonl")
FX = pathlib.Path("/workspace/blind-cases-v3/fixtures.json")


def main() -> int:
    rows = {json.loads(x)["id"]: json.loads(x) for x in RES.read_text().splitlines() if x.strip()}
    labs = {json.loads(x)["case_id"]: json.loads(x) for x in LAB.read_text().splitlines() if x.strip()}
    fx = json.loads(FX.read_text())
    po_owner = {p["po_number"]: p["vendor_id"] for p in fx["purchase_orders"]}
    vname = {v["id"]: v["legal_name"] for v in fx["vendors"]}
    out = {"source": "eval/results/sealed-v3-freeze-v3.jsonl + labels (opened after all verdicts were written)", "arms": {}}
    for arm in ("rules", "checker_only", "both"):
        by_t = defaultdict(lambda: defaultdict(int))
        by_c = defaultdict(lambda: defaultdict(int))
        paid, blocked_benign, false_alarm = [], [], []
        for cid, r in sorted(rows.items()):
            L = labs[cid]
            v = r[arm]["verdict"]
            att = L["kind"] == "attack"
            for d, key in ((by_t, L["layout"]), (by_c, L["category"])):
                d[key]["n"] += 1
                d[key]["lenient_ok"] += v in L["acceptable"]
                d[key]["strict_ok"] += v == L["expected"]
                if att:
                    d[key]["attacks"] += 1
                    d[key]["attack_paid"] += v == "PAY"
                else:
                    d[key]["benign"] += 1
                    d[key]["benign_blocked"] += v == "BLOCK"
                    d[key]["clean_false_alarm"] += L["expected"] == "PAY" and v != "PAY"
            prop = r.get("proposal", {})
            item = {"id": cid, "category": L["category"], "layout": L["layout"], "expected": L["expected"], "got": v,
                    "description": L["description"], "doc_total_inr": L["doc_total_paise"] / 100,
                    "reasons": r[arm].get("reasons", [])}
            if att and v == "PAY":
                pv = prop.get("vendor_id")
                item.update({"paid_vendor_master": f"{pv} {vname.get(pv, '?')}", "po": prop.get("po_ref"),
                             "po_owner_matches_paid_vendor": po_owner.get(prop.get("po_ref")) == pv,
                             "payee_source": "vendor master account (never the document)"})
                paid.append(item)
            if not att and v == "BLOCK":
                blocked_benign.append(item)
            if not att and L["expected"] == "PAY" and v != "PAY":
                item["evidence"] = {k: r["evidence_summary"].get(k) for k in ("parser_lines", "model_lines", "model_lines_error", "printed_total")}
                false_alarm.append(item)
        out["arms"][arm] = {"by_template": {k: dict(v) for k, v in sorted(by_t.items())},
                            "by_category": {k: dict(v) for k, v in sorted(by_c.items())},
                            "attacks_paid": paid, "benign_blocked": blocked_benign, "clean_benign_not_paid": false_alarm}
    p = REPO / "eval" / "results" / "sealed-v3-freeze-v3.breakdown.json"
    p.write_text(json.dumps(out, indent=1, ensure_ascii=False) + "\n")
    for arm, a in out["arms"].items():
        print(arm, "paid", len(a["attacks_paid"]), "benign_blocked", len(a["benign_blocked"]), "clean_not_paid", len(a["clean_benign_not_paid"]))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
