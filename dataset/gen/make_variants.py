"""Format variants of the risk-set attacks: same attack, different layout/medium.

Each risk-set spec is re-rendered as T2 layout (text PDF) and as a scan of T1. Ground truth is
inherited from the base case. A SHA-256 over the sorted manifest lets the set be frozen and
checked later. Dev material until the held-out split is declared.
"""
from __future__ import annotations

import hashlib
import json
import pathlib

from assembler.scan import make_photo, make_scan
from dataset.gen.make_invoice import make_t1, repo_relative
from dataset.gen.make_riskset import cases, legit_for

VARIANTS = ("t2", "scan", "photo")


def build(out_dir: pathlib.Path) -> list[dict]:
    out_dir.mkdir(parents=True, exist_ok=True)
    built = []
    for c in cases():
        for var in VARIANTS:
            g = dict(c["gen"])
            vid = g.pop("vid", None) or c["vendor_id"]
            cid = f"{c['id']}-{var}"
            pdf = out_dir / f"{cid}.pdf"
            m = make_t1(vid, c["invoice_no"], c["po_ref"], c["amount_paise"], pdf,
                        layout="t2" if var == "t2" else "t1", **g)
            if var in ("scan", "photo"):
                tmp = pdf.with_suffix(".img.pdf")
                (make_scan if var == "scan" else make_photo)(pdf, tmp)
                tmp.replace(pdf)
            built.append({**{k: v for k, v in c.items() if k != "gen"}, "id": cid, "base_id": c["id"],
                          "variant": var, "pdf": str(pdf), "cover_email": m["cover_email"],
                          "legit": legit_for(c)})
    return built


def manifest_hash(built: list[dict]) -> str:
    keep = [{k: v for k, v in b.items() if k not in ("pdf",)} for b in sorted(built, key=lambda b: b["id"])]
    return hashlib.sha256(json.dumps(keep, sort_keys=True, ensure_ascii=False).encode()).hexdigest()


if __name__ == "__main__":
    import sys
    d = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else "eval/variants")
    b = build(d)
    h = manifest_hash(b)
    (d / "manifest.json").write_text(json.dumps([{**x, "pdf": repo_relative(x["pdf"])} for x in b],
                                                indent=1, ensure_ascii=False) + "\n")
    print(len(b), "variant cases", h)
