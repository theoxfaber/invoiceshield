"""Write tools/shadow_sample.csv: a SYNTHETIC pilot log for testing tools/shadow.py.

Rows reuse committed fictional invoices: 12 genuine invoices from dataset/clean, one per PO,
decisions 8 days apart (the team paid them), 8 attack invoices from eval/riskset (the team held or rejected 7 of them and, to model a
team mistake, paid the bank-swap X04), a resubmitted duplicate of the first genuine invoice
(team rejected), and one row with a typo in the decision (invalid). No real AP data.

    python3 -m tools.make_shadow_sample
"""
import csv
import datetime as dt
import json
import pathlib

REPO = pathlib.Path(__file__).resolve().parents[1]
OUT = REPO / "tools" / "shadow_sample.csv"


def main():
    clean = json.loads((REPO / "dataset/clean/manifest.json").read_text())
    risk = {c["id"]: c for c in json.loads((REPO / "eval/riskset/manifest.json").read_text())}
    rows = []
    day = 0

    def add(c, decision, note):
        nonlocal day
        day += 1
        rows.append({"invoice_file": "../" + c["pdf"], "human_decision": decision, "vendor_id": c["vendor_id"],
                     "invoice_no": c["invoice_no"], "amount": f"{c['amount_paise'] / 100:.2f}", "po_ref": c["po_ref"],
                     "sender_email": c["cover_email"]["from"], "decided_at": (dt.datetime(2026, 11, 1, 11, 0) + dt.timedelta(days=8 * day)).isoformat() + "+05:30",
                     "note": note})

    # one invoice per PO, decisions 8 days apart: genuine rows must not trip the PO-balance or
    # 7-day split rules because of how this sample is assembled
    picks, seen = [], set()
    for c in clean:
        if not c.get("group") and c.get("currency", "INR") == "INR" and c["po_ref"] not in seen:
            seen.add(c["po_ref"])
            picks.append(c)
    picks = picks[:12]
    for c in picks:
        add(c, "pay", "genuine (synthetic)")
    for cid, dec in [("X04", "pay"), ("X01", "hold"), ("X03", "reject"), ("L01", "reject"), ("L02", "reject"),
                     ("L03", "reject"), ("I02", "hold"), ("X_ifsc", "hold")]:
        add(risk[cid], dec, f"attack {risk[cid]['klass']}" + (" - team paid it (modelled team error)" if dec == "pay" else ""))
    add(picks[0], "reject", "resubmitted duplicate of the first invoice")
    add(picks[1], "aprove", "typo in decision: invalid row")
    with open(OUT, "w", newline="") as f:
        w = csv.DictWriter(f, fieldnames=list(rows[0]))
        w.writeheader()
        w.writerows(rows)
    print("wrote", OUT.relative_to(REPO), len(rows), "rows")


if __name__ == "__main__":
    main()
