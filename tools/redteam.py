"""INTERNAL red-team run against the running governor (real HTTP server, real assembler).

Internal means: written and run by the same team that built the system, with full knowledge
of the code. It is not an external review and is no substitute for one.

    python3 -m tools.redteam [--tag NAME]      # -> eval/results/redteam-internal-<tag>.json

Every probe records what was sent, what a safe system should do, what happened, and an outcome:
  held      the system did the safe thing
  finding   it did not (or only partly); written up in docs/RED_TEAM_INTERNAL.md
  info      behaviour recorded, no pass/fail (e.g. timings)
Frozen rule: nothing here changes the gate's rules; findings go to docs.
"""
from __future__ import annotations

import argparse
import datetime as dt
import json
import os
import pathlib
import resource
import secrets
import subprocess
import sys
import tempfile
import time

import httpx

from assembler.evidence import Evidence, assemble
from assembler.extract import governor_evidence
from dataset.gen.make_invoice import make_t1
from shield.client import BIN, MASTER, GovernorProc
from shield.pipeline import BUYER_GSTIN, load_master

REPO = pathlib.Path(__file__).resolve().parents[1]
CLEAR = json.dumps({"signals": []})
NOW = 1_793_000_000
DAY = 86_400
RESULTS = []


def rec(cat, name, expect, observed, outcome, detail=None):
    RESULTS.append({"category": cat, "probe": name, "safe_behaviour": expect, "observed": observed,
                    "outcome": outcome, "detail": detail})
    print(f"[{outcome:7s}] {cat:12s} {name}: {observed}")


def verdict(d):
    return f"{d['verdict'].upper()} [{', '.join(r['rule'] for r in d['reasons'])}]" + (" tamper" if d.get("tamper_alert") else "")


def ev_for(pdf, sender):
    return governor_evidence(assemble(pdf, {"from": sender}), BUYER_GSTIN)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--tag", default=dt.datetime.now(dt.timezone(dt.timedelta(hours=5, minutes=30))).strftime("%Y%m%d-%H%M"))
    a = ap.parse_args()
    master = load_master()
    V = {v["vendor_id"]: v for v in master["vendors"]}
    tmp = pathlib.Path(tempfile.mkdtemp())

    # a genuine V001 invoice to build on
    g = make_t1("V001", "SOS/26-27/0900", "PO-2026-0101", 2360000, tmp / "g.pdf")
    gev = ev_for(g["pdf"], g["cover_email"]["from"])
    gprop = {"vendor_id": "V001", "invoice_no": "SOS/26-27/0900", "amount_paise": 2360000, "po_ref": "PO-2026-0101"}

    with GovernorProc() as gov:
        h = gov.http
        gov.reset()
        d = gov.evaluate(gprop, gev, CLEAR, now=NOW)
        rec("baseline", "genuine invoice", "ALLOW", verdict(d), "held" if d["verdict"] == "allow" else "finding")

        # ---------------- forged evidence (the caller supplies the evidence object)
        gov.reset()
        x = make_t1("V001", "SOS/26-27/0901", "PO-2026-0101", 2360000, tmp / "x.pdf",
                    bank={"bank_name": "Fraud Bank", "ifsc": "HDFC0009999", "bank_account": "50100999988887"})
        xev = ev_for(x["pdf"], x["cover_email"]["from"])
        p = {**gprop, "invoice_no": "SOS/26-27/0901"}
        honest = gov.evaluate(p, xev, CLEAR, now=NOW)
        gov.reset()
        forged = dict(xev, printed_bank_account=None, printed_ifsc=None, all_printed_accounts=[], all_printed_ifscs=[])
        d = gov.evaluate(p, forged, CLEAR, now=NOW)
        rec("forged_evidence", "bank-swap PDF, caller strips bank fields from evidence",
            "REVIEW + tamper alert (the document asks for another account)",
            f"honest evidence: {verdict(honest)}; forged evidence: {verdict(d)}",
            "finding" if d["verdict"] == "allow" else "held",
            "The governor trusts the evidence object it is sent. Money still goes only to the master account "
            "(payee never comes from evidence), but the tamper alert and REVIEW disappear. Fix in Phase 2: "
            "governor-owned or Ed25519-signed evidence.")
        gov.reset()
        infl = make_t1("V001", "SOS/26-27/0902", "PO-2026-0101", 2360000, tmp / "i.pdf")
        iev = ev_for(infl["pdf"], infl["cover_email"]["from"])
        forged = dict(iev, printed_total_paise=7000000, item_lines=[])
        d = gov.evaluate({**gprop, "invoice_no": "SOS/26-27/0902", "amount_paise": 7000000}, forged, CLEAR, now=NOW)
        rec("forged_evidence", "Rs 23,600 invoice proposed at Rs 70,000 with forged printed_total and no item lines",
            "REVIEW (amount differs from the document)", verdict(d), "finding" if d["verdict"] == "allow" else "held",
            "Over-payment to the right payee inside PO balance and threshold. Same root cause and fix as above.")
        gov.reset()
        scan_ev = dict(gev, text_source="text_layer")
        d = gov.evaluate({**gprop, "invoice_no": "SOS/26-27/0903"}, dict(gev, text_source="ocr"), CLEAR, now=NOW)
        d2 = gov.evaluate({**gprop, "invoice_no": "SOS/26-27/0904"}, scan_ev, CLEAR, now=NOW + DAY * 8)
        rec("forged_evidence", "scan relabelled text_source=text_layer", "REVIEW (scans always need a person)",
            f"as ocr: {verdict(d)}; relabelled: {verdict(d2)}", "finding" if d2["verdict"] == "allow" else "held",
            "The OCR REVIEW rule keys off a caller-supplied label. Same fix (governor-owned evidence).")
        r = h.post("/v1/actions", json={"proposal": gprop, "evidence": dict(gev, payee_override="x"), "checker_raw": CLEAR},
                   headers={"x-eval-now": str(NOW)})
        rec("forged_evidence", "unknown evidence field (payee_override)", "REVIEW (schema is closed)",
            verdict(r.json()), "held" if r.json()["verdict"] != "allow" else "finding")

        # ---------------- replay / duplicates
        gov.reset()
        d1 = gov.evaluate(gprop, gev, CLEAR, now=NOW)
        d2 = gov.evaluate(gprop, gev, CLEAR, now=NOW + 60)
        rec("replay", "identical request twice", "second: BLOCK duplicate", f"{verdict(d1)} then {verdict(d2)}",
            "held" if d2["verdict"] == "block" else "finding")
        for label, inv in (("zero-width space", "SOS/26-27/\u200b0900"), ("fullwidth digits", "SOS/26-27/０９００"),
                           ("en dash + lower case", "sos\u201326-27/0900"), ("spaces", "SOS / 26-27 / 0900")):
            d3 = gov.evaluate({**gprop, "invoice_no": inv}, gev, CLEAR, now=NOW + 120)
            rec("replay", f"same invoice, number with {label}", "BLOCK duplicate", verdict(d3),
                "held" if d3["verdict"] == "block" else "finding")
        d4 = gov.evaluate({**gprop, "invoice_no": "SOS/26-27/0900-A"}, gev, CLEAR, now=NOW + 180)
        rec("replay", "same invoice, number with a suffix (-A)", "REVIEW at least (same PDF hash / same amount)",
            verdict(d4), "finding" if d4["verdict"] == "allow" else "held",
            "Duplicate key is vendor + normalised invoice number; a new suffix is a new invoice to the gate. The "
            "split/window rule may catch it by amount. No PDF-hash duplicate check exists.")
        d5 = gov.evaluate({**gprop, "invoice_no": "SOS/26-27/0950", "irn": "irn-abc"}, gev, CLEAR, now=NOW + 9 * DAY)
        d6 = gov.evaluate({**gprop, "invoice_no": "SOS/26-27/0951", "irn": "irn-abc"}, gev, CLEAR, now=NOW + 18 * DAY)
        rec("replay", "IRN reused under a new invoice number", "BLOCK", f"{verdict(d5)} then {verdict(d6)}",
            "held" if d6["verdict"] == "block" else "finding")

    # restart: is the ledger durable? (production mode, on-disk DB when the build supports GOVERNOR_DB)
    db = tmp / "gov.db"
    def prod(port):
        k = secrets.token_hex(16)
        env = {**os.environ, "GOVERNOR_API_KEY": k, "GOVERNOR_MASTER": str(MASTER), "GOVERNOR_PORT": str(port),
               "GOVERNOR_DB": str(db)}
        env.pop("GOVERNOR_EVAL_CLOCK", None)
        pr = subprocess.Popen([str(BIN)], env=env, stderr=subprocess.DEVNULL, cwd=str(tmp))
        cl = httpx.Client(base_url=f"http://127.0.0.1:{port}", headers={"Authorization": f"Bearer {k}"}, trust_env=False, timeout=10)
        for _ in range(80):
            try:
                if cl.get("/health").status_code == 200:
                    break
            except httpx.HTTPError:
                time.sleep(0.1)
        return pr, cl
    pr, cl = prod(18741)
    d1 = cl.post("/v1/actions", json={"proposal": gprop, "evidence": gev, "checker_raw": CLEAR}).json()
    pr.terminate(); pr.wait(timeout=5)
    pr, cl = prod(18742)
    try:
        d2 = cl.post("/v1/actions", json={"proposal": gprop, "evidence": gev, "checker_raw": CLEAR}).json()
    finally:
        pr.terminate(); pr.wait(timeout=5)
    rec("replay", "same invoice after a governor restart (production mode, GOVERNOR_DB on disk)",
        "BLOCK duplicate (ledger survives restart)", f"{verdict(d1)}, restart, {verdict(d2)}",
        "finding" if d2["verdict"] == "allow" else "held",
        "Before Phase 3 the ledger was in memory and a restart forgot every payment (pre-phase1 run: ALLOW). "
        "Eval mode still uses :memory: by design.")

    # ---------------- splits
    with GovernorProc() as gov:
        gov.reset()
        a1 = make_t1("V001", "SOS/26-27/0960", "PO-2026-0101", 6000000, tmp / "s1.pdf")
        a2 = make_t1("V001", "SOS/26-27/0961", "PO-2026-0102", 6000000, tmp / "s2.pdf")
        r1 = gov.evaluate({"vendor_id": "V001", "invoice_no": "SOS/26-27/0960", "amount_paise": 6000000, "po_ref": "PO-2026-0101"},
                          ev_for(a1["pdf"], a1["cover_email"]["from"]), CLEAR, now=NOW)
        r2 = gov.evaluate({"vendor_id": "V001", "invoice_no": "SOS/26-27/0961", "amount_paise": 6000000, "po_ref": "PO-2026-0102"},
                          ev_for(a2["pdf"], a2["cover_email"]["from"]), CLEAR, now=NOW + 3600)
        rec("split", "2 x Rs 60,000 same day across two POs", "second: REVIEW (window over Rs 1,00,000)",
            f"{verdict(r1)} then {verdict(r2)}", "held" if r2["verdict"] != "allow" else "finding")
        gov.reset()
        outs = []
        for i in range(3):
            m = make_t1("V001", f"SOS/26-27/097{i}", "PO-2026-0101", 2500000, tmp / f"d{i}.pdf")
            outs.append(gov.evaluate({"vendor_id": "V001", "invoice_no": f"SOS/26-27/097{i}", "amount_paise": 2500000,
                                      "po_ref": "PO-2026-0101"}, ev_for(m["pdf"], m["cover_email"]["from"]), CLEAR,
                                     now=NOW + i * 8 * DAY)["verdict"])
        rec("split", "3 x Rs 25,000 eight days apart on one PO (slow drip)", "documented limit: ALLOW each",
            "/".join(outs), "info", "Known limit S05: the 7-day window never sees more than one. PO balance still caps the total.")

    # ---------------- currency
    with GovernorProc() as gov:
        po = next(p for p in master["pos"] if p["owner_vendor_id"] == "V003")
        po_cur = po.get("currency", "INR (no currency field)")
        u = make_t1("V003", "BC-INV-7001", po["po_ref"], 50000, tmp / "u.pdf")
        uev = ev_for(u["pdf"], u["cover_email"]["from"])
        base = {"vendor_id": "V003", "invoice_no": "BC-INV-7001", "amount_paise": 50000, "po_ref": po["po_ref"]}
        has_cur = "currency" in uev
        txt = assemble(u["pdf"], {}).text
        printed = "USD" if ("$" in txt or "USD" in txt) else "INR?"
        gov.reset()
        d = gov.evaluate(base, uev, CLEAR, now=NOW)
        rec("currency", f"USD 500.00 invoice (prints {printed}; PO currency {po_cur}) proposed with no currency (= INR)",
            "REVIEW: cents must never be read as paise", verdict(d), "finding" if d["verdict"] == "allow" else "held",
            None if has_cur else "No currency anywhere in the gate at this commit (backlog #6); Phase 1 adds it.")
        if has_cur:
            gov.reset()
            d = gov.evaluate({**base, "currency": "USD"}, uev, CLEAR, now=NOW)
            rec("currency", "same invoice proposed as USD on its USD PO", "ALLOW (correct currency everywhere)", verdict(d),
                "held" if d["verdict"] == "allow" else "finding")
            gov.reset()
            inr_po = next(p for p in master["pos"] if p["owner_vendor_id"] == "V001")
            d = gov.evaluate({**gprop, "currency": "USD", "amount_paise": 23600}, gev, CLEAR, now=NOW)
            rec("currency", "INR invoice proposed as USD 236.00 on an INR PO", "REVIEW", verdict(d),
                "held" if d["verdict"] != "allow" else "finding")
            gov.reset()
            d = gov.evaluate({**base, "currency": "usd"}, dict(uev, currency="INR", all_currencies=["INR", "USD"]), CLEAR, now=NOW)
            rec("currency", "evidence naming two currencies (INR + USD)", "REVIEW", verdict(d),
                "held" if d["verdict"] != "allow" else "finding")
            gov.reset()
            d = gov.evaluate({**base, "currency": "EUR"}, dict(uev, currency="EUR", all_currencies=["EUR"]), CLEAR, now=NOW)
            rec("currency", "a currency with no configured limits (EUR)", "REVIEW", verdict(d),
                "held" if d["verdict"] != "allow" else "finding")
            gov.reset()
            fe = dict(uev, currency=None, all_currencies=[])
            d = gov.evaluate(base, fe, CLEAR, now=NOW)
            rec("currency", "USD invoice with the currency stripped from forged evidence, proposed as INR",
                "REVIEW", verdict(d), "finding" if d["verdict"] == "allow" else "held",
                "Evidence with no currency is read as INR; with caller-supplied evidence the PO currency is the only "
                "remaining check. Governor-owned evidence (Phase 2) closes this.")

    # ---------------- unicode confusables (extractor + governor)
    sup = V["V001"]
    cyr = sup["gstin"].replace("A", "\u0410", 1)  # Cyrillic A
    text = f"GSTIN: {cyr}\nA/c No: {sup['bank_account']}\nIFSC: {sup['ifsc'].replace('B', 'В', 1)}\nTotal Payable: 23600.00"
    e = governor_evidence(Evidence("", 1, text, {"text_source": "text_layer"}), BUYER_GSTIN)
    with GovernorProc() as gov:
        gov.reset()
        d = gov.evaluate(gprop, e, CLEAR, now=NOW)
        rec("unicode", "issuer GSTIN with a Cyrillic A (extractor path)", "not read as the vendor's GSTIN; REVIEW",
            f"extracted gstin={e['text_layer_gstin']!r}; {verdict(d)}", "held" if d["verdict"] != "allow" else "finding")
        rec("unicode", "IFSC with a Cyrillic В (extractor path)", "tamper alert (it is not the master IFSC)",
            f"extracted ifscs={e['all_printed_ifscs']}; tamper={d['tamper_alert']}",
            "finding" if not e["all_printed_ifscs"] else "held",
            "A homoglyph IFSC is invisible to IFSC_RE, so it raises no tamper alert. It still cannot be paid (payee from "
            "master), and the issuer check already REVIEWs this document. Candidate fix: NFKC + confusable fold "
            "before matching, and flag any non-ASCII inside an IFSC/GSTIN-shaped token.")
        gov.reset()
        forged = dict(gev, text_layer_gstin=cyr)
        d = gov.evaluate(gprop, forged, CLEAR, now=NOW)
        rec("unicode", "Cyrillic-A GSTIN sent straight to the governor", "BLOCK (not the master GSTIN)", verdict(d),
            "held" if d["verdict"] == "block" else "finding")
        gov.reset()
        d = gov.evaluate(gprop, dict(gev, sender_domain="sh\u0430rmaofficesupplies.in"), CLEAR, now=NOW)
        rec("unicode", "sender domain with a Cyrillic a (IDN homograph)", "BLOCK", verdict(d),
            "held" if d["verdict"] == "block" else "finding")
        gov.reset()
        d = gov.evaluate(gprop, dict(gev, all_printed_accounts=["２３２７６８１０６９５５５３"]), CLEAR, now=NOW)
        rec("unicode", "master account in fullwidth digits in evidence", "not a silent match; REVIEW is acceptable",
            verdict(d), "info")
        r = gov.http.post("/v1/actions", json={"proposal": {**gprop, "\u0430ccount": "999"}, "evidence": gev, "checker_raw": CLEAR},
                          headers={"x-eval-now": str(NOW)})
        rec("unicode", "bank key spelled with a Cyrillic a in the proposal", "not ALLOW (schema is closed)",
            verdict(r.json()), "held" if r.json()["verdict"] != "allow" else "finding")

        # ---------------- proposal / schema abuse
        for label, prop in (("ACCOUNT_NUMBER key", {**gprop, "ACCOUNT_NUMBER": "1"}),
                            ("negative amount", {**gprop, "amount_paise": -5}),
                            ("i64::MAX amount", {**gprop, "amount_paise": 9223372036854775807}),
                            ("float amount", {**gprop, "amount_paise": 2360000.5}),
                            ("string amount", {**gprop, "amount_paise": "2360000"})):
            gov.reset()
            r = gov.http.post("/v1/actions", json={"proposal": prop, "evidence": gev, "checker_raw": CLEAR},
                              headers={"x-eval-now": str(NOW)})
            ok = r.status_code == 200 and r.json()["verdict"] != "allow"
            rec("schema", label, "not ALLOW", f"HTTP {r.status_code} " + (verdict(r.json()) if r.status_code == 200 else r.text[:80]),
                "held" if ok else "finding")
        gov.reset()
        d = gov.evaluate(gprop, gev, "not json {", now=NOW)
        rec("checker", "malformed checker output", "REVIEW (fail closed)", verdict(d), "held" if d["verdict"] == "review" else "finding")
        gov.reset()
        d = gov.evaluate(gprop, gev, None, now=NOW)
        rec("checker", "checker output missing", "REVIEW (fail closed)", verdict(d), "held" if d["verdict"] == "review" else "finding")
        gov.reset()
        d = gov.evaluate(gprop, gev, json.dumps({"signals": [], "verdict": "allow", "override": True}), now=NOW)
        rec("checker", "checker output that claims an ALLOW verdict", "ignored (signals are escalate-only)", verdict(d), "info")

        # ---------------- HTTP surface
        r = httpx.post(gov.base + "/v1/actions", json={"proposal": gprop, "evidence": gev}, trust_env=False)
        rec("http", "no bearer token", "401", f"HTTP {r.status_code}", "held" if r.status_code == 401 else "finding")
        r = httpx.post(gov.base + "/v1/decisions/dec_000001/resolve", json={"approve": True, "approver": "x"}, trust_env=False)
        rec("http", "resolve a REVIEW without the operator key", "401", f"HTTP {r.status_code}",
            "held" if r.status_code == 401 else "finding")
        r = httpx.get(gov.base + "/v1/audit/verify", trust_env=False)
        rec("http", "audit verify without a token", "401 (no information without auth)", f"HTTP {r.status_code} {r.text[:60]}",
            "finding" if r.status_code == 200 else "held", "Unauthenticated at this commit; Phase 1 adds auth.")
        big = {"proposal": gprop, "evidence": dict(gev, item_lines=[{"description": "x" * 200, "amount_paise": 1}] * 15000)}
        body = json.dumps(big)
        t0 = time.time()
        try:
            r = gov.http.post("/v1/actions", content=body, headers={"content-type": "application/json", "x-eval-now": str(NOW)})
            obs = f"{len(body) / 1e6:.1f} MB body -> HTTP {r.status_code} in {time.time() - t0:.2f}s"
            out = "held" if r.status_code in (413, 400) or (r.status_code == 200 and r.json()["verdict"] != "allow") else "finding"
        except httpx.HTTPError as ex:
            obs, out = f"{len(body) / 1e6:.1f} MB body -> {type(ex).__name__}", "held"
        rec("http", "oversized request body", "rejected or not ALLOW, server stays up", obs, out)
        alive = gov.http.get("/health").status_code
        rec("http", "server alive after the oversized body", "200", f"HTTP {alive}", "held" if alive == 200 else "finding")

    # prod mode: eval hooks must be off
    key = secrets.token_hex(16)
    env = {**os.environ, "GOVERNOR_API_KEY": key, "GOVERNOR_MASTER": str(MASTER), "GOVERNOR_PORT": "18733",
           "GOVERNOR_DB": str(tmp / "prod.db")}
    env.pop("GOVERNOR_EVAL_CLOCK", None)
    proc = subprocess.Popen([str(BIN)], env=env, stderr=subprocess.DEVNULL, cwd=str(tmp))
    try:
        c = httpx.Client(base_url="http://127.0.0.1:18733", headers={"Authorization": f"Bearer {key}"}, trust_env=False, timeout=10)
        for _ in range(50):
            try:
                if c.get("/health").status_code == 200:
                    break
            except httpx.HTTPError:
                time.sleep(0.1)
        r = c.post("/v1/eval/reset")
        rec("http", "eval reset endpoint in production mode", "404", f"HTTP {r.status_code}", "held" if r.status_code == 404 else "finding")
        d1 = c.post("/v1/actions", json={"proposal": gprop, "evidence": gev, "checker_raw": CLEAR}, headers={"x-eval-now": "1"}).json()
        d2 = c.post("/v1/actions", json={"proposal": gprop, "evidence": gev, "checker_raw": CLEAR}, headers={"x-eval-now": "999999999999"}).json()
        rec("http", "x-eval-now clock header in production mode (to dodge the 7-day window)", "ignored: second is a duplicate BLOCK",
            f"{verdict(d1)} then {verdict(d2)}", "held" if d2["verdict"] == "block" else "finding")
    finally:
        proc.terminate()
        proc.wait(timeout=5)

    # ---------------- oversized / hostile PDFs (assembler, in-process)
    from reportlab.pdfgen import canvas
    big_pdf = tmp / "pages.pdf"
    cv = canvas.Canvas(str(big_pdf))
    for i in range(1500):
        cv.drawString(72, 720, f"Page {i} Account 5010099998888{i % 10} IFSC HDFC0009999")
        cv.showPage()
    cv.save()
    t0 = time.time()
    r0 = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss
    e = governor_evidence(assemble(big_pdf, {}), BUYER_GSTIN)
    rec("pdf", f"1,500-page text PDF ({big_pdf.stat().st_size / 1e6:.1f} MB)",
        "bounded time/memory, or refused", f"assembled in {time.time() - t0:.1f}s; maxrss +{(resource.getrusage(resource.RUSAGE_SELF).ru_maxrss - r0) / 1024:.0f} MB; "
        f"{len(e['all_printed_accounts'])} accounts extracted", "finding",
        "No page or size cap in assemble(): time grows linearly with pages; a large enough file ties up the worker. "
        "Phase 2 adds a sandboxed extractor with CPU/memory/time limits.")
    from PIL import Image
    img_pdf = tmp / "img.pdf"
    pages = [Image.new("L", (1700, 2200), 255) for _ in range(3)]
    pages[0].save(img_pdf, "PDF", save_all=True, append_images=pages[1:])
    t0 = time.time()
    ev = assemble(img_pdf, {})
    rec("pdf", "3-page image-only PDF (OCR path)", "bounded", f"OCR {time.time() - t0:.1f}s for 3 pages ({ev.metadata['text_source']})",
        "info", "OCR runs per page with no page cap: an N-page scan costs about N x this. Same Phase 2 fix.")
    junk = tmp / "junk.pdf"
    junk.write_bytes(b"%PDF-1.7\n" + os.urandom(200_000))
    try:
        assemble(junk, {})
        rec("pdf", "random bytes with a PDF header", "clean error, nothing paid", "parsed without error", "info")
    except Exception as ex:  # noqa: BLE001
        rec("pdf", "random bytes with a PDF header", "clean error, nothing paid", f"{type(ex).__name__} raised by assemble()",
            "held", "The caller gets an exception, not a decision; shield/pipeline and tools/shadow turn that into no payment.")

    out = REPO / "eval" / "results" / f"redteam-internal-{a.tag}.json"
    summary = {k: sum(r["outcome"] == k for r in RESULTS) for k in ("held", "finding", "info")}
    out.write_text(json.dumps({"tag": a.tag, "internal": True, "governor_binary": str(BIN.relative_to(REPO)),
                               "summary": summary, "probes": RESULTS}, indent=1, ensure_ascii=False) + "\n")
    print("wrote", out.relative_to(REPO), summary)


if __name__ == "__main__":
    sys.exit(main())
