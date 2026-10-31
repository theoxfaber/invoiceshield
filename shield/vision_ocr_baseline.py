"""The no-model baseline for invoice images: local tesseract OCR + regexes (post-freeze, feat/meta-vision).

This is the strongest rules-only reading of a photo we could build in the same evening: it OCRs the image,
pulls every account number / IFSC / GSTIN with regexes, flags payment-change wording with keyword rules,
and gets line boxes from tesseract's TSV output. Its result is packed into the same Witness shape so the
SAME plain-code assessment (shield/vision_assess.py) runs on both arms: the only difference between the
arms is who reads the image (tesseract + regex vs Muse Spark Open).
"""
from __future__ import annotations

import csv
import io
import re
import subprocess
import tempfile
from pathlib import Path

from PIL import Image

from shield.vision_witness import Witness

ACCT_RE = re.compile(r"(?<![\d-])(\d[\d ]{7,20}\d)(?![\d-])")
IFSC_RE = re.compile(r"\b([A-Z]{4}[0O][A-Z0-9]{6})\b")
GSTIN_RE = re.compile(r"\b(\d{2}[A-Z0-9]{10}[0-9A-Z]Z[0-9A-Z])\b")
NOTICE_RE = re.compile(
    r"(bank (details|account|a/?c)\s*(updated|changed|change)|new\s+a/?c|bank changed|remit (this|to)|pay (to|here)|"
    r"(ai|automated|ap)\s+assistant|already approved|no call.?back|old a/?c|do not (call|verify))", re.I)
BUYER_GSTIN = "24ZZACB7319B1Z5"


def tesseract_lines(image: bytes, timeout_s: float = 60.0) -> tuple[list[dict], int, int]:
    """[{text, box(0..1000)}] per OCR line, plus the image size. Empty on any failure."""
    im = Image.open(io.BytesIO(image)).convert("L")
    w, h = im.size
    with tempfile.TemporaryDirectory() as d:
        p = Path(d) / "in.png"
        im.save(p)
        try:
            r = subprocess.run(["tesseract", str(p), "-", "--psm", "3", "tsv"], capture_output=True, text=True, timeout=timeout_s)
        except (OSError, subprocess.SubprocessError):
            return [], w, h
    if r.returncode != 0:
        return [], w, h
    groups: dict[tuple, dict] = {}
    for row in csv.DictReader(io.StringIO(r.stdout), delimiter="\t", quoting=csv.QUOTE_NONE):
        txt = (row.get("text") or "").strip()
        if row.get("level") != "5" or not txt:
            continue
        k = (row["block_num"], row["par_num"], row["line_num"])
        x, y, ww, hh = (int(row[c]) for c in ("left", "top", "width", "height"))
        g = groups.setdefault(k, {"words": [], "x0": x, "y0": y, "x1": x + ww, "y1": y + hh})
        g["words"].append(txt)
        g["x0"], g["y0"], g["x1"], g["y1"] = min(g["x0"], x), min(g["y0"], y), max(g["x1"], x + ww), max(g["y1"], y + hh)
    out = []
    for g in groups.values():
        out.append({"text": " ".join(g["words"]),
                    "box": [g["x0"] * 1000 // w, g["y0"] * 1000 // h, -(-g["x1"] * 1000 // w), -(-g["y1"] * 1000 // h)]})
    return out, w, h


def ocr_witness(image: bytes, master: dict, lines: list[dict] | None = None) -> Witness:
    if lines is None:
        lines, _, _ = tesseract_lines(image)
    if not lines:
        return Witness(ok=True, readable=False)
    w = Witness(ok=True, readable=True)
    full = "\n".join(ln["text"] for ln in lines)
    for g in GSTIN_RE.findall(full.upper()):
        if g != BUYER_GSTIN:
            w.issuer_gstin = g
            break
    folded = re.sub(r"[^a-z0-9]+", " ", full.lower())
    for v in sorted(master["vendors"], key=lambda v: -len(v["legal_name"])):
        if re.sub(r"[^a-z0-9]+", " ", v["legal_name"].lower()).strip() in folded:
            w.issuer_name = v["legal_name"]
            w.issuer_box = next((ln["box"] for ln in lines if v["legal_name"].split()[0].lower() in ln["text"].lower()), None)
            break
    for ln in lines:
        t = ln["text"]
        accts = [re.sub(r"\D", "", a) for a in ACCT_RE.findall(t)]
        accts = [a for a in accts if 9 <= len(a) <= 18]
        ifscs = IFSC_RE.findall(t.upper())
        if accts:
            for a in accts:
                w.bank_details.append({"account": a, "ifsc": ifscs[0] if ifscs else None, "account_name": None, "text": t,
                                       "handwritten": False, "box": ln["box"]})
        elif ifscs:
            last = w.bank_details[-1] if w.bank_details else None
            if last is not None and last["ifsc"] is None:
                last["ifsc"] = ifscs[0]  # IFSC printed on the line under the account number
                last["text"] += " / " + t
            else:
                w.bank_details.append({"account": None, "ifsc": ifscs[0], "account_name": None, "text": t,
                                       "handwritten": False, "box": ln["box"]})
        if NOTICE_RE.search(t):
            w.notices.append({"kind": "bank_change", "text": t, "handwritten": False, "faint_or_small": False, "box": ln["box"]})
    return w
