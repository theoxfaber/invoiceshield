"""Builds ui/demo.html: one self-contained page replaying recorded runs of the same invoice
with the shield OFF (unshielded agent paid it) and ON (governor decision + reasons).
No model calls, nothing waits: replies come from eval/results (real Glimmer runs, sample 0).
Run from repo root: python3 -m ui.build_demo
"""
from __future__ import annotations
import base64, json, pathlib, subprocess, tempfile

ROOT = pathlib.Path(__file__).resolve().parents[1]
CASES = ["X04", "L02", "L03", "X_ifsc", "B01"]
TITLES = {
    "X04": "Bank details swapped in the PDF", "L02": "Look-alike sender domain",
    "L03": "Look-alike supplier tax ID (GSTIN)", "X_ifsc": "Same account, new IFSC branch",
    "B01": "Genuine invoice (control)"}
WHY = {
    "bank_details_differ_from_master": "The account on the PDF is not the one in the trusted vendor master.",
    "sender_domain_mismatch": "The email came from a domain that is not on the vendor's record.",
    "issuer_gstin_mismatch": "The tax ID printed on the invoice is not this vendor's.",
    "injection_suspected": "The quarantined checker saw instructions aimed at an AI agent.",
    "bank_box_mismatch": "The bank box on the page differs from the text layer.",
}
def inr(p): return "₹{:,.2f}".format(p / 100)

def load(name):
    out = {}
    for l in (ROOT / "eval/results" / name).read_text().splitlines():
        d = json.loads(l)
        if d["sample"] == 0: out[d["id"]] = d
    return out

def png_b64(pdf):
    with tempfile.TemporaryDirectory() as t:
        subprocess.run(["pdftoppm", "-png", "-r", "70", "-singlefile", str(pdf), f"{t}/p"], check=True)
        return base64.b64encode(pathlib.Path(f"{t}/p.png").read_bytes()).decode()

def main():
    off, on = load("day1_off.jsonl"), load("day1_on.jsonl")
    man = {c["id"]: c for c in json.loads((ROOT / "eval/riskset/manifest.json").read_text())}
    cases = []
    for cid in CASES:
        o, n, m = off[cid], on[cid], man[cid]
        a = o["tool"]["arguments"]
        reasons = []
        for r in n["reasons"]:
            parts = r.split(":")
            if parts[0] == "signal": key, sev = parts[1], parts[2]
            else: key, sev = parts[0], parts[1]
            reasons.append({"rule": key, "severity": sev, "text": WHY.get(key, key)})
        cases.append({
            "id": cid, "title": TITLES[cid], "image": png_b64(ROOT / "eval/riskset" / f"{cid}.pdf"),
            "email_from": m["cover_email"]["from"], "invoice_no": m["invoice_no"],
            "off": {"decision": o["decision"], "payee": a.get("payee_name"), "account": str(a.get("account_number")),
                    "ifsc": a.get("ifsc"), "amount": inr(o["paid_amount_paise"]), "harmful": bool(o.get("harmful")),
                    "why": o.get("harm_why") or "", "agent_reason": a.get("reason", "")},
            "on": {"outcome": n["outcome"], "reasons": reasons, "agent_reason": n["agent_args"].get("reason", ""),
                   "amount": inr(n["agent_args"].get("amount_paise", 0)),
                   "trusted_account": m["legit"]["account"], "trusted_ifsc": m["legit"]["ifsc"]},
        })
    tpl = (ROOT / "ui/demo.template.html").read_text()
    (ROOT / "ui/demo.html").write_text(tpl.replace("__DATA__", json.dumps(cases)))
    print("wrote ui/demo.html", len(cases), "cases")

if __name__ == "__main__": main()
