"""Held-out evaluation harness (review items 4-5, 2026-10-31).

Sets:
  v2   /workspace/blind-cases-v2: the second held-out set, authored after run 1 by a separate AI agent
       with no repo access; attack categories were informed by run 1's failure classes. The harness
       never opens labels/ while running; `--score` reads labels.jsonl only after the run.
  v3   /workspace/blind-cases-v3: sealed held-out set (eval/sealed/v3.SEALED.md), v2 schema, same adapter as v2.
       Run once on the freeze-v3 gate code (added 2026-10-31 before the run; harness only).
  v1   /workspace/blind-cases (the first set). Its one blind run is run1 (680be4d). Any run here is a
       POST-FIX RE-RUN ON A SET WE HAD ALREADY SEEN: NO LONGER BLIND.

Arms (the gate decides in both):
  rules  deterministic gate only: item-table parser only (no model lines), checker treated as clear.
  both   the gate, with Muse Spark Open allowed to (a) propose item lines when the parser read none
         (page images + text layer; every number verified verbatim by the governor) and (b) raise
         checker signals (escalate-only).
  checker_only  the quarantined Spark checker alone, no deterministic rules (added 2026-10-31 before
         the v2 run; mapping as in eval/run_ablation_dev.py).

v2 proposal ("fooled agent"): the agent pays what the document asks: vendor = master vendor whose GSTIN
is read as issuer, else whose legal name is printed, else the owner of the printed PO, else the closest
name; PO = a master PO number printed on the document; invoice number = printed; amount = printed total.
Adapter decisions: docs/DECISIONS.md ("Held-out v2 adapter"), committed before the run.

Usage:
  python3 -m tools.heldout_eval --set v2 --prefetch            # model calls only (cached), resumable
  python3 -m tools.heldout_eval --set v2 --run --out NAME --commit SHA
  python3 -m tools.heldout_eval --set v2 --score NAME          # reads labels AFTER the run
"""
from __future__ import annotations

import argparse
import concurrent.futures as cf
import datetime as dt
import hashlib
import json
import pathlib
import re
import sys
import tempfile
import time
from collections import Counter

from assembler.evidence import Evidence as AsmEvidence
from assembler.fields import printed_invoice_no
from checker.checker import run_checker
from extractor.line_proposer import needs_model_lines, propose_lines, render_pages
from providers.llm import get_provider
from shield.client import GovernorProc

REPO = pathlib.Path(__file__).resolve().parents[1]
IST = dt.timezone(dt.timedelta(hours=5, minutes=30))
CLEAR = json.dumps({"signals": []})
VERDICT = {"allow": "PAY", "review": "REVIEW", "block": "BLOCK"}
ROOTS = {"v3": pathlib.Path("/workspace/blind-cases-v3"), "v2": pathlib.Path("/workspace/blind-cases-v2"),
         "v1": pathlib.Path("/workspace/blind-cases")}
V2_SCHEMA = ("v2", "v3")  # v3 reuses the v2 schema (blind-cases-v3/MANIFEST.md); same adapter, no new rules
V1_SHA = "14a370497e59bfd8a66e6d177bb2cbae999ba5d73a4f2b1b49f9ccb82df62bb1"
LIMIT: list[str] | None = None  # v1 smoke tests only (--only); refused on v2


def epoch(date: str, hour: int) -> int:
    d = dt.date.fromisoformat(date)
    return int(dt.datetime(d.year, d.month, d.day, hour, tzinfo=IST).timestamp())


def fold(s: str) -> str:
    return re.sub(r"\s+", " ", (s or "").lower()).strip()


def v2_integrity(root: pathlib.Path) -> dict:
    """Check every non-label file against SHA256SUMS; never reads labels/."""
    sums = (root / "SHA256SUMS").read_text()
    bad, n = [], 0
    for line in sums.splitlines():
        if not line.strip():
            continue
        h, name = line.split(None, 1)
        name = name.strip().lstrip("*")
        if name.startswith("labels/"):
            continue
        n += 1
        if hashlib.sha256((root / name).read_bytes()).hexdigest() != h:
            bad.append(name)
    return {"sha256sums_sha256": hashlib.sha256(sums.encode()).hexdigest(), "files_checked": n, "mismatched": bad}


DATE_RES = [
    (re.compile(r"\b(20\d\d)-(\d\d)-(\d\d)\b"), lambda m: (int(m[1]), int(m[2]), int(m[3]))),
    (re.compile(r"\b(\d{1,2})[/.-](\d{1,2})[/.-](20\d\d)\b"), lambda m: (int(m[3]), int(m[2]), int(m[1]))),
    (re.compile(r"\b(\d{1,2})[\s-]([A-Za-z]{3})[A-Za-z]*[\s,-]+(20\d\d)\b"), lambda m: (int(m[3]), m[2], int(m[1]))),
    (re.compile(r"\b([A-Za-z]{3})[A-Za-z]*\s+(\d{1,2}),?\s+(20\d\d)\b"), lambda m: (int(m[3]), m[1], int(m[2]))),
]
MONTHS = {m: i for i, m in enumerate(["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"], 1)}


def _first_date(seg: str) -> str | None:
    best = None
    for rx, conv in DATE_RES:
        for m in rx.finditer(seg):
            y, mo, d = conv(m)
            if isinstance(mo, str):
                mo = MONTHS.get(mo.lower()[:3])
            try:
                v = dt.date(y, mo, d)
            except (TypeError, ValueError):
                continue
            if best is None or m.start() < best[0]:
                best = (m.start(), v)
    return best[1].isoformat() if best else None


def doc_date(text: str) -> str | None:
    """Invoice date: the first date after a 'date' label, else the first date in the text."""
    for m in re.finditer(r"(?i)date", text or ""):
        got = _first_date(text[m.end():m.end() + 40])
        if got:
            return got
    return _first_date(text or "")


def v2_master(fx: dict, case: dict) -> dict:
    vendors = [{"vendor_id": v["id"], "legal_name": v["legal_name"], "gstin": v.get("gstin") or None, "pan": None,
                "beneficiary_id": f"BENE_{v['id']}", "bank_account": v["bank_account"], "ifsc": v["ifsc"],
                "bank_name": v.get("bank_name", ""), "email_domains": [], "category": ""} for v in fx["vendors"]]
    seeded: Counter = Counter()
    billed: Counter = Counter()
    for p in fx["paid_invoices"]:
        seeded[p["po_number"]] += int(p["amount_paise"])
        for ln in p.get("lines", []):
            billed[(p["po_number"], ln.get("hsn"), ln.get("unit_price_paise"))] += int(ln.get("qty", 0))
    tol = int(fx["policy"].get("rounding_tolerance_paise", 0))
    pos = []
    for po in fx["purchase_orders"]:
        lines = []
        for ln in po["lines"]:
            q = int(ln["qty"]) - billed.get((po["po_number"], ln.get("hsn"), ln["unit_price_paise"]), 0)
            lines.append({"item": ln["description"], "aliases": [ln["sku"]] if ln.get("sku") else [], "qty": max(q, 0),
                          "unit_price_minor": int(ln["unit_price_paise"]), "unit": ln.get("unit") or "nos"})
        pos.append({"po_ref": po["po_number"], "owner_vendor_id": po["vendor_id"], "category": "", "currency": "INR",
                    "open_balance_paise": int(po["balance_paise"]) + seeded.get(po["po_number"], 0) + tol, "lines": lines})
    thr = int(fx["policy"]["review_threshold_paise"])
    return {"vendors": vendors, "pos": pos, "policy": {
        "approval_threshold_paise": thr, "payout_cap_paise": thr * 10, "po_tolerance_bp": 0,
        "window_secs": int(fx["policy"].get("split_window_days", 7)) * 86400,
        "near_duplicate_window_secs": 90 * 86400, "buyer_gstin": case["buyer_entity_gstin"]}}


def v2_seeds(fx: dict) -> list:
    gst = {v["id"]: v.get("gstin") for v in fx["vendors"]}
    po_desc = {(po["po_number"], ln.get("hsn"), int(ln["unit_price_paise"])): ln["description"]
               for po in fx["purchase_orders"] for ln in po["lines"]}
    out = []
    for p in fx["paid_invoices"]:
        prop = {"vendor_id": p["vendor_id"], "invoice_no": p["invoice_number"], "amount_paise": int(p["amount_paise"]),
                "currency": "INR", "po_ref": p["po_number"]}
        ev = {"text_layer_gstin": gst.get(p["vendor_id"]), "has_text_layer": True, "text_source": "text_layer"}
        # v3 prep (2026-10-31, re-runs only): the paid record's own lines (qty, unit price) go into the
        # ledger seed, so the resubmission rule can compare itemized content with history the way it
        # would with a decision the gate made itself. Descriptions come from the matching PO line.
        lines = [{"description": po_desc.get((p["po_number"], ln.get("hsn"), int(ln.get("unit_price_paise", 0))), "paid line"),
                  "amount_paise": int(ln["qty"]) * int(ln["unit_price_paise"]), "qty": int(ln["qty"]),
                  "unit_price_minor": int(ln["unit_price_paise"]), "is_tax": False}
                 for ln in p.get("lines", []) if ln.get("qty") and ln.get("unit_price_paise")]
        if lines:
            ev["item_lines"] = lines
        out.append((epoch(p["invoice_date"], 10), prop, ev))
    return sorted(out, key=lambda x: x[0])


def v2_proposal(fx: dict, ev: dict, case_id: str) -> dict:
    text = ev.get("text_layer") or ""
    ft = fold(text)
    vend = None
    g = ev.get("text_layer_gstin")
    if g:
        vend = next((v["id"] for v in fx["vendors"] if (v.get("gstin") or "").upper() == g.upper()), None)
    if vend is None:
        named = [v for v in fx["vendors"] if fold(v["legal_name"]) in ft]
        if named:
            vend = max(named, key=lambda v: len(v["legal_name"]))["id"]
    nospace = re.sub(r"\s+", "", text)
    hits = [p for p in fx["purchase_orders"] if re.sub(r"\s+", "", p["po_number"]) in nospace]
    po = max(hits, key=lambda p: len(p["po_number"])) if hits else None
    if vend is None and po is not None:
        vend = po["vendor_id"]
    if vend is None:
        toks = set(re.findall(r"[a-z]{3,}", ft))
        vend = max(fx["vendors"], key=lambda v: len(set(re.findall(r"[a-z]{3,}", fold(v["legal_name"]))) & toks))["id"]
    amt = ev.get("printed_total_paise")
    return {"vendor_id": vend, "invoice_no": printed_invoice_no(text) or f"UNREAD-{case_id}",
            "amount_paise": int(amt) if amt else 1, "currency": "INR",
            "po_ref": po["po_number"] if po else "PO-NOT-PRINTED"}


def load_cases(which: str) -> tuple[list[dict], dict]:
    cases, meta = _load_cases(which)
    if LIMIT:
        if which in V2_SCHEMA:
            raise SystemExit(f"--only is refused on {which} (one full run only)")
        cases = [c for c in cases if c["id"] in LIMIT]
    return cases, meta


def _load_cases(which: str) -> tuple[list[dict], dict]:
    root = ROOTS[which]
    if which in V2_SCHEMA:
        meta = v2_integrity(root)
        if meta["mismatched"]:
            raise SystemExit(f"integrity failure: {meta['mismatched']}")
        fx = json.loads((root / "fixtures.json").read_text())
        cases = []
        for d in sorted((root / "cases").iterdir()):
            ctx = json.loads((d / "context.json").read_text())
            cases.append({"id": ctx["case_id"], "pdf": d / ctx["document"], "ctx": ctx, "fx": fx})
        return cases, meta
    got = hashlib.sha256((root / "cases.jsonl").read_bytes()).hexdigest()
    if got != V1_SHA:
        raise SystemExit("v1 hash mismatch")
    policy = json.loads((root / "policy.json").read_text())
    cases = [json.loads(x) for x in (root / "cases.jsonl").read_text().splitlines() if x.strip()]
    return [{"id": c["id"], "pdf": root / c["pdf"], "case": c, "policy": policy} for c in cases], {"cases_sha256": got}


class Ex:
    """The real extraction service object (sandboxed worker + Ed25519 signing), one per buyer GSTIN."""
    _by: dict = {}

    @classmethod
    def get(cls, buyer: str):
        from extractor.service import Extractor
        if buyer not in cls._by:
            cls._by[buyer] = Extractor(buyer_gstin=buyer)
        return cls._by[buyer]


def buyer_of(which, c):
    return c["ctx"]["buyer_entity_gstin"] if which in V2_SCHEMA else c["policy"]["buyer"]["gstin"]


def case_inputs(which, c) -> dict:
    """Evidence as the extraction service computes it (same code, same sandbox), used to build the
    proposal and model prompts. The governor still receives a freshly signed envelope."""
    env = Ex.get(buyer_of(which, c)).extract(c["pdf"].read_bytes(), {})
    ev = json.loads(env["payload"]).get("evidence") or {}
    if which in V2_SCHEMA:
        prop = v2_proposal(c["fx"], ev, c["id"])
    else:
        from tools.blind_eval import proposal as v1_prop
        prop = v1_prop(c["case"])
    return {"ev": ev, "prop": prop}


def model_calls(which, c, provider, inp=None) -> dict:
    """Model calls for one case (cached on disk by request hash)."""
    inp = inp or case_inputs(which, c)
    pdf = c["pdf"].read_bytes()
    out = {"lines": None, "lines_error": None}
    text = inp["ev"].get("text_layer", "") or ""
    if needs_model_lines(inp["ev"]):
        lp = propose_lines(provider, text, images=render_pages(pdf))
        out["lines"] = lp.lines if lp.ok else None
        out["lines_error"] = lp.error
    asm = AsmEvidence(pdf_sha256=hashlib.sha256(pdf).hexdigest(), pages=1, text=text)
    out["checker_raw"] = run_checker(provider, asm, sample=0, reasoning="low").raw_for_governor
    return out


def prefetch(which: str, workers: int) -> None:
    cases, _ = load_cases(which)
    prov = get_provider("spark")
    t0, done = time.time(), 0
    with cf.ThreadPoolExecutor(workers) as pool:
        futs = {pool.submit(model_calls, which, c, prov): c["id"] for c in cases}
        for f in cf.as_completed(futs):
            try:
                f.result()
                done += 1
            except Exception as e:  # noqa: BLE001
                print("prefetch error", futs[f], type(e).__name__, str(e)[:200], flush=True)
    print(f"prefetched {done}/{len(cases)} in {time.time() - t0:.0f}s", flush=True)


def gate_once(which, c, inp, arm, calls) -> dict:
    with tempfile.TemporaryDirectory() as t:
        mpath = pathlib.Path(t) / "master.json"
        if which in V2_SCHEMA:
            master, seeds = v2_master(c["fx"], c["ctx"]), v2_seeds(c["fx"])
        else:
            from tools.blind_eval import build_master
            master = build_master(c["case"], c["policy"])
            master["policy"]["buyer_gstin"] = c["policy"]["buyer"]["gstin"]
            vend = {v["vendor_id"]: v for v in c["case"]["vendor_master"]}
            seeds = [(epoch(h.get("paid_on") or h["invoice_date"], 10),
                      {"vendor_id": h["vendor_id"], "invoice_no": h["invoice_number"], "amount_paise": round(float(h["amount"]) * 100),
                       "currency": "INR", "po_ref": h["po_reference"]},
                      {"text_layer_gstin": vend.get(h["vendor_id"], {}).get("gstin") or None, "has_text_layer": True, "text_source": "text_layer"})
                     for h in c["case"].get("payment_history", [])]
        mpath.write_text(json.dumps(master))
        ex = Ex.get(buyer_of(which, c))
        seed_v = []
        with GovernorProc(mpath, dev_unsigned=True, extra_env={"GOVERNOR_EXTRACTOR_PUBKEY": ex.public_hex}) as gov:
            last = 0
            for ts, prop, ev in seeds:
                seed_v.append(VERDICT[gov.evaluate(prop, ev, CLEAR, now=ts)["verdict"]])
                last = max(last, ts)
            if which in V2_SCHEMA:
                day = doc_date(inp["ev"].get("text_layer", "")) or c["ctx"]["received_on"]
                now = epoch(day, 12)
            else:
                day = c["case"]["invoice"]["header"]["invoice_date"]
                now = max(epoch(day, 12), last + 3600)
            pdf = c["pdf"].read_bytes()
            env = ex.extract(pdf, {})
            lines = calls.get("lines") if arm == "both" else None
            chk = calls.get("checker_raw") if arm == "both" else CLEAR
            d = gov.evaluate_signed(inp["prop"], env, chk, hashlib.sha256(pdf).hexdigest(), now=now, proposed_item_lines=lines)
    return {"verdict": VERDICT[d["verdict"]], "deterministic_verdict": VERDICT[d["deterministic_verdict"]],
            "reasons": [f"{r['rule']}:{r['verdict']}" for r in d["reasons"]],
            "signals": [f"{s['kind']}:{s['severity']}" for s in d.get("signals", [])],
            "tamper_alert": d["tamper_alert"], "seed_verdicts": dict(Counter(seed_v)), "now_date": day,
            "model_lines_offered": lines is not None}


def checker_only(raw: str | None) -> dict:
    """Checker-only arm: the quarantined Spark checker alone, no deterministic rules (same mapping as
    eval/run_ablation_dev.py): no signals -> PAY, any review signal -> REVIEW, any block signal ->
    BLOCK, unparseable/missing -> REVIEW."""
    from eval.run_ablation_dev import checker_verdict
    return {"verdict": checker_verdict(raw, raw is not None), "reasons": []}


NOTES = {
    "v1": "POST-FIX RE-RUN ON A SET WE HAD ALREADY SEEN: NO LONGER BLIND",
    "v2": ("second held-out set, authored after run 1 by a separate AI agent with no repo access; attack "
           "categories were informed by run 1's failure classes. Run once."),
    "v3": ("third held-out set, sealed before any run (eval/sealed/v3.SEALED.md); authored by an AI agent given only "
           "SCHEMA.md and THREAT_MODEL.md, no repo access, no results seen. Run once on freeze-v3 gate code; BLIND."),
}


def run(which: str, out: str, commit: str, resume: bool = False, budget_s: float | None = None) -> int:
    """`resume`: keep rows already in the output file and run only the missing cases (re-runs on
    seen sets, so a run can be split across short sessions; never used for a blind run).
    `budget_s`: stop after this many seconds (exit code 3 = not finished; call again with resume)."""
    cases, meta = load_cases(which)
    prov = get_provider("spark", offline=True)
    rows = []
    path = REPO / "eval" / "results" / f"{out}.jsonl"
    if resume and path.exists():
        rows = [json.loads(x) for x in path.read_text().splitlines() if x.strip()]
    done_ids = {r["id"] for r in rows}
    t_start = time.time()
    started_ist = dt.datetime.now(IST).isoformat(timespec="seconds")
    for c in cases:
        if c["id"] in done_ids:
            continue
        if budget_s is not None and time.time() - t_start > budget_s:
            print(f"budget reached: {len(rows)}/{len(cases)} done", flush=True)
            return 3
        row = {"id": c["id"]}
        try:
            inp = case_inputs(which, c)
            calls = model_calls(which, c, prov, inp)  # offline: cache only
            ev = inp["ev"]
            row["proposal"] = {k: inp["prop"][k] for k in ("vendor_id", "invoice_no", "amount_paise", "po_ref")}
            row["evidence_summary"] = {"parser_lines": len(ev.get("item_lines") or []), "printed_total": ev.get("printed_total_paise"),
                                       "issuer_gstin_read": bool(ev.get("text_layer_gstin")), "tax_kinds": ev.get("tax_kinds"),
                                       "document_kind": ev.get("document_kind"), "text_source": ev.get("text_source"),
                                       "model_lines": None if calls.get("lines") is None else len(calls["lines"]),
                                       "model_lines_error": calls.get("lines_error")}
            row["rules"] = gate_once(which, c, inp, "rules", calls)
            row["both"] = gate_once(which, c, inp, "both", calls)
            row["checker_only"] = checker_only(calls.get("checker_raw"))
        except Exception as e:  # recorded, never retried
            row["error"] = f"{type(e).__name__}: {e}"[:300]
        rows.append(row)
        print(row["id"], {a: row.get(a, {}).get("verdict") for a in ("rules", "checker_only", "both")}, row.get("error", ""), flush=True)
        path.write_text("\n".join(json.dumps(r, ensure_ascii=False) for r in rows) + "\n")
    meta.update({"set": which, "commit": commit, "cases": len(rows), "harness_errors": sum("error" in r for r in rows),
                 "started_at_ist": started_ist, "ran_at_ist": dt.datetime.now(IST).isoformat(timespec="seconds"),
                 "model_sample_index": 0, "checker_reasoning": "low",
                 "note": NOTES[which]})
    (REPO / "eval" / "results" / f"{out}.meta.json").write_text(json.dumps(meta, indent=1) + "\n")
    return 0


def score(which: str, out: str) -> int:
    rows = [json.loads(x) for x in (REPO / "eval" / "results" / f"{out}.jsonl").read_text().splitlines() if x.strip()]
    labels = {}
    if which in V2_SCHEMA:
        for x in (ROOTS[which] / "labels" / "labels.jsonl").read_text().splitlines():
            if x.strip():
                lab = json.loads(x)
                labels[lab["case_id"]] = lab
    else:
        for x in (ROOTS["v1"] / "cases.jsonl").read_text().splitlines():
            if x.strip():
                c = json.loads(x)
                labels[c["id"]] = {"expected": c["expected_verdict"], "acceptable": c["acceptable_verdicts"],
                                   "kind": c["kind"], "category": c["category"]}
    summ = {}
    arms = [a for a in ("rules", "checker_only", "both") if any(a in r for r in rows)] or ["rules", "both"]
    for arm in arms:
        s, misses = Counter(), []
        for r in rows:
            lab = labels[r["id"]]
            v = r.get(arm, {}).get("verdict", "HARNESS_ERROR")
            att = lab["kind"] == "attack"
            clean = lab["kind"] == "benign" and lab["expected"] == "PAY"
            s["cases"] += 1
            s["attacks"] += att
            s["benign"] += not att
            s["clean_benign"] += clean
            s["strict"] += v == lab["expected"]
            s["lenient"] += v in lab["acceptable"]
            s["attack_paid"] += att and v == "PAY"
            s["benign_blocked"] += (not att) and v == "BLOCK"
            s["clean_benign_false_alarm"] += clean and v != "PAY"
            if v not in lab["acceptable"] or v != lab["expected"]:
                misses.append({"id": r["id"], "kind": lab["kind"], "category": lab.get("category"), "expected": lab["expected"],
                               "acceptable": lab["acceptable"], "got": v, "reasons": r.get(arm, {}).get("reasons")})
        summ[arm] = {**s, "misses": misses}
    (REPO / "eval" / "results" / f"{out}.score.json").write_text(json.dumps(summ, indent=1, ensure_ascii=False) + "\n")
    for arm, s in summ.items():
        print(arm, {k: v for k, v in s.items() if k != "misses"})
    return 0


def main(argv=None) -> int:
    global LIMIT
    ap = argparse.ArgumentParser()
    ap.add_argument("--set", choices=["v1", "v2", "v3"], required=True)
    ap.add_argument("--prefetch", action="store_true")
    ap.add_argument("--workers", type=int, default=8)
    ap.add_argument("--run", action="store_true")
    ap.add_argument("--out", default=None)
    ap.add_argument("--commit", default="")
    ap.add_argument("--score", default=None)
    ap.add_argument("--only", default=None, help="comma-separated v1 case ids (smoke test)")
    ap.add_argument("--resume", action="store_true", help="re-runs only: keep finished rows, run the rest")
    ap.add_argument("--budget", type=float, default=None, help="stop after N seconds (exit 3); use with --resume")
    a = ap.parse_args(argv)
    LIMIT = a.only.split(",") if a.only else None
    if a.prefetch:
        prefetch(a.set, a.workers)
        return 0
    if a.run:
        return run(a.set, a.out, a.commit, a.resume, a.budget)
    if a.score:
        return score(a.set, a.score)
    ap.print_help()
    return 2


if __name__ == "__main__":
    sys.exit(main())
