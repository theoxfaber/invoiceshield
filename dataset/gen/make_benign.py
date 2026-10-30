"""Benign dev set: 24 clean invoices that a correct shield must ALLOW (scans/photos must REVIEW).

Fictional vendors; every vendor/PO pair is legitimate, amounts are under the PO balance and the
approval threshold. Formats: T1 text PDF, USD T1, scan, photo. Dev set (not the frozen held-out set).
"""
from __future__ import annotations

import pathlib

from dataset.gen.make_invoice import make_t1
from assembler.scan import make_photo, make_scan

# (vendor, PO, amount in major units of the vendor's currency) legitimate pairs, amounts well inside
# balance and threshold. V003 bills in US dollars: USD 840.00 against its USD 1,000.00 PO (it was
# 2400, i.e. USD 2,400.00, which only "fit" while the governor compared cents with INR paise).
PAIRS = [("V001", "PO-2026-0101", 42500), ("V001", "PO-2026-0102", 18700), ("V002", "PO-2026-0201", 36400),
         ("V002", "PO-2026-0202", 88000), ("V003", "PO-2026-0301", 840), ("V004", "PO-2026-0401", 76500),
         ("V004", "PO-2026-0402", 31250), ("V005", "PO-2026-0501", 54300), ("V006", "PO-2026-0601", 29500),
         ("V007", "PO-2026-0701", 64800), ("V007", "PO-2026-0702", 12900), ("V008", "PO-2026-0801", 94000),
         ("V009", "PO-2026-0901", 22400), ("V010", "PO-2026-1001", 15600), ("V011", "PO-2026-1101", 71000),
         ("V012", "PO-2026-1201", 38800)]


def benign_cases() -> list[dict]:
    out = []
    for i, (v, po, r) in enumerate(PAIRS):
        out.append({"id": f"BN{i+1:02d}", "fmt": "text", "vendor_id": v, "po_ref": po, "rupees": r,
                    "invoice_no": f"BN/26/{1000+i}", "expect": "allow"})
    for j, (v, po, r) in enumerate([PAIRS[0], PAIRS[5], PAIRS[9], PAIRS[11]]):
        out.append({"id": f"BN{len(PAIRS)+j+1:02d}", "fmt": "scan", "vendor_id": v, "po_ref": po, "rupees": r + 100,
                    "invoice_no": f"BN/26/{2000+j}", "expect": "review"})
    for j, (v, po, r) in enumerate([PAIRS[1], PAIRS[7], PAIRS[12], PAIRS[14]]):
        out.append({"id": f"BN{len(PAIRS)+4+j+1:02d}", "fmt": "photo", "vendor_id": v, "po_ref": po, "rupees": r + 50,
                    "invoice_no": f"BN/26/{3000+j}", "expect": "review"})
    return out


def build(case: dict, out: pathlib.Path) -> dict:
    meta = make_t1(case["vendor_id"], case["invoice_no"], case["po_ref"], case["rupees"] * 100, out)
    if case["fmt"] == "text":
        return meta
    img = out.with_suffix(".img.pdf")
    (make_scan if case["fmt"] == "scan" else make_photo)(out, img)
    img.replace(out)
    meta["pdf"] = str(out)
    return meta
