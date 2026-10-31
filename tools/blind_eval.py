"""Run the externally authored blind case set (/workspace/blind-cases) through the real governor ONCE.

The set lives outside the repo and is never copied in. This harness only reads it and writes
results (rule names and verdicts, no case content) to eval/results/. Adapter choices were
written into docs/DECISIONS.md before the run ("Blind-case adapter", Fri Oct 30, 2026).

Per case:
  * a fresh governor whose master is built from the case's vendor master + PO, with the
    blind set's own policy (auto-pay limit ₹5,00,000 → approval threshold; payout cap ₹50,00,000;
    7-day split window; 90-day near-duplicate window);
  * the case's payment history is seeded first, as unsigned proposals with minimal evidence
    (DEV/TEST path) at their paid/invoice dates, so the ledger knows what was already paid;
  * the case PDF goes through the real sandboxed extraction service (signing key in this
    process's memory, buyer GSTIN = the blind policy's buyer), and the signed envelope plus the
    proposal (adversarial for attacks, expected for benign; bank and payee fields dropped,
    because InvoiceShield's agent schema has none) goes to the governor with a clear checker.

Usage: python3 -m tools.blind_eval --cases /workspace/blind-cases --expect-sha256 <hash> --out <name>
       python3 -m tools.blind_eval --selftest   (synthetic case built from dev fixtures; never the blind set)
"""
from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import json
import pathlib
import sys
import tempfile
from collections import Counter

from shield.client import GovernorProc

REPO = pathlib.Path(__file__).resolve().parents[1]
CLEAR = json.dumps({"signals": []})
IST = dt.timezone(dt.timedelta(hours=5, minutes=30))
VERDICT = {"allow": "PAY", "review": "REVIEW", "block": "BLOCK"}


def sha256_file(p: pathlib.Path) -> str:
    """Same as `sha256sum cases.jsonl` (raw bytes of the file), the way FROZEN.sha256 records it."""
    return hashlib.sha256(p.read_bytes()).hexdigest()


def epoch(date: str, hour: int) -> int:
    d = dt.date.fromisoformat(date)
    return int(dt.datetime(d.year, d.month, d.day, hour, tzinfo=IST).timestamp())


def minor(x) -> int:
    return int(round(float(x) * 100))


def domain(email: str | None) -> list[str]:
    return [email.split("@", 1)[1].lower()] if email and "@" in email else []


def build_master(case: dict, policy: dict) -> dict:
    vendors = []
    registered = []  # v3 prep (2026-10-31, re-runs only): the vendor's other registered accounts
    for v in case["vendor_master"]:
        accts = v.get("bank_accounts") or []
        primary = next((a for a in accts if a.get("primary")), accts[0] if accts else {})
        registered += [{"vendor_id": v["vendor_id"], "bank_account": a.get("account_number", ""), "ifsc": a.get("ifsc", "")}
                       for a in accts if a is not primary and a.get("account_number")]
        vendors.append({
            "vendor_id": v["vendor_id"], "legal_name": v["legal_name"], "gstin": v.get("gstin") or None,
            "pan": v.get("pan") or None, "beneficiary_id": f"BENE_{v['vendor_id']}",
            "bank_account": primary.get("account_number", ""), "ifsc": primary.get("ifsc", ""),
            "bank_name": primary.get("bank_name", ""), "email_domains": domain(v.get("email")), "category": "",
        })
    po = case["purchase_order"]
    pos = {po["po_number"]: {
        "po_ref": po["po_number"], "owner_vendor_id": po["vendor_id"], "category": "", "currency": po.get("currency", "INR"),
        "open_balance_paise": minor(po["open_balance_incl_tax"]),
        "lines": [{"item": ln["description"], "aliases": [ln["item_code"]] if ln.get("item_code") else [],
                   "qty": int(ln["qty_open"]), "unit_price_minor": minor(ln["unit_price"]), "unit": ln.get("uom") or "lot"}
                  for ln in po.get("lines", [])],
    }}
    # Seeded history counts against its PO in the governor; add it back so the case PO's available
    # balance stays the case's own open balance, and give history on other POs a PO of its own
    # whose balance is exactly that history.
    seeded: dict[str, int] = {}
    owner: dict[str, str] = {}
    for h in case.get("payment_history", []):
        seeded[h["po_reference"]] = seeded.get(h["po_reference"], 0) + minor(h["amount"])
        owner.setdefault(h["po_reference"], h["vendor_id"])
    for ref, amt in seeded.items():
        if ref in pos:
            pos[ref]["open_balance_paise"] += amt
        else:
            pos[ref] = {"po_ref": ref, "owner_vendor_id": owner[ref], "category": "", "currency": "INR",
                        "open_balance_paise": amt, "lines": []}
    auto = minor(policy.get("auto_pay_limit_inr", 500000))
    out = {"vendors": vendors, "pos": list(pos.values()), "policy": {
        "approval_threshold_paise": auto, "payout_cap_paise": auto * 10, "po_tolerance_bp": 200,
        "window_secs": int(policy.get("split_window_days", 7)) * 86400,
        "near_duplicate_window_secs": int(policy.get("duplicate_window_days", 90)) * 86400,
    }}
    if registered:
        out["registered_accounts"] = registered
    return out


def proposal(case: dict) -> dict:
    p = case.get("adversarial_proposal") or case.get("expected_proposal")
    out = {"vendor_id": p["vendor_id"], "invoice_no": p["invoice_number"], "amount_paise": minor(p["amount"]),
           "currency": p.get("currency", "INR"), "po_ref": p["po_reference"]}
    irn = (case.get("invoice", {}).get("header") or {}).get("irn")
    if irn:
        out["irn"] = irn
    return out


class InProcExtractor:
    """The real extraction service object (sandboxed extraction + Ed25519 signing), in this process."""

    def __init__(self, buyer_gstin: str):
        from extractor.service import Extractor
        self.ex = Extractor(buyer_gstin=buyer_gstin)
        self.public_key_hex = self.ex.public_hex

    def extract(self, doc: bytes) -> dict:
        return self.ex.extract(doc, {})


def run_case(case: dict, pdf: bytes, policy: dict, ex: InProcExtractor, tmp: pathlib.Path) -> dict:
    master = tmp / f"master-{case['id']}.json"
    master.write_text(json.dumps(build_master(case, policy)))
    vend = {v["vendor_id"]: v for v in case["vendor_master"]}
    row = {"id": case["id"], "kind": case["kind"], "category": case["category"], "expected": case["expected_verdict"],
           "acceptable": case["acceptable_verdicts"], "seeds": []}
    with GovernorProc(master, dev_unsigned=True, extra_env={"GOVERNOR_EXTRACTOR_PUBKEY": ex.public_key_hex}) as gov:
        last = 0
        for h in case.get("payment_history", []):
            ts = epoch(h.get("paid_on") or h["invoice_date"], 10)
            last = max(last, ts)
            v = vend.get(h["vendor_id"], {})
            seed = {"vendor_id": h["vendor_id"], "invoice_no": h["invoice_number"], "amount_paise": minor(h["amount"]),
                    "currency": "INR", "po_ref": h["po_reference"]}
            ev = {"text_layer_gstin": v.get("gstin") or None, "has_text_layer": True, "text_source": "text_layer"}
            d = gov.evaluate(seed, ev, CLEAR, now=ts)
            row["seeds"].append({"verdict": VERDICT[d["verdict"]], "rules": [r["rule"] for r in d["reasons"]]})
        now = max(epoch(case["invoice"]["header"]["invoice_date"], 12), last + 3600)
        env = ex.extract(pdf)
        status = json.loads(env["payload"]).get("status")
        d = gov.evaluate_signed(proposal(case), env, CLEAR, hashlib.sha256(pdf).hexdigest(), now=now)
    v = VERDICT[d["verdict"]]
    row.update({"extraction_status": status, "verdict": v, "deterministic_verdict": VERDICT[d["deterministic_verdict"]],
                "reasons": [f"{r['rule']}:{r['verdict']}" for r in d["reasons"]], "tamper_alert": d["tamper_alert"],
                "strict": v == case["expected_verdict"], "lenient": v in case["acceptable_verdicts"],
                "attack_paid": case["kind"] == "attack" and v == "PAY",
                "benign_blocked": case["kind"] == "benign" and v == "BLOCK"})
    return row


def summarize(rows: list[dict]) -> dict:
    att = [r for r in rows if r["kind"] == "attack"]
    ben = [r for r in rows if r["kind"] == "benign"]
    return {
        "cases": len(rows), "attacks": len(att), "benign": len(ben),
        "strict_match": sum(r["strict"] for r in rows), "lenient_match": sum(r["lenient"] for r in rows),
        "attack_strict": sum(r["strict"] for r in att), "attack_lenient": sum(r["lenient"] for r in att),
        "benign_strict": sum(r["strict"] for r in ben), "benign_lenient": sum(r["lenient"] for r in ben),
        "attack_paid": sum(r["attack_paid"] for r in att), "benign_blocked": sum(r["benign_blocked"] for r in ben),
        "attack_verdicts": dict(Counter(r["verdict"] for r in att)), "benign_verdicts": dict(Counter(r["verdict"] for r in ben)),
        "extraction_failed": sum(r["extraction_status"] != "ok" for r in rows),
    }


def selftest() -> int:
    """Synthetic case in the blind schema built from the dev fixture (B01). Never touches the blind set."""
    fx = json.loads((REPO / "dataset" / "fixtures" / "master.json").read_text())
    v = next(x for x in fx["vendors"] if x["vendor_id"] == "V001")
    manifest = json.loads((REPO / "eval" / "riskset" / "manifest.json").read_text())
    b01 = next(c for c in manifest if c.get("id") == "B01")
    case = {"id": "SELF-01", "kind": "benign", "category": "selftest", "expected_verdict": "PAY", "acceptable_verdicts": ["PAY"],
            "vendor_master": [{"vendor_id": "V001", "legal_name": v["legal_name"], "gstin": v["gstin"], "pan": v["pan"],
                               "email": "x@" + v["email_domains"][0],
                               "bank_accounts": [{"account_number": v["bank_account"], "ifsc": v["ifsc"], "bank_name": v["bank_name"], "primary": True}]}],
            "purchase_order": {"po_number": b01["po_ref"], "vendor_id": "V001", "currency": "INR", "open_balance_incl_tax": 80000.0,
                               "lines": [{"description": "Stationery", "qty_open": 1, "unit_price": 80000.0, "uom": "lot"}]},
            "payment_history": [{"vendor_id": "V001", "invoice_number": "SELF/OLD/1", "amount": 1000, "po_reference": "PO-OTHER",
                                 "invoice_date": "2026-10-01", "paid_on": "2026-10-02", "status": "PAID"}],
            "invoice": {"header": {"invoice_date": "2026-10-20", "irn": None}},
            "expected_proposal": {"vendor_id": "V001", "invoice_number": b01["invoice_no"], "amount": b01["amount_paise"] / 100,
                                  "currency": "INR", "po_reference": b01["po_ref"]}}
    pdf = (REPO / "eval" / "riskset" / "B01.pdf").read_bytes()
    from extractor.service import DEFAULT_BUYER_GSTIN
    ex = InProcExtractor(DEFAULT_BUYER_GSTIN)
    with tempfile.TemporaryDirectory() as t:
        row = run_case(case, pdf, {}, ex, pathlib.Path(t))
    print(json.dumps(row))
    return 0


def main(argv=None) -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--cases", default="/workspace/blind-cases")
    ap.add_argument("--expect-sha256", default=None)
    ap.add_argument("--out", default=None)
    ap.add_argument("--selftest", action="store_true")
    a = ap.parse_args(argv)
    if a.selftest:
        return selftest()
    root = pathlib.Path(a.cases)
    got = sha256_file(root / "cases.jsonl")
    print("cases.jsonl sha256", got)
    if a.expect_sha256 is None or got != a.expect_sha256:
        print("HASH MISMATCH or no expected hash: refusing to run", file=sys.stderr)
        return 3
    frozen = (root / "FROZEN.sha256").read_text().split()[0]
    print("FROZEN.sha256 says", frozen, "match" if frozen == got else "MISMATCH")
    if frozen != got:
        return 3
    policy = json.loads((root / "policy.json").read_text())
    cases = [json.loads(line) for line in (root / "cases.jsonl").read_text().splitlines() if line.strip()]
    ex = InProcExtractor(policy["buyer"]["gstin"])
    out = REPO / "eval" / "results" / (a.out or "blind-cases-run1.jsonl")
    rows = []
    with tempfile.TemporaryDirectory() as t:
        for c in cases:
            try:
                r = run_case(c, (root / c["pdf"]).read_bytes(), policy, ex, pathlib.Path(t))
            except Exception as e:  # recorded, never retried
                r = {"id": c["id"], "kind": c["kind"], "category": c["category"], "expected": c["expected_verdict"],
                     "acceptable": c["acceptable_verdicts"], "verdict": "HARNESS_ERROR", "error": f"{type(e).__name__}: {e}"[:300],
                     "strict": False, "lenient": False, "attack_paid": False, "benign_blocked": False, "extraction_status": "n/a"}
            rows.append(r)
            print(r["id"], r["expected"], "->", r["verdict"], r.get("reasons", r.get("error")), flush=True)
            out.write_text("\n".join(json.dumps(x, ensure_ascii=False) for x in rows) + "\n")
    s = summarize(rows)
    s.update({"cases_sha256": got, "harness_errors": sum(r["verdict"] == "HARNESS_ERROR" for r in rows)})
    out.with_suffix(".summary.json").write_text(json.dumps(s, indent=1) + "\n")
    print(json.dumps(s))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
