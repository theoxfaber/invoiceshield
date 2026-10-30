"""Loaders for the third-party invoice sets (see dataset/real/sources.py for provenance).

Each loader returns cases: {"id", "source", "group", "text" | "pdf", "text_source", "gold": {...}}.
Gold values are plaintext in memory only; dataset/real/labels_*.json commits them hashed.

MIDD gold (per invoice):
  supplier_gstin / buyer_gstin  the true GSTIN: MIDD's SUPP_G / BUY_G tag, OCR slips repaired
                                (structural.gstin.repair_ocr, checksum-gated) or, when the tag is
                                missing, looked up by the SUPP_N tag; the printed token is kept as
                                *_printed (the text-faithful gold)
  invoice_no                    INV_NO tag tokens joined (no spaces)
  total_paise                   GT_AMT tag, Indian digit grouping
  currency                      "INR" (every MIDD invoice is an Indian GST tax invoice)
  bank_account / ifsc           hand labels per supplier (data/third_party/raw/midd/hand_labels.json),
                                set only on invoices whose text prints that value, wherever it is (exactly or
                                with <=2 (account) / <=3 (IFSC, O/0 folded) OCR slips); the
                                printed token is kept as *_printed
  line_items                    not labelled in MIDD (None)
"""
from __future__ import annotations

import csv
import hashlib
import io
import json
import re
from functools import lru_cache

from dataset.real.sources import RAW
from structural.gstin import repair_ocr

MIDD_DIR = RAW / "midd" / "IOB"
HAND = RAW / "midd" / "hand_labels.json"
SPARROW_DIR = RAW / "sparrow"

_BANK_BOX_RE = re.compile(r"\bA\s*/\s*[Cc]\b|\bBank\s+Name\b|\bIFS\w*\s+Code\b|\bIFSC\b", re.I)


def _rows(path):
    with open(path, encoding="utf-8", errors="replace", newline="") as f:
        rows = list(csv.reader(f))
    return [r for r in rows[1:] if r and len(r) >= 2]


def _spans(rows, tag):
    out, cur = [], None
    for tok, t in ((r[0], r[1]) for r in rows):
        if t == f"B-{tag}":
            if cur:
                out.append(cur)
            cur = [tok]
        elif t == f"I-{tag}" and cur is not None:
            cur.append(tok)
        else:
            if cur:
                out.append(cur)
            cur = None
    if cur:
        out.append(cur)
    return out


def indian_amount_paise(s: str) -> int | None:
    s = (s or "").replace(" ", "").replace(",", "")
    m = re.fullmatch(r"(?:Rs\.?|INR|₹)?(\d+)(?:\.(\d{1,2}))?", s)
    if not m:
        return None
    return int(m.group(1)) * 100 + int(((m.group(2) or "") + "00")[:2])


_OCR_DIGIT = str.maketrans({"O": "0", "o": "0", "D": "0", "I": "1", "l": "1", "|": "1", "S": "5"})


def _printed_variant(text: str, canonical: str | None, max_diff: int = 2) -> str | None:
    """The token in `text` that prints `canonical` (exactly or with <= max_diff OCR slips), else None."""
    if not canonical:
        return None
    for tok in re.findall(r"[0-9A-Za-z|]{%d,%d}" % (len(canonical) - 1, len(canonical) + 1), text):
        if tok == canonical:
            return tok
    for tok in re.findall(r"[0-9A-Za-z|]{%d}" % len(canonical), text):
        a = tok.upper().translate(_OCR_DIGIT)
        if canonical[:4].isalpha():  # IFSC: 4 letters, then '0' + 6 alnum; compare with O/0 folded
            a, b = tok.upper().replace("O", "0"), canonical.replace("O", "0")
        else:
            b = canonical
        if sum(x != y for x, y in zip(a, b)) <= max_diff:
            return tok
    return None


def _true_gstin(printed: str | None, known: list[str]) -> str | None:
    if not printed:
        return None
    t = re.sub(r"[^0-9A-Z$|]", "", printed.upper())
    for cand in (t, t[-15:], t[:15]):
        r = repair_ocr(cand)
        if r:
            return r
    # unrepairable: nearest known canonical within 3 edits (same length), else None
    best = min(known, key=lambda k: sum(a != b for a, b in zip(k, t[:15].ljust(15))), default=None)
    if best and sum(a != b for a, b in zip(best, t[:15].ljust(15))) <= 3:
        return best
    return None


@lru_cache(maxsize=1)
def hand_labels() -> dict:
    return json.loads(HAND.read_text())


def load_midd() -> list[dict]:
    hl = hand_labels()
    sup_known = list(hl["suppliers"])
    cases = []
    for path in sorted(MIDD_DIR.glob("Layout*/*.csv")):
        rows = _rows(path)
        toks = [r[0] for r in rows]
        text = " ".join(toks)
        sg = _spans(rows, "SUPP_G")
        bg = _spans(rows, "BUY_G")
        sup_printed = "".join(sg[0]) if sg else None
        sup = _true_gstin(sup_printed, sup_known)
        if sup is None:
            name = " ".join(sum(_spans(rows, "SUPP_N")[:1], [])).upper()
            sup = hl["supplier_by_name"].get(name)
        buy_printed = "".join(bg[0]) if bg else None
        buy = _true_gstin(buy_printed, hl["buyers"])
        inv = _spans(rows, "INV_NO")
        gt = _spans(rows, "GT_AMT")
        s = hl["suppliers"].get(sup or "", {})
        box = bool(_BANK_BOX_RE.search(text))
        acct_tok = _printed_variant(text, s.get("bank_account"))
        if acct_tok is None and s.get("bank_account") and s["bank_account"] in re.sub(r"(?<=\d) (?=\d)", "", text):
            acct_tok = s["bank_account"]  # printed in digit groups ('571131 2022')
        ifsc_tok = _printed_variant(text, s.get("ifsc"), max_diff=3)
        raw = path.read_bytes()
        cases.append({
            "id": f"midd/{path.parent.name}/{path.stem}",
            "source": "midd",
            "group": path.parent.name,
            "supplier_key": s.get("key"),
            "file_sha256": hashlib.sha256(raw).hexdigest(),
            "text_sha256": hashlib.sha256(text.encode()).hexdigest(),
            "text": text,
            "text_source": "ocr",  # MIDD tokens are OCR output of scanned/printed invoices
            "gold": {
                "supplier_gstin": sup,
                "supplier_gstin_printed": sup_printed,
                "buyer_gstin": buy,
                "invoice_no": "".join(inv[0]) if inv else None,
                "total_paise": indian_amount_paise(" ".join(gt[0])) if gt else None,
                "currency": "INR",
                "bank_box_printed": box,
                "bank_account": s.get("bank_account") if acct_tok else None,
                "bank_account_printed": acct_tok,
                "ifsc": s.get("ifsc") if ifsc_tok else None,
                "ifsc_printed": ifsc_tok,
                "account_unrecoverable": bool(s.get("account_unrecoverable")) and box,
                "line_items": None,
            },
        })
    return cases


def _sparrow_amount_cents(s: str | None) -> int | None:
    """'$ 978,12' / '$ 1 234,50' / '978,12' (decimal comma, space thousands) -> cents."""
    if not s:
        return None
    t = re.sub(r"[^\d,.]", "", s)
    if "," in t and (t.rfind(",") > t.rfind(".")):
        t = t.replace(".", "").replace(",", ".")
    else:
        t = t.replace(",", "")
    try:
        return round(float(t) * 100)
    except ValueError:
        return None


def load_sparrow(materialise: bool = True) -> list[dict]:
    """76 rendered invoice images (test 26 + validation 50). Each is wrapped as an image-only
    PDF in data/third_party/raw/sparrow/pdf/ so it goes through the real assemble() OCR path."""
    import pyarrow.parquet as pq
    from PIL import Image

    out_dir = SPARROW_DIR / "pdf"
    out_dir.mkdir(parents=True, exist_ok=True)
    cases = []
    for split in ("test", "validation"):
        for i, row in enumerate(pq.read_table(SPARROW_DIR / f"{split}.parquet").to_pylist()):
            gt = json.loads(row["ground_truth"])["gt_parse"]
            h, items, summ = gt.get("header", {}), gt.get("items", []), gt.get("summary", {})
            img = row["image"]["bytes"]
            pdf = out_dir / f"{split}-{i:03d}.pdf"
            if materialise and not pdf.exists():
                Image.open(io.BytesIO(img)).convert("RGB").save(pdf, "PDF", resolution=150)
            cur = "USD" if "$" in (summ.get("total_gross_worth") or "") else None
            cases.append({
                "id": f"sparrow/{split}-{i:03d}",
                "source": "sparrow",
                "group": split,
                "file_sha256": hashlib.sha256(img).hexdigest(),
                "pdf": str(pdf),
                "text_source": "ocr",
                "gold": {
                    "supplier_gstin": None,
                    "invoice_no": h.get("invoice_no"),
                    "total_paise": _sparrow_amount_cents(summ.get("total_gross_worth")),
                    "currency": cur,
                    "iban": (h.get("iban") or "").replace(" ", "") or None,
                    "bank_account": None,
                    "ifsc": None,
                    "line_items": [{"description": it.get("item_desc", ""),
                                    "amount_paise": _sparrow_amount_cents(it.get("item_gross_worth"))} for it in items],
                },
            })
    return cases


LABELS_DIR = __import__("pathlib").Path(__file__).resolve().parent
_SALT = "invoiceshield-real-v1:"


def _h(v) -> str | None:
    return None if v in (None, "") else hashlib.sha256((_SALT + str(v)).encode()).hexdigest()[:16]


def anonymised_labels(cases: list[dict]) -> list[dict]:
    """Committable labels: identifiers hashed (salted sha256, 16 hex), amounts/currency kept.
    The source data is public, so a hash is a commitment for re-derivation, not secrecy."""
    out = []
    for c in cases:
        g = c["gold"]
        row = {"id": c["id"], "group": c["group"], "supplier_key": c.get("supplier_key"), "file_sha256": c["file_sha256"],
               "total_paise": g.get("total_paise"), "currency": g.get("currency"),
               "n_line_items": len(g["line_items"]) if g.get("line_items") is not None else None}
        for k in ("supplier_gstin", "supplier_gstin_printed", "buyer_gstin", "invoice_no", "bank_account",
                  "bank_account_printed", "ifsc", "ifsc_printed", "iban"):
            if k in g:
                row[f"{k}_h"] = _h(g[k])
        out.append(row)
    return out


def write_labels() -> dict:
    import json as _json
    counts = {}
    for name, loader in (("midd", load_midd), ("sparrow", lambda: load_sparrow(materialise=False))):
        rows = anonymised_labels(loader())
        (LABELS_DIR / f"labels_{name}.json").write_text(_json.dumps(rows, indent=0, ensure_ascii=False) + "\n")
        counts[name] = len(rows)
    return counts


def check_labels(name: str, cases: list[dict]) -> list[str]:
    """Ids whose freshly derived labels differ from the committed hashes (empty = reproduced)."""
    import json as _json
    p = LABELS_DIR / f"labels_{name}.json"
    if not p.exists():
        return ["<no committed labels>"]
    want = {r["id"]: r for r in _json.loads(p.read_text())}
    return [r["id"] for r in anonymised_labels(cases) if want.get(r["id"]) != r]
