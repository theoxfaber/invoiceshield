"""Shadow mode: run InvoiceShield next to a human AP team and report where it agrees.

Nothing is paid. The governor runs in eval mode on a throwaway ledger; the CSV records what the
humans actually did, and this tool only compares. This is the harness for a real pilot; it has
been run on a synthetic CSV only (tools/shadow_sample.csv, tests/test_shadow.py), never on a real
AP team's decisions.

    python3 -m tools.shadow decisions.csv --master vendor_master.json --buyer-gstin 24ABCDE1234F1Z5 \
        [--out shadow_report]   # writes shadow_report.json + shadow_report.md

CSV columns (header row required; extra columns are carried through):
  invoice_file     path to the invoice PDF (relative to the CSV's folder or absolute)
  human_decision   pay | hold | reject   (approve/paid -> pay; review/query -> hold; block -> reject)
  vendor_id        master vendor the human booked it to
  invoice_no       invoice number as booked
  amount           amount as booked, rupees ("59000.00") - or amount_paise
  po_ref           PO it was booked against
  currency         ISO 4217 code of the booked payment (optional; governor default INR)
  sender_email     From: address of the email that carried the invoice   (optional)
  decided_at       ISO date/time of the human decision (optional; orders the shared ledger and
                   drives the duplicate / split-payment windows; default: CSV order, one day apart)
  checker_raw      optional quarantined-checker JSON to replay; default: no signals

Rows run in decided_at order on ONE ledger, as in production, so duplicates and splits across
the pilot are seen. A human "pay" leaves the shield's ALLOW/REVIEW entry in the ledger; a human
hold/reject still leaves the shield's own entry (the shield does not know the human outcome).

Agreement: shield ALLOW = would pay; REVIEW/BLOCK = would not pay without a person.
  pay  vs ALLOW          agree
  pay  vs REVIEW         shield more cautious (cost: one extra look)
  pay  vs BLOCK          disagree (shield refused something the team paid: false positive, or a
                         miss by the team; read the reasons)
  hold/reject vs ALLOW   disagree (the team stopped something the shield would pay: the case that
                         matters most; read it)
  hold/reject vs REVIEW/BLOCK   agree
"""
from __future__ import annotations

import argparse
import csv
import datetime as dt
import json
import math
import pathlib
import sys

from assembler.evidence import assemble
from assembler.extract import governor_evidence
from shield.client import GovernorProc

try:  # Phase 2 builds: the governor reads only evidence signed by the extraction service
    from shield.client import ExtractorProc  # noqa: F401
    from extractor.service import DEFAULT_BUYER_GSTIN as SERVICE_BUYER_GSTIN
    SIGNED = True
except ImportError:  # pragma: no cover
    SIGNED, SERVICE_BUYER_GSTIN = False, None

CLEAR = json.dumps({"signals": []})
_HUMAN = {"pay": "pay", "paid": "pay", "approve": "pay", "approved": "pay", "allow": "pay",
          "hold": "hold", "review": "hold", "query": "hold", "pending": "hold",
          "reject": "reject", "rejected": "reject", "block": "reject", "fraud": "reject"}


def wilson(k: int, n: int, z: float = 1.959964):
    if n == 0:
        return None
    p, d = k / n, 1 + z * z / n
    c, h = (p + z * z / (2 * n)) / d, z * math.sqrt(p * (1 - p) / n + z * z / (4 * n * n)) / d
    return [round(max(0.0, c - h), 4), round(min(1.0, c + h), 4)]


def _paise(row) -> int:
    if row.get("amount_paise"):
        return int(row["amount_paise"])
    r, _, p = row["amount"].replace(",", "").replace("₹", "").strip().partition(".")
    return int(r) * 100 + int((p + "00")[:2])


def _ts(row, i: int, base: int) -> int:
    v = (row.get("decided_at") or "").strip()
    if not v:
        return base + i * 86_400
    t = dt.datetime.fromisoformat(v)
    if t.tzinfo is None:
        t = t.replace(tzinfo=dt.timezone(dt.timedelta(hours=5, minutes=30)))  # IST unless stated
    return int(t.timestamp())


def classify(human: str, shield: str) -> str:
    would_pay = shield == "allow"
    if human == "pay":
        return "agree" if would_pay else ("shield_more_cautious" if shield == "review" else "disagree_shield_refused")
    return "disagree_shield_would_pay" if would_pay else "agree"


def kappa(rows) -> float | None:
    """Cohen's kappa on pay / not-pay."""
    n = len(rows)
    if not n:
        return None
    a = [r["human"] == "pay" for r in rows]
    b = [r["shield"] == "allow" for r in rows]
    po = sum(x == y for x, y in zip(a, b)) / n
    pa, pb = sum(a) / n, sum(b) / n
    pe = pa * pb + (1 - pa) * (1 - pb)
    return None if pe == 1 else round((po - pe) / (1 - pe), 4)


def run(csv_path: pathlib.Path, master: pathlib.Path, buyer_gstin: str, base_ts: int = 1_793_000_000) -> dict:
    with open(csv_path, newline="", encoding="utf-8") as f:
        rows = list(csv.DictReader(f))
    order = sorted(range(len(rows)), key=lambda i: (_ts(rows[i], i, base_ts), i))
    out = []
    # Signed (production) path when the extraction service reads with this buyer GSTIN; the
    # service's buyer is fixed at startup, so another buyer falls back to the DEV/TEST unsigned
    # path with the same extractor run in-process. The report records which path was used.
    signed = SIGNED and buyer_gstin == SERVICE_BUYER_GSTIN
    with GovernorProc(master=master, **({} if signed or not SIGNED else {"dev_unsigned": True})) as gov:
        gov.reset()
        for i in order:
            row = rows[i]
            human = _HUMAN.get((row.get("human_decision") or "").strip().lower())
            rec = {"row": i + 2, "invoice_file": row.get("invoice_file"), "human_raw": row.get("human_decision")}
            if human is None:
                rec.update(error="unknown human_decision", human=None, shield=None, agreement="invalid_row")
                out.append(rec)
                continue
            pdf = pathlib.Path(row["invoice_file"])
            if not pdf.is_absolute():
                pdf = csv_path.parent / pdf
            try:
                prop = {"vendor_id": row["vendor_id"], "invoice_no": row["invoice_no"], "amount_paise": _paise(row),
                        "po_ref": row["po_ref"]}
                if (row.get("currency") or "").strip():
                    prop["currency"] = row["currency"].strip().upper()
                cover = {"from": row.get("sender_email", "")}
                if signed:  # production path: PDF -> sandboxed extraction service -> signed evidence -> governor
                    d = gov.evaluate_document(prop, pdf, cover, row.get("checker_raw") or CLEAR, now=_ts(row, i, base_ts))
                else:
                    gev = governor_evidence(assemble(pdf, cover), buyer_gstin)
                    d = gov.evaluate(prop, gev, row.get("checker_raw") or CLEAR, now=_ts(row, i, base_ts))
                shield = d["verdict"]
                rec.update(shield=shield, reasons=[f"{r['rule']}:{r['verdict']}" for r in d["reasons"]],
                           reason_detail=[r.get("detail", "") for r in d["reasons"]], tamper_alert=d["tamper_alert"],
                           decision_id=d["decision_id"])
            except Exception as e:  # unreadable PDF, bad row: the shield would not pay it either
                shield = "review"
                rec.update(shield=shield, reasons=[f"harness_error:{type(e).__name__}"], reason_detail=[str(e)[:200]])
            rec.update(human=human, agreement=classify(human, shield))
            out.append(rec)
    valid = [r for r in out if r["agreement"] != "invalid_row"]
    n = len(valid)
    agree = sum(r["agreement"] == "agree" for r in valid)
    matrix = {h: {s: sum(1 for r in valid if r["human"] == h and r["shield"] == s) for s in ("allow", "review", "block")}
              for h in ("pay", "hold", "reject")}
    return {
        "csv": str(csv_path), "master": str(master), "rows": len(rows), "valid": n,
        "evidence_path": "signed" if signed else ("dev_unsigned" if SIGNED else "unsigned"),
        "agreement_strict": {"k": agree, "n": n, "rate": round(agree / n, 4) if n else None, "wilson95": wilson(agree, n)},
        "pay_decision_agreement": {"k": sum((r["human"] == "pay") == (r["shield"] == "allow") for r in valid), "n": n},
        "cohen_kappa_pay_vs_not": kappa(valid),
        "matrix_human_x_shield": matrix,
        "counts": {k: sum(r["agreement"] == k for r in out) for k in
                   ("agree", "shield_more_cautious", "disagree_shield_refused", "disagree_shield_would_pay", "invalid_row")},
        "shield_would_pay_what_team_stopped": [r for r in out if r["agreement"] == "disagree_shield_would_pay"],
        "shield_refused_what_team_paid": [r for r in out if r["agreement"] == "disagree_shield_refused"],
        "decisions": out,
    }


def markdown(rep: dict) -> str:
    a = rep["agreement_strict"]
    m = rep["matrix_human_x_shield"]
    lines = [
        "# InvoiceShield shadow-mode report", "",
        f"CSV `{rep['csv']}`: {rep['rows']} rows, {rep['valid']} valid. Nothing was paid; the governor ran on a throwaway ledger.", "",
        f"**Agreement (pay vs not-pay, REVIEW on a paid invoice counts as a disagreement here): {a['k']}/{a['n']} = {a['rate']}** "
        f"(Wilson 95% {a['wilson95']}); Cohen's kappa {rep['cohen_kappa_pay_vs_not']}.", "",
        "| human \\ shield | ALLOW | REVIEW | BLOCK |", "|---|---|---|---|",
    ] + [f"| {h} | {m[h]['allow']} | {m[h]['review']} | {m[h]['block']} |" for h in ("pay", "hold", "reject")] + [
        "", "Counts: " + ", ".join(f"{k} {v}" for k, v in rep["counts"].items()), "",
        "## Team stopped it, shield would have paid (read every one)", ""]
    for r in rep["shield_would_pay_what_team_stopped"] or [{"invoice_file": "none"}]:
        lines.append(f"- row {r.get('row', '-')}: `{r['invoice_file']}` human={r.get('human_raw', '')}")
    lines += ["", "## Team paid it, shield refused (BLOCK)", ""]
    for r in rep["shield_refused_what_team_paid"] or [{"invoice_file": "none", "reasons": []}]:
        lines.append(f"- row {r.get('row', '-')}: `{r['invoice_file']}` {', '.join(r.get('reasons', []))}")
    return "\n".join(lines) + "\n"


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("csv")
    ap.add_argument("--master", required=True)
    ap.add_argument("--buyer-gstin", required=True)
    ap.add_argument("--out", default=None, help="output prefix (default: <csv stem>_shadow)")
    a = ap.parse_args(argv)
    csv_path = pathlib.Path(a.csv).resolve()
    rep = run(csv_path, pathlib.Path(a.master).resolve(), a.buyer_gstin)
    prefix = pathlib.Path(a.out) if a.out else csv_path.with_name(csv_path.stem + "_shadow")
    prefix.with_suffix(".json").write_text(json.dumps(rep, indent=1, ensure_ascii=False) + "\n")
    prefix.with_suffix(".md").write_text(markdown(rep))
    print(markdown(rep))
    return 0


if __name__ == "__main__":
    sys.exit(main())
