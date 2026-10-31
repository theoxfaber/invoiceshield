"""Real public invoice layouts through the rules+model arm (freeze-v3 gate code; no tuning).

    python3 -m tools.real_public_eval --prefetch     # model reads (cached), resumable
    python3 -m tools.real_public_eval --run          # writes eval/results/real_public_invoices.json

LABEL: real public layouts, assumed genuine, not customer shadow mode.

Sources (raw files are NOT committed; they are downloaded to RAW below, licence per file in the result):
  * ashg59/invoices-images (Hugging Face, Apache-2.0): scanned Indian B2B GST tax invoices.
  * surajitsadhukhan/indian-grocery-tax-invoice-image-dataset (Kaggle, CC BY 4.0): phone photos of
    Indian wholesale grocery GST invoices; a fixed deterministic sample (every 7th file by name).
Each JPG is wrapped into a one-page PDF (no text layer), so the extractor takes its OCR path.

Assumption (documented, not hidden): every invoice is treated as GENUINE, and the trusted records
are built from the invoice's OWN printed details, read independently of the gate's deterministic
extractor by one Muse Spark Open vision call per invoice (vendor GSTIN, bank account, IFSC, buyer
GSTIN, invoice no, grand total, item lines). Vendor master = those printed values (placeholders that
no text can match when a value is not printed); one PO owned by that vendor with open balance =
printed total + Rs 1 and rate lines = the printed item lines; approval threshold / payout cap raised
out of the way (amount policy is not a layout question; invoices above the default Rs 1,00,000
threshold are counted separately). The agent proposes exactly the printed payment. Anything but PAY
is a failure on a genuine invoice; reasons are reported. Because the master comes from a model read
of the same page, a model misread shows up as a mismatch the gate stops (REVIEW/BLOCK), never as
money moved; this measures how often real layouts get through, not fraud catch.
"""
from __future__ import annotations

import argparse
import base64
import concurrent.futures as cf
import datetime as dt
import hashlib
import json
import pathlib
import re
import tempfile
from collections import Counter

REPO = pathlib.Path(__file__).resolve().parents[1]
RAW = pathlib.Path("/workspace/work/realinv")
PDF_DIR = RAW / "pdf"
IST = dt.timezone(dt.timedelta(hours=5, minutes=30))
OUT = REPO / "eval" / "results" / "real_public_invoices.json"
VERDICT = {"allow": "PAY", "review": "REVIEW", "block": "BLOCK"}
DEFAULT_THRESHOLD_PAISE = 1_00_000_00

SOURCES = {
    "ashg59": {"dir": RAW / "ashg59", "dataset": "https://huggingface.co/datasets/ashg59/invoices-images",
               "file_url": "https://huggingface.co/datasets/ashg59/invoices-images/resolve/main/{name}",
               "license": "Apache-2.0", "license_url": "https://huggingface.co/datasets/ashg59/invoices-images (card: license: apache-2.0)",
               "kind": "scanned B2B GST tax invoice"},
    "grocery": {"dir": RAW / "grocery" / "Photos" / "Photos",
                "dataset": "https://www.kaggle.com/datasets/surajitsadhukhan/indian-grocery-tax-invoice-image-dataset",
                "file_url": "kaggle dataset file Photos/{name}", "license": "CC BY 4.0",
                "license_url": "https://www.kaggle.com/datasets/surajitsadhukhan/indian-grocery-tax-invoice-image-dataset (License: CC BY 4.0)",
                "attribution": "Indian Grocery Tax Invoice Image Dataset by Surajit Sadhukhan, CC BY 4.0",
                "kind": "phone photo of wholesale grocery GST invoice"},
}

READ_SYSTEM = """You read an Indian GST invoice image and copy what is PRINTED on it. Output ONLY JSON:
{"is_gst_invoice": bool, "supplier_name": str|null, "supplier_gstin": str|null, "buyer_gstin": str|null,
 "bank_account": str|null, "ifsc": str|null, "invoice_no": str|null, "invoice_date": "YYYY-MM-DD"|null,
 "grand_total": number|null,
 "lines": [{"description": str, "qty": number, "unit_price": number, "amount": number}]}
Rules: copy identifiers exactly as printed (no spaces); grand_total is the final payable amount in rupees;
unit_price is the pre-tax rate per unit as printed; null when not printed or unreadable. Never guess."""


def files() -> list[tuple[str, pathlib.Path]]:
    out = [("ashg59", p) for p in sorted(SOURCES["ashg59"]["dir"].glob("*.jp*g"))]
    gro = sorted(SOURCES["grocery"]["dir"].glob("*.jpg"))
    out += [("grocery", p) for p in gro[::7]]
    return out


def to_pdf(src: str, img: pathlib.Path) -> pathlib.Path:
    from PIL import Image, ImageOps
    PDF_DIR.mkdir(parents=True, exist_ok=True)
    out = PDF_DIR / f"{src}__{re.sub(r'[^A-Za-z0-9]+', '_', img.stem)}.pdf"
    if not out.exists():
        im = ImageOps.exif_transpose(Image.open(img)).convert("RGB")
        im.thumbnail((2480, 2480))
        im.save(out, resolution=250)
    return out


def png_of(img: pathlib.Path) -> bytes:
    import io
    from PIL import Image, ImageOps
    im = ImageOps.exif_transpose(Image.open(img)).convert("RGB")
    im.thumbnail((1600, 1600))
    b = io.BytesIO()
    im.save(b, "PNG")
    return b.getvalue()


def model_read(provider, img: pathlib.Path) -> dict:
    from providers.strict import parse_model  # noqa: F401  (strict JSON helper used elsewhere)
    msgs = [{"role": "system", "content": READ_SYSTEM},
            {"role": "user", "content": [{"type": "text", "text": "Read this invoice."},
                                         {"type": "image_url", "image_url": {"url": "data:image/png;base64," + base64.b64encode(png_of(img)).decode()}}]}]
    r = provider.chat(msgs, reasoning="low", max_tokens=6000, sample=0)
    if not r.ok:
        return {"error": f"provider: {r.error}"}
    txt = re.sub(r"(?s)<think>.*?</think>", "", r.content or "").strip()
    m = re.search(r"(?s)\{.*\}", txt)
    if not m:
        return {"error": "no json"}
    try:
        return json.loads(m.group(0))
    except json.JSONDecodeError as e:
        return {"error": f"bad json: {e}"[:120]}


def paise(x) -> int | None:
    try:
        return round(float(str(x).replace(",", "")) * 100)
    except (TypeError, ValueError):
        return None


def clean_id(s) -> str | None:
    s = re.sub(r"[^A-Za-z0-9]", "", str(s or "")).upper()
    return s or None


def mask(v) -> str | None:
    if not v:
        return None
    v = str(v)
    return v[:2] + "*" * max(len(v) - 6, 0) + v[-4:] if len(v) > 6 else "***"


def gate(fid: str, pdf: pathlib.Path, rd: dict, provider) -> dict:
    from extractor.line_proposer import needs_model_lines, propose_lines, render_pages
    from checker.checker import run_checker
    from assembler.evidence import Evidence as AsmEvidence
    from assembler.fields import printed_invoice_no
    from tools.heldout_eval import Ex
    from shield.client import GovernorProc

    buyer = clean_id(rd.get("buyer_gstin")) or "24AAECN5172R1ZX"
    if len(buyer) != 15:
        buyer = "24AAECN5172R1ZX"
    total = paise(rd.get("grand_total"))
    ex = Ex.get(buyer)
    doc = pdf.read_bytes()
    env = ex.extract(doc, {})
    ev = json.loads(env["payload"]).get("evidence") or {}
    text = ev.get("text_layer") or ""
    lines = []
    for ln in rd.get("lines") or []:
        q, up = ln.get("qty"), paise(ln.get("unit_price"))
        try:
            q = int(round(float(q)))
        except (TypeError, ValueError):
            continue
        if q > 0 and up:
            lines.append({"item": str(ln.get("description") or "item")[:200], "aliases": [], "qty": q, "unit_price_minor": up, "unit": "nos"})
    vid = "R_" + hashlib.sha256(fid.encode()).hexdigest()[:8]
    master = {"vendors": [{"vendor_id": vid, "legal_name": rd.get("supplier_name") or "printed supplier",
                           "gstin": clean_id(rd.get("supplier_gstin")), "pan": None, "beneficiary_id": f"BENE_{vid}",
                           "bank_account": clean_id(rd.get("bank_account")) or "000000000000",
                           "ifsc": clean_id(rd.get("ifsc")) or "NONE0000000", "bank_name": "printed", "email_domains": [], "category": "real public"}],
              "pos": [{"po_ref": f"PO-{vid}", "owner_vendor_id": vid, "category": "real public", "currency": "INR",
                       "open_balance_paise": (total or 0) + 100, "lines": lines}],
              "policy": {"approval_threshold_paise": 10**12, "payout_cap_paise": 10**13, "po_tolerance_bp": 0,
                         "window_secs": 7 * 86400, "near_duplicate_window_secs": 90 * 86400, "buyer_gstin": buyer}}
    prop = {"vendor_id": vid, "invoice_no": rd.get("invoice_no") or printed_invoice_no(text) or f"UNREAD-{vid}",
            "amount_paise": total or 1, "currency": "INR", "po_ref": f"PO-{vid}"}
    mlines, merr = None, None
    if needs_model_lines(ev):
        lp = propose_lines(provider, text, images=render_pages(doc))
        mlines, merr = (lp.lines if lp.ok else None), lp.error
    asm = AsmEvidence(pdf_sha256=hashlib.sha256(doc).hexdigest(), pages=1, text=text)
    chk = run_checker(provider, asm, sample=0, reasoning="low").raw_for_governor
    with tempfile.TemporaryDirectory() as t:
        mp = pathlib.Path(t) / "master.json"
        mp.write_text(json.dumps(master))
        with GovernorProc(mp, dev_unsigned=True, extra_env={"GOVERNOR_EXTRACTOR_PUBKEY": ex.public_hex}) as gov:
            now = int(dt.datetime(2026, 10, 31, 12, tzinfo=IST).timestamp())
            d = gov.evaluate_signed(prop, env, chk, hashlib.sha256(doc).hexdigest(), now=now, proposed_item_lines=mlines)
    return {"verdict": VERDICT[d["verdict"]], "deterministic_verdict": VERDICT[d["deterministic_verdict"]],
            "reasons": [f"{r['rule']}:{r['verdict']}" for r in d["reasons"]],
            "signals": [f"{s['kind']}:{s['severity']}" for s in d.get("signals", [])],
            "evidence": {"text_source": ev.get("text_source"), "parser_lines": len(ev.get("item_lines") or []),
                         "printed_total_read": ev.get("printed_total_paise") is not None,
                         "issuer_gstin_read": bool(ev.get("text_layer_gstin")),
                         "issuer_gstin_matches_master": bool(ev.get("text_layer_gstin")) and clean_id(ev.get("text_layer_gstin")) == master["vendors"][0]["gstin"],
                         "model_lines": None if mlines is None else len(mlines), "model_lines_error": merr},
            "master_from_print": {"supplier_gstin": mask(master["vendors"][0]["gstin"]), "bank_account": mask(clean_id(rd.get("bank_account"))),
                                  "ifsc_printed": bool(clean_id(rd.get("ifsc"))), "buyer_gstin_printed": bool(clean_id(rd.get("buyer_gstin"))),
                                  "total_printed": total is not None, "po_lines": len(lines)},
            "over_default_threshold": bool(total and total > DEFAULT_THRESHOLD_PAISE)}


def main(argv=None) -> int:
    from providers.llm import get_provider
    ap = argparse.ArgumentParser()
    ap.add_argument("--prefetch", action="store_true")
    ap.add_argument("--run", action="store_true")
    a = ap.parse_args(argv)
    fl = files()
    if a.prefetch:
        prov = get_provider("spark")

        def pre(item):
            src, img = item
            rd = model_read(prov, img)
            pdf = to_pdf(src, img)
            if "error" not in rd:
                try:
                    gate(f"{src}/{img.name}", pdf, rd, prov)  # warms line/checker cache
                except Exception as e:  # noqa: BLE001
                    return f"{img.name}: {type(e).__name__} {e}"[:200]
            return None
        with cf.ThreadPoolExecutor(6) as p:
            for msg in p.map(pre, fl):
                if msg:
                    print("prefetch note", msg, flush=True)
        print("prefetched", len(fl), flush=True)
        return 0
    if not a.run:
        ap.print_help()
        return 2
    prov = get_provider("spark", offline=True)
    started = dt.datetime.now(IST).isoformat(timespec="seconds")
    rows = []
    for src, img in fl:
        fid = f"{src}/{img.name}"
        S = SOURCES[src]
        row = {"id": fid, "source": src, "dataset": S["dataset"], "file_url": S["file_url"].format(name=img.name),
               "license": S["license"], "kind": S["kind"], "file_sha256": hashlib.sha256(img.read_bytes()).hexdigest()}
        rd = model_read(prov, img)
        if "error" in rd:
            row.update(verdict="REVIEW_HARNESS", excluded=True, exclusion=f"master read failed: {rd['error']}")
        elif rd.get("is_gst_invoice") is False:
            row.update(excluded=True, exclusion="model read says not a GST invoice")
        else:
            try:
                row.update(gate(fid, to_pdf(src, img), rd, prov))
            except Exception as e:  # recorded, never retried
                row.update(excluded=True, exclusion=f"harness error: {type(e).__name__}: {e}"[:200])
        rows.append(row)
        print(fid, row.get("verdict"), row.get("reasons", row.get("exclusion")), flush=True)
    scored = [r for r in rows if not r.get("excluded")]
    n = len(scored)
    vc = Counter(r["verdict"] for r in scored)
    reasons = Counter(x.split(":")[0] for r in scored if r["verdict"] != "PAY" for x in r["reasons"])
    by_src = {}
    for s in SOURCES:
        rs = [r for r in scored if r["source"] == s]
        by_src[s] = {"n": len(rs), **{k: sum(r["verdict"] == k for r in rs) for k in ("PAY", "REVIEW", "BLOCK")}}
    res = {"label": "real public layouts, assumed genuine, not customer shadow mode",
           "gate_code": "freeze-v3 (5c698b0); harness tools/real_public_eval.py", "arm": "rules+model (Muse Spark Open lines + checker, escalate-only)",
           "started_at_ist": started, "finished_at_ist": dt.datetime.now(IST).isoformat(timespec="seconds"),
           "assumption": ("each invoice is assumed genuine; vendor master and PO are built from the invoice's own printed details "
                          "(read by one Muse Spark Open vision call, independent of the deterministic extractor); threshold raised "
                          "out of the way; the agent proposes exactly the printed payment; anything but PAY is a failure"),
           "sources": {k: {kk: vv for kk, vv in v.items() if kk != "dir"} for k, v in SOURCES.items()},
           "files": len(rows), "scored": n, "excluded": len(rows) - n,
           "rates": {k: {"k": vc.get(k, 0), "n": n, "rate": round(vc.get(k, 0) / n, 4) if n else None} for k in ("PAY", "REVIEW", "BLOCK")},
           "by_source": by_src, "failure_reasons": dict(reasons.most_common()),
           "over_default_threshold": sum(r.get("over_default_threshold", False) for r in scored),
           "privacy": "raw images not committed; identifiers masked; only business invoices; file names are as published",
           "rows": rows}
    OUT.write_text(json.dumps(res, indent=1, ensure_ascii=False) + "\n")
    print(json.dumps({k: res[k] for k in ("files", "scored", "rates", "by_source", "failure_reasons")}, indent=1))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
