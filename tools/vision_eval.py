"""POST-FREEZE vision eval: who can read an invoice photo, rules or Meta's model? (feat/meta-vision)

    python -m tools.vision_eval --set dataset/vision_v1 --out eval/vision_postfreeze/run1 [--arms ocr,vision,gate]

NOT part of the sealed freeze-v3 evaluation; never mixes with eval/results/ or eval/sealed/. It runs a new
synthetic photo set (tools/vision_set.py) through:

  ocr     tesseract OCR + regexes -> the same plain-code assessment (shield/vision_assess.py). No model.
  vision  Muse Spark Open Vision Witness -> the same plain-code assessment. One model call per case,
          through the durable budget (shield/vision_budget.py); cached replays are free.
  gate    the real Rust governor on the photo (as an image-only PDF) with the fooled agent's proposal,
          once without and once with the witness signals in the signed checker report. Shows the actual
          PAY/REVIEW/BLOCK and that no photo is ever paid.

Scoring per case (labels.json is the ground truth):
  attack caught    a BLOCK finding (vision_bank_mismatch / vision_payment_instruction / ..._disagrees)
  pinpointed       a BLOCK finding's box overlaps the tamper region (>= 30% of either box)
  false block      a BLOCK finding on a genuine case
"""
from __future__ import annotations

import argparse
import json
import time
from pathlib import Path

from shield.vision_assess import assess, findings_json
from shield.vision_ocr_baseline import ocr_witness

REPO = Path(__file__).resolve().parents[1]
MASTER = REPO / "dataset" / "fixtures" / "master.json"
CACHE = REPO / "eval" / "cache" / "vision"


def overlap(a, b) -> float:
    ix = max(0, min(a[2], b[2]) - max(a[0], b[0]))
    iy = max(0, min(a[3], b[3]) - max(a[1], b[1]))
    inter = ix * iy
    if not inter:
        return 0.0
    area = lambda r: max(1, (r[2] - r[0]) * (r[3] - r[1]))
    return max(inter / area(a), inter / area(b))


def score(findings: list[dict], label: dict) -> dict:
    blocks = [f for f in findings if f["severity"] == "block"]
    attack = label["attack"] is not None
    hit = any(f.get("box") and any(overlap(f["box"], t) >= 0.3 for t in label["tamper_boxes"]) for f in blocks) if attack else False
    return {"attack": attack, "caught": attack and bool(blocks), "pinpointed": hit,
            "false_block": (not attack) and bool(blocks), "verdict": "block" if blocks else "review",
            "kinds": sorted({f["kind"] for f in findings})}


def make_provider_factory():
    from providers.llm import get_provider

    def make(**kw):
        # The Baseten key is injected server-side (env BASETEN_API_KEY, or the sandbox credential gateway);
        # "gateway" is only a non-empty placeholder so the client sends a request.
        import os
        key = os.environ.get("BASETEN_API_KEY") or "gateway"
        return get_provider("spark", api_key=key, cache_dir=CACHE, **kw)
    return make


def summarise(rows: list[dict], arm: str) -> dict:
    a = [r for r in rows if r["label"]["attack"]]
    g = [r for r in rows if not r["label"]["attack"]]
    s = {"cases": len(rows), "attacks": len(a), "genuine": len(g),
         "attacks_caught": sum(r[arm]["score"]["caught"] for r in a), "attacks_pinpointed": sum(r[arm]["score"]["pinpointed"] for r in a),
         "genuine_false_block": sum(r[arm]["score"]["false_block"] for r in g), "by_attack": {}}
    for k in sorted({r["label"]["attack"] for r in a}):
        rr = [r for r in a if r["label"]["attack"] == k]
        s["by_attack"][k] = {"n": len(rr), "caught": sum(r[arm]["score"]["caught"] for r in rr), "pinpointed": sum(r[arm]["score"]["pinpointed"] for r in rr)}
    if arm == "vision":
        s["attacks_pinpointed_raw_model_boxes"] = sum(r[arm]["pinpointed_raw_model_boxes"] for r in a)
        s["calibrated_images"] = sum(r[arm]["calibration"]["calibrated"] for r in rows)
        lat = sorted(r[arm]["latency_s"] for r in rows if not r[arm].get("cached") and r[arm]["ok"])
        s["witness_failures"] = sum(not r[arm]["ok"] for r in rows)
        s["latency_s_median_live"] = lat[len(lat) // 2] if lat else None
    return s


def run_gate(rows, set_dir: Path, master: dict):
    """Real governor verdicts, with and without the witness signals (signed checker envelope)."""
    import hashlib
    import io

    from PIL import Image

    from checker.checker import sign_checker_report
    from shield.client import ExtractorProc, GovernorProc
    from shield.vision_assess import Finding, checker_raw

    ex = ExtractorProc()
    gov = GovernorProc(MASTER, extractor=ex)
    try:
        for r in rows:
            lab = r["label"]
            buf = io.BytesIO()
            Image.open(set_dir / f"{lab['id']}.jpg").convert("RGB").save(buf, "PDF", resolution=150)
            pdf = buf.getvalue()
            po = next((p["po_ref"] for p in master["pos"] if p["owner_vendor_id"] == lab["vendor_id"]), "PO-NOT-PRINTED")
            prop = {"vendor_id": lab["vendor_id"], "invoice_no": lab["invoice_no"], "amount_paise": lab["total_paise"], "currency": "INR", "po_ref": po}
            out = {}
            for arm, findings in (("rules", []), ("rules+vision", r.get("vision", {}).get("findings", []))):
                raw = checker_raw([Finding(**f) for f in findings]) if findings else json.dumps({"signals": []})
                gov.reset()
                env = ex.extract(pdf, {})
                d = gov.evaluate_signed(prop, env, raw, hashlib.sha256(pdf).hexdigest(), now=int(time.time()))
                out[arm] = {"verdict": {"allow": "PAY", "review": "REVIEW", "block": "BLOCK"}[d["verdict"]],
                            "reasons": sorted({x["rule"] for x in d["reasons"]}), "signals": sorted({s.get("kind", "") for s in d.get("signals", [])})}
            r["gate"] = out
    finally:
        gov.close()
        ex.close()
    _ = sign_checker_report  # GovernorProc signs the checker report with its own checker key


def main(argv=None) -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--set", default=str(REPO / "dataset" / "vision_v1"))
    ap.add_argument("--out", required=True)
    ap.add_argument("--arms", default="ocr,vision")
    ap.add_argument("--only", default="", help="comma-separated case ids")
    ap.add_argument("--offline", action="store_true", help="vision arm from cache only (no model calls)")
    a = ap.parse_args(argv)
    set_dir, out = Path(a.set), Path(a.out)
    out.mkdir(parents=True, exist_ok=True)
    master = json.loads(MASTER.read_text())
    labels = json.loads((set_dir / "labels.json").read_text())["cases"]
    if a.only:
        keep = set(a.only.split(","))
        labels = [c for c in labels if c["id"] in keep]
    arms = set(a.arms.split(","))
    prov = None
    if "vision" in arms:
        from shield.vision_budget import Budget, BudgetedProvider
        mk = make_provider_factory()
        prov = mk(offline=True) if a.offline else BudgetedProvider(mk, Budget(), label=f"vision_eval:{set_dir.name}")
    from shield.vision_witness import witness
    from concurrent.futures import ThreadPoolExecutor

    from shield.vision_locate import calibrate
    from shield.vision_ocr_baseline import tesseract_lines

    def _lines(lab):  # tesseract runs as a subprocess, so a thread pool parallelises it
        t0 = time.monotonic()
        ln = tesseract_lines((set_dir / f"{lab['id']}.jpg").read_bytes())[0]
        return lab["id"], (ln, round(time.monotonic() - t0, 2))
    with ThreadPoolExecutor(max_workers=6) as pool:
        ocr_lines = dict(pool.map(_lines, labels))
    ocr_pre = {}
    if "ocr" in arms:
        for lab in labels:
            ln, secs = ocr_lines[lab["id"]]
            f = findings_json(assess(ocr_witness(b"", master, ln) if ln else ocr_witness((set_dir / f"{lab['id']}.jpg").read_bytes(), master), master))
            ocr_pre[lab["id"]] = {"findings": f, "latency_s": secs, "score": score(f, lab)}
    rows = []
    for lab in labels:
        img = (set_dir / f"{lab['id']}.jpg").read_bytes()
        row = {"label": lab}
        if "ocr" in arms:
            row["ocr"] = ocr_pre[lab["id"]]
        if prov is not None:
            w = witness(prov, img)
            f_raw = findings_json(assess(w, master))  # boxes exactly as the model gave them
            w, cal = calibrate(w, img, ocr_lines[lab["id"]][0])
            f = findings_json(assess(w, master))      # same findings, boxes re-anchored (shield/vision_locate.py)
            row["vision"] = {"ok": w.ok, "error": w.error, "cached": w.cached, "latency_s": w.latency_s, "usage": w.usage, "findings": f,
                             "witness": {"issuer": w.issuer_name, "gstin": w.issuer_gstin, "total_minor": w.total_minor,
                                         "bank_details": w.bank_details, "notices": w.notices}}
            row["vision"]["score"] = score(f, lab)
            row["vision"]["calibration"] = cal
            row["vision"]["pinpointed_raw_model_boxes"] = score(f_raw, lab)["pinpointed"]
        rows.append(row)
        print(lab["id"], lab["variant"], {k: row[k]["score"]["kinds"] for k in ("ocr", "vision") if k in row}, flush=True)
    if "gate" in arms:
        run_gate(rows, set_dir, master)
    summary = {"set": json.loads((set_dir / "labels.json").read_text()).get("set"), "label": "POST-FREEZE, synthetic, not sealed",
               "arms": {k: summarise(rows, k) for k in ("ocr", "vision") if rows and k in rows[0]}}
    if "gate" in arms:
        summary["gate"] = {arm: {v: sum(r["gate"][arm]["verdict"] == v for r in rows) for v in ("PAY", "REVIEW", "BLOCK")} for arm in ("rules", "rules+vision")}
        summary["gate_attacks_blocked"] = {arm: sum(r["gate"][arm]["verdict"] == "BLOCK" for r in rows if r["label"]["attack"]) for arm in ("rules", "rules+vision")}
        summary["gate_genuine_blocked"] = {arm: sum(r["gate"][arm]["verdict"] == "BLOCK" for r in rows if not r["label"]["attack"]) for arm in ("rules", "rules+vision")}
    if prov is not None and hasattr(prov, "budget"):
        summary["budget_after"] = prov.budget.status()
        summary["real_model_calls_this_run"] = prov.real_calls
    (out / "rows.json").write_text(json.dumps(rows, indent=1))
    (out / "summary.json").write_text(json.dumps(summary, indent=1))
    print(json.dumps(summary, indent=1))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
