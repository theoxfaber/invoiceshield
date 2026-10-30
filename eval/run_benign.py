"""Benign dev set through the real governor (checker assumed clear). Output: eval/results/benign_gate_only.jsonl."""
from __future__ import annotations

import json
import pathlib
import tempfile

from assembler.evidence import assemble
from assembler.extract import governor_evidence
from dataset.gen.make_benign import benign_cases, build
from dataset.gen.make_invoice import PROF
from shield.client import GovernorProc
from shield.pipeline import BUYER_GSTIN

ROOT = pathlib.Path(__file__).resolve().parent
CLEAR = json.dumps({"signals": []})


def main():
    rows = []
    with GovernorProc() as gov, tempfile.TemporaryDirectory() as t:
        for c in benign_cases():
            gov.reset()
            meta = build(c, pathlib.Path(t) / f"{c['id']}.pdf")
            ev = assemble(meta["pdf"], meta["cover_email"])
            prop = {"vendor_id": c["vendor_id"], "invoice_no": c["invoice_no"], "amount_paise": c["rupees"] * 100, "po_ref": c["po_ref"],
                    "currency": PROF["vendors"][c["vendor_id"]].get("currency", "INR")}
            d = gov.evaluate_document(prop, meta["pdf"], meta["cover_email"], CLEAR, now=1_793_000_000)
            rows.append({"id": c["id"], "fmt": c["fmt"], "expect": c["expect"], "verdict": d["verdict"],
                         "rules": [r["rule"] for r in d["reasons"]]})
    (ROOT / "results" / "benign_gate_only.jsonl").write_text("\n".join(json.dumps(r) for r in rows) + "\n")
    for r in rows:
        if r["verdict"] != r["expect"]:
            print("MISMATCH", r)
    print(f"{sum(r['verdict']==r['expect'] for r in rows)}/{len(rows)} as expected; "
          f"false BLOCK {sum(r['verdict']=='block' for r in rows)}")


if __name__ == "__main__":
    main()
