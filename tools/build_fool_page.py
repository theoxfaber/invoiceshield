"""Build the static "Fool the Shield" replay page (docs/fool-the-shield/index.html).

Post-freeze verdicts only: by default the blind held-out v3 run on freeze-v3 (--set v2 rebuilds the older v2 run 2 at 55fb679) (rules / checker-only / both) and the
demo opening case OC1. No case documents are copied; only ids, the set's category labels, verdicts
and rule names. The page makes no network calls and needs no account; it is ready for GitHub Pages
(serve /docs). Its UI says "Replaying recorded runs".

    python3 -m tools.build_fool_page [--set v3|v2] [--labels <labels.jsonl>]
"""
from __future__ import annotations

import argparse
import html
import json
import pathlib

REPO = pathlib.Path(__file__).resolve().parents[1]
RUN = "heldout-v2-run2-55fb679"
SETS = {
    "v2": {"run": "heldout-v2-run2-55fb679", "commit": "55fb679", "labels": "/workspace/blind-cases-v2/labels/labels.jsonl",
           "title": "Choose an attack from the second held-out set",
           "blurb": "Written after our first blind test by a separate AI agent with no access to the code; its attack categories were informed by that test's failures. The first attempt to run it crashed before producing any verdict."},
    "v3": {"run": "sealed-v3-freeze-v3", "commit": "5c698b0 (freeze-v3)", "labels": "/workspace/blind-cases-v3/labels/labels.jsonl",
           "title": "Choose an attack from the third held-out set (blind, run once)",
           "blurb": "150 invoices (100 attacks, 50 benign), sealed and hashed before the gate saw them, written by a separate AI agent with no repo access, run once on the frozen gate code; labels opened after every verdict was written. Earlier sets v1/v2 are re-runs on documents we had already seen and are not blind."},
}


def data(labels_path: pathlib.Path, run: str = RUN, commit: str = "55fb679") -> dict:
    labs = {}
    for line in labels_path.read_text().splitlines():
        if line.strip():
            x = json.loads(line)
            labs[x["case_id"]] = {"kind": x["kind"], "category": x["category"], "expected": x["expected"]}
    rows = [json.loads(x) for x in (REPO / "eval" / "results" / f"{run}.jsonl").read_text().splitlines() if x.strip()]
    cases = []
    for r in rows:
        lab = labs[r["id"]]
        cases.append({"id": r["id"], **lab,
                      "arms": {a: {"verdict": r[a]["verdict"],
                                   "reasons": [x.split(":")[0] for x in r[a].get("reasons", []) if not x.endswith(":allow")]}
                               for a in ("rules", "checker_only", "both")}})
    oc = json.loads((REPO / "eval" / "results" / "demo-opening-case.json").read_text())
    score = json.loads((REPO / "eval" / "results" / f"{run}.score.json").read_text())
    return {"run": run, "commit": commit, "cases": cases,
            "summary": {a: {k: score[a][k] for k in ("attacks", "attack_paid", "clean_benign", "clean_benign_false_alarm")} for a in score},
            "opening": {"verdict": oc["arms"]["both"]["verdict"].upper(),
                        "reasons": [x["rule"] for x in oc["arms"]["both"]["reasons"]]}}


PAGE = """<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src 'unsafe-inline'; script-src 'unsafe-inline'">
<title>Fool the Shield: InvoiceShield replay</title>
<style>
body{font:16px/1.5 system-ui,-apple-system,Segoe UI,Roboto,sans-serif;margin:0;background:#f6f7f9;color:#14171f}
header{background:#14171f;color:#fff;padding:20px 24px}header h1{margin:0;font-size:24px}
.banner{display:inline-block;margin-top:8px;background:#ffd166;color:#14171f;font-weight:600;padding:3px 10px;border-radius:999px;font-size:13px}
main{max-width:980px;margin:0 auto;padding:20px}
.card{background:#fff;border:1px solid #e3e6ec;border-radius:12px;padding:16px 18px;margin:14px 0}
.grid{display:grid;grid-template-columns:repeat(auto-fit,minmax(210px,1fr));gap:10px}
button.cat{text-align:left;border:1px solid #d5d9e2;background:#fff;border-radius:10px;padding:10px 12px;cursor:pointer;font:inherit}
button.cat:hover,button.cat.on{border-color:#3355ff;background:#eef1ff}
.v{font-weight:700;padding:2px 8px;border-radius:6px;font-size:13px}
.PAY{background:#ffe1e1;color:#9b1c1c}.REVIEW{background:#fff3cd;color:#7a5200}.BLOCK{background:#dff5e3;color:#14632b}
table{border-collapse:collapse;width:100%}td,th{padding:6px 8px;border-bottom:1px solid #eef0f4;text-align:left;font-size:14px}
small,.muted{color:#5b6475}code{font-size:13px}
</style></head><body>
<header><h1>Fool the Shield</h1><div class="banner">Replaying recorded runs</div>
<div class="muted" style="color:#c9cfdb;margin-top:6px">Pick an attack. See what the AI checker alone would have done, and what the gate did. Every verdict below was recorded on the frozen commit __COMMIT__; nothing runs live and nothing is sent anywhere.</div></header>
<main>
<div class="card"><b>Opening case (fictional, modelled on a publicly reported Oct 2026 campaign):</b> look-alike sender, an expiring discount, and instructions for AI agents hidden in invisible characters.
Gate verdict: <span class="v __OCV__">__OCV__</span> <small>(__OCR__)</small></div>
<div class="card" id="sum"></div>
<div class="card"><b>__TITLE__</b><div class="muted" style="font-size:14px">__BLURB__</div><div class="grid" id="cats" style="margin-top:10px"></div></div>
<div class="card" id="detail"><span class="muted">No attack selected.</span></div>
<div class="card"><small>Arms: <b>AI checker alone</b> = Muse Spark Open reading the invoice, no rules. <b>Gate</b> = the deterministic Rust governor (payee always from the vendor master). <b>Gate + AI</b> = the gate with the model allowed to propose item lines (verified verbatim) and raise signals (escalate only). PAY in a gate arm always pays the master account, never the account on the document. Source: <code>eval/results/__RUN__.jsonl</code>.</small></div>
</main>
<script id="data" type="application/json">__DATA__</script>
<script>
const D=JSON.parse(document.getElementById('data').textContent);
const ARMS=[['checker_only','AI checker alone'],['rules','Gate'],['both','Gate + AI']];
function el(t,c,txt){const e=document.createElement(t);if(c)e.className=c;if(txt!=null)e.textContent=txt;return e}
const s=document.getElementById('sum');s.appendChild(el('b',null,`Across all ${D.summary.both.attacks} attacks: `));
ARMS.forEach(([a,n])=>{const x=D.summary[a];s.appendChild(el('span',null,` ${n}: ${x.attack_paid}/${x.attacks} paid ·`))});
s.appendChild(el('div','muted',`Genuine invoices sent to a human: gate ${D.summary.rules.clean_benign_false_alarm}/${D.summary.rules.clean_benign}, gate + AI ${D.summary.both.clean_benign_false_alarm}/${D.summary.both.clean_benign} `));
const atk=D.cases.filter(c=>c.kind==='attack');const cats=[...new Set(atk.map(c=>c.category))].sort();
const box=document.getElementById('cats');
cats.forEach(cat=>{const b=el('button','cat');b.appendChild(el('div',null,cat.replaceAll('_',' ')));
 const n=atk.filter(c=>c.category===cat);b.appendChild(el('small',null,`${n.length} case(s)`));
 b.onclick=()=>{document.querySelectorAll('button.cat').forEach(x=>x.classList.remove('on'));b.classList.add('on');show(cat)};box.appendChild(b)});
function show(cat){const d=document.getElementById('detail');d.textContent='';
 d.appendChild(el('b',null,cat.replaceAll('_',' ')));const t=el('table');const h=el('tr');
 ['case'].concat(ARMS.map(a=>a[1])).forEach(x=>h.appendChild(el('th',null,x)));t.appendChild(h);
 atk.filter(c=>c.category===cat).forEach(c=>{const r=el('tr');r.appendChild(el('td',null,c.id));
  ARMS.forEach(([a])=>{const td=el('td');const v=c.arms[a].verdict;td.appendChild(el('span','v '+v,v));
   if(c.arms[a].reasons.length)td.appendChild(el('div','muted',c.arms[a].reasons.join(', ')));r.appendChild(td)});t.appendChild(r)});
 d.appendChild(t)}
</script></body></html>
"""


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--set", default="v3", choices=sorted(SETS))
    ap.add_argument("--labels", default=None)
    a = ap.parse_args()
    cfg = SETS[a.set]
    d = data(pathlib.Path(a.labels or cfg["labels"]), cfg["run"], cfg["commit"])
    out = REPO / "docs" / "fool-the-shield"
    out.mkdir(parents=True, exist_ok=True)
    blob = json.dumps(d, ensure_ascii=False).replace("</", "<\\/")
    page = (PAGE.replace("__DATA__", blob).replace("__COMMIT__", d["commit"]).replace("__RUN__", d["run"]).replace("__TITLE__", html.escape(cfg["title"])).replace("__BLURB__", html.escape(cfg["blurb"]))
            .replace("__OCV__", html.escape(d["opening"]["verdict"])).replace("__OCR__", html.escape(", ".join(d["opening"]["reasons"]))))
    (out / "index.html").write_text(page)
    print("wrote", (out / "index.html").relative_to(REPO), len(d["cases"]), "cases")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
