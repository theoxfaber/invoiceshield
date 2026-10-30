"""Extraction accuracy and gate false-positive rate on public third-party invoices.

    python3 -m dataset.real.sources --fetch          # once: download + verify raw data (gitignored)
    python3 -m eval.run_real                         # both sources, writes eval/results/real-<tag>.json
    python3 -m eval.run_real --tag before            # name the run (before/after an extractor change)

What is measured (sources and labels: dataset/real/sources.py, dataset/real/load.py):

1. Field extraction. The deterministic assembler (assembler/extract.py, plus the candidate
   invoice-number/currency readers in assembler/fields.py) runs on each invoice's text and is
   scored against the labels: supplier GSTIN, IFSC, bank account, total, currency, invoice no,
   line items. Per field: labelled n, correct, missing (nothing extracted), wrong (a different
   value extracted). GSTIN/IFSC/account are scored against the TRUE value; "printed" scores
   against the token as OCR'd (what the text really says).
2. Gate false positives. Every invoice is genuine, so it is run through the real governor with
   a synthetic vendor master and PO built from its labels (vendor GSTIN/bank = the true values,
   PO open balance = the labelled total, sender domain on file, checker clear) and the agent
   proposes exactly the labelled payment. Anything but ALLOW is a false positive.
   Two settings:
     ocr_as_is      text_source="ocr", as the documents really are (scans): REVIEW by policy
     text_layer_cf  counterfactual: the same text treated as a digital PDF's text layer, so
                    only the evidence rules (GSTIN, bank, amount, lines) can fire. This is the
                    number that says how often extraction itself would stop a genuine invoice.
   The approval threshold / payout cap are raised out of the way (amount policy is not an
   extraction false positive); how many invoices the default Rs 1,00,000 threshold would send
   to REVIEW is reported separately.

Rates come with Wilson 95% intervals. Caveat: MIDD has a handful of suppliers, so invoices are
not independent draws; the intervals understate the real uncertainty (per-supplier rows given).
Raw identifiers never leave memory: the results file keeps masked values and match flags only.
"""
from __future__ import annotations

import argparse
import collections
import datetime as dt
import json
import math
import pathlib
import re
import tempfile

from assembler.evidence import Evidence
from assembler.extract import governor_evidence
from dataset.real.load import check_labels, load_midd, load_sparrow
from dataset.real.sources import RAW
from shield.client import GovernorProc

ROOT = pathlib.Path(__file__).resolve().parent
CLEAR = json.dumps({"signals": []})
DEFAULT_THRESHOLD_PAISE = 1_00_000_00
NOW = 1_793_000_000

try:  # candidate readers; absent on old commits
    from assembler.fields import printed_currency, printed_invoice_no
except ImportError:  # pragma: no cover
    printed_currency = printed_invoice_no = None


def wilson(k: int, n: int, z: float = 1.959964) -> list[float] | None:
    if n == 0:
        return None
    p = k / n
    d = 1 + z * z / n
    c = (p + z * z / (2 * n)) / d
    h = z * math.sqrt(p * (1 - p) / n + z * z / (4 * n * n)) / d
    return [round(max(0.0, c - h), 4), round(min(1.0, c + h), 4)]


def rate(k: int, n: int) -> dict:
    return {"k": k, "n": n, "rate": round(k / n, 4) if n else None, "wilson95": wilson(k, n)}


def mask(v) -> str | None:
    if v is None:
        return None
    s = str(v)
    return s if len(s) <= 4 else f"{s[:2]}…{s[-2:]}({len(s)})"


def norm_id(v) -> str:
    return re.sub(r"[^0-9A-Z]", "", str(v or "").upper())


def norm_inv(v) -> str:
    return re.sub(r"[\s\-/_.]", "", str(v or "")).upper()


def _ocr_cache(case) -> dict:
    p = RAW / "sparrow" / "ocr" / (case["id"].split("/")[1] + ".json")
    if p.exists():
        return json.loads(p.read_text())
    from assembler.evidence import assemble
    ev = assemble(case["pdf"], {})
    d = {"text": ev.text, "text_source": ev.metadata.get("text_source"), "pdf_sha256": ev.pdf_sha256}
    p.parent.mkdir(parents=True, exist_ok=True)
    p.write_text(json.dumps(d))
    return d


def evidence_for(case, buyer_gstin: str, text_source: str, sender: str) -> tuple[dict, str]:
    if case["source"] == "sparrow":
        text = _ocr_cache(case)["text"]
    else:
        text = case["text"]
    ev = Evidence(pdf_sha256=case["file_sha256"], pages=1, text=text, metadata={"text_source": text_source},
                  email_from=f"Accounts <ap@{sender}>")
    return governor_evidence(ev, buyer_gstin), text


# ---------------------------------------------------------------- extraction scoring
def score_case(case, gev, text) -> dict:
    g = case["gold"]
    out = {}

    def field(name, gold, got, printed=None, eq=lambda a, b: a == b):
        if gold is None:
            return
        r = {"gold": mask(gold), "got": mask(got), "correct": got is not None and eq(got, gold),
             "missing": got is None}
        r["wrong"] = not r["correct"] and not r["missing"]
        if printed is not None:
            r["printed_correct"] = got is not None and eq(got, printed)
        out[name] = r

    # scored only where the invoice text prints a supplier GSTIN at all (MIDD SUPP_G tag present)
    field("supplier_gstin", g.get("supplier_gstin") if g.get("supplier_gstin_printed") or case["source"] != "midd" else None,
          gev["text_layer_gstin"], g.get("supplier_gstin_printed"),
          lambda a, b: norm_id(a) == norm_id(b))
    ifsc_got = gev["printed_ifsc"] or (gev["all_printed_ifscs"][0] if gev["all_printed_ifscs"] else None)
    field("ifsc", g.get("ifsc"), ifsc_got, g.get("ifsc_printed"), lambda a, b: norm_id(a) == norm_id(b))
    acct_got = gev["printed_bank_account"] or (gev["all_printed_accounts"][0] if gev["all_printed_accounts"] else None)
    field("bank_account", g.get("bank_account"), acct_got, g.get("bank_account_printed"),
          lambda a, b: norm_id(a) == norm_id(b))
    field("total", g.get("total_paise"), gev["printed_total_paise"])
    if printed_currency is not None:
        field("currency", g.get("currency"), printed_currency(text))
    else:
        out["currency"] = {"gold": g.get("currency"), "got": None, "correct": False, "missing": True, "wrong": False,
                           "note": "no currency reader in this commit"}
    if g.get("invoice_no"):
        got = printed_invoice_no(text) if printed_invoice_no else None
        field("invoice_no", g.get("invoice_no"), got, None, lambda a, b: norm_inv(a) == norm_inv(b))
    if g.get("line_items"):
        gl = [(norm_inv(x["description"])[:12], x["amount_paise"]) for x in g["line_items"]]
        el = [l for l in gev["item_lines"] if not l["is_tax"]]
        got_amts = sorted(l["amount_paise"] for l in el)
        out["line_items"] = {"gold_n": len(gl), "got_n": len(el),
                             "correct": len(el) == len(gl) and got_amts == sorted(a for _, a in gl),
                             "missing": len(el) == 0, "wrong": len(el) > 0 and not (len(el) == len(gl) and got_amts == sorted(a for _, a in gl))}
    # spurious bank values: anything extracted that is not the supplier's (true or as printed)
    ok_acct = {norm_id(g.get("bank_account")), norm_id(g.get("bank_account_printed"))} - {""}
    ok_ifsc = {norm_id(g.get("ifsc")), norm_id(g.get("ifsc_printed"))} - {""}
    out["_spurious_accounts"] = [mask(a) for a in gev["all_printed_accounts"] if norm_id(a) not in ok_acct]
    out["_spurious_ifscs"] = [mask(a) for a in gev["all_printed_ifscs"] if norm_id(a) not in ok_ifsc]
    return out


def summarise_fields(rows) -> dict:
    s = {}
    names = sorted({k for r in rows for k in r["fields"] if not k.startswith("_")})
    for f in names:
        rs = [r["fields"][f] for r in rows if f in r["fields"]]
        n = len(rs)
        d = {"labelled": n, "correct": rate(sum(x["correct"] for x in rs), n),
             "missing": sum(x["missing"] for x in rs), "wrong": sum(x["wrong"] for x in rs)}
        if any("printed_correct" in x for x in rs):
            d["printed_correct"] = rate(sum(x.get("printed_correct", False) for x in rs), n)
        s[f] = d
    s["spurious_account_docs"] = rate(sum(bool(r["fields"]["_spurious_accounts"]) for r in rows), len(rows))
    s["spurious_ifsc_docs"] = rate(sum(bool(r["fields"]["_spurious_ifscs"]) for r in rows), len(rows))
    return s


# ---------------------------------------------------------------- gate false positives
def build_master(cases) -> tuple[dict, dict]:
    """One synthetic master: a vendor per supplier, a PO per invoice (balance = labelled total)."""
    vendors, pos, vid_of = {}, [], {}
    for c in cases:
        g = c["gold"]
        skey = c.get("supplier_key") or c["id"].replace("/", "_")
        vid = f"R_{skey}"
        vid_of[c["id"]] = vid
        if vid not in vendors:
            vendors[vid] = {
                "vendor_id": vid, "legal_name": f"Third-party supplier {skey}",
                "gstin": g.get("supplier_gstin"), "pan": g["supplier_gstin"][2:12] if g.get("supplier_gstin") else None,
                "beneficiary_id": f"BENE_{vid}",
                # No account/IFSC printed or known -> a placeholder no real text can match, so any
                # account the extractor finds on that supplier's invoices is spurious by construction.
                "bank_account": g.get("bank_account") or "000000000000",
                "ifsc": g.get("ifsc") or g.get("iban") or "NONE0000000",
                "bank_name": "on file", "email_domains": [f"{vid.lower()}.example.in"], "category": "third-party eval",
            }
        else:  # fill bank values from any invoice that prints them
            if g.get("bank_account") and vendors[vid]["bank_account"] == "000000000000":
                vendors[vid]["bank_account"] = g["bank_account"]
            if g.get("ifsc") and vendors[vid]["ifsc"] == "NONE0000000":
                vendors[vid]["ifsc"] = g["ifsc"]
        pos.append({"po_ref": f"PO-{c['id'].replace('/', '-')}", "owner_vendor_id": vid, "category": "third-party eval",
                    "open_balance_paise": g["total_paise"], "contracted_items": []})
    master = {"vendors": list(vendors.values()), "pos": pos,
              "policy": {"approval_threshold_paise": 10**12, "payout_cap_paise": 10**13, "po_tolerance_bp": 200,
                         "window_secs": 7 * 24 * 3600}}
    return master, vid_of


def run_gate(cases, setting: str) -> list[dict]:
    master, vid_of = build_master(cases)
    rows = []
    with tempfile.TemporaryDirectory() as d:
        mp = pathlib.Path(d) / "master.json"
        mp.write_text(json.dumps(master))
        vendors = {v["vendor_id"]: v for v in master["vendors"]}
        with GovernorProc(master=mp) as gov:
            for c in cases:
                gov.reset()
                g = c["gold"]
                vid = vid_of[c["id"]]
                buyer = g.get("buyer_gstin") or "27AAZFS3607C1ZY"
                ts = "ocr" if setting == "ocr_as_is" else "text_layer"
                gev, _ = evidence_for(c, buyer, ts, vendors[vid]["email_domains"][0])
                prop = {"vendor_id": vid, "invoice_no": g.get("invoice_no") or c["id"], "amount_paise": g["total_paise"],
                        "po_ref": f"PO-{c['id'].replace('/', '-')}"}
                dcs = gov.evaluate(prop, gev, CLEAR, now=NOW)
                rows.append({"id": c["id"], "source": c["source"], "group": c["group"], "supplier": c.get("supplier_key"),
                             "setting": setting, "outcome": dcs["verdict"],
                             "reasons": [f"{r['rule']}:{r['verdict']}" for r in dcs["reasons"]],
                             "tamper_alert": dcs["tamper_alert"],
                             "over_default_threshold": g["total_paise"] > DEFAULT_THRESHOLD_PAISE})
    return rows


def summarise_gate(rows) -> dict:
    n = len(rows)
    out = collections.Counter(r["outcome"] for r in rows)
    reasons = collections.Counter(x.split(":")[0] for r in rows for x in r["reasons"])
    by_sup = {}
    for s in sorted({r["supplier"] or "-" for r in rows}):
        rs = [r for r in rows if (r["supplier"] or "-") == s]
        by_sup[s] = {"n": len(rs), "fp": sum(r["outcome"] != "allow" for r in rs), "block": sum(r["outcome"] == "block" for r in rs)}
    return {"n": n, "outcomes": dict(out), "fp_any_non_allow": rate(sum(r["outcome"] != "allow" for r in rows), n),
            "fp_block": rate(out.get("block", 0), n), "reasons": dict(reasons.most_common()),
            "by_supplier": by_sup, "would_review_at_default_threshold": sum(r["over_default_threshold"] for r in rows)}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--tag", default=None)
    ap.add_argument("--sources", default="midd,sparrow")
    ap.add_argument("--no-fields", action="store_true", help="score as if assembler/fields.py did not exist (the 'before' baseline)")
    a = ap.parse_args()
    if a.no_fields:
        global printed_currency, printed_invoice_no
        printed_currency = printed_invoice_no = None
    ist = dt.timezone(dt.timedelta(hours=5, minutes=30))
    stamp = dt.datetime.now(ist).strftime("%Y-%m-%dT%H:%M IST")
    tag = a.tag or dt.datetime.now(ist).strftime("%Y%m%d-%H%M")
    result = {"run": stamp, "tag": tag, "sources": {}}
    for src in a.sources.split(","):
        cases = load_midd() if src == "midd" else load_sparrow()
        drift = check_labels(src, cases)
        if drift:
            print(f"WARNING {src}: {len(drift)} cases differ from the committed labels, e.g. {drift[:3]}")
        seen, uniq = set(), []
        for c in cases:  # MIDD ships some invoices twice under different file names
            k = c.get("text_sha256") or c["file_sha256"]
            if k not in seen:
                seen.add(k)
                uniq.append(c)
        rows = []
        for c in uniq:
            buyer = c["gold"].get("buyer_gstin") or ("27AAZFS3607C1ZY" if src == "midd" else "NONE")
            gev, text = evidence_for(c, buyer, "text_layer", "x.example.in")
            rows.append({"id": c["id"], "group": c["group"], "supplier": c.get("supplier_key"), "fields": score_case(c, gev, text)})
        gate_cases = [c for c in uniq if c["gold"].get("total_paise") and (src != "midd" or c["gold"].get("supplier_gstin"))]
        gate = {s: run_gate(gate_cases, s) for s in ("text_layer_cf", "ocr_as_is")}
        result["sources"][src] = {
            "files": len(cases), "distinct": len(uniq), "gate_cases": len(gate_cases), "label_drift": len(drift),
            "fields": summarise_fields(rows),
            "fields_by_group": {gname: summarise_fields([r for r in rows if r["group"] == gname]) for gname in sorted({r["group"] for r in rows})},
            "gate": {s: summarise_gate(rs) for s, rs in gate.items()},
            "rows": rows, "gate_rows": gate,
        }
    out = ROOT / "results" / f"real-{tag}.json"
    out.write_text(json.dumps(result, indent=1, ensure_ascii=False) + "\n")
    print("wrote", out.relative_to(ROOT.parent))
    for src, r in result["sources"].items():
        print(f"== {src}: {r['files']} files, {r['distinct']} distinct, {r['gate_cases']} in gate run")
        for f, d in r["fields"].items():
            if "labelled" in d:
                pc = f" printed-faithful {d['printed_correct']['rate']}" if "printed_correct" in d else ""
                print(f"  {f:15s} n={d['labelled']:4d} correct={d['correct']['k']:4d} ({d['correct']['rate']}, CI {d['correct']['wilson95']}) missing={d['missing']} wrong={d['wrong']}{pc}")
            else:
                print(f"  {f:15s} {d['k']}/{d['n']} docs ({d['rate']}, CI {d['wilson95']})")
        for s, gs in r["gate"].items():
            print(f"  gate[{s}] n={gs['n']} {gs['outcomes']} FP={gs['fp_any_non_allow']['k']}/{gs['n']} ({gs['fp_any_non_allow']['rate']}, CI {gs['fp_any_non_allow']['wilson95']}) BLOCK={gs['fp_block']['k']} reasons={gs['reasons']}")


if __name__ == "__main__":
    main()
