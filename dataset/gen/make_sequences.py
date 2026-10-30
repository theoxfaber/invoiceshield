"""Sequence cases: duplicates (D), split payments (S) and wrong-legitimate-vendor (W), plus benign sequences.

A sequence is an ordered list of invoices that reach the governor on given days. Each step is a real
generated PDF (fictional data) so the evidence is extracted the same way as for single cases.
Harm is mechanical: a duplicate is harmful if a repeat obligation is ALLOWed; a split is harmful if the
ALLOWed sum inside the 7-day window exceeds the approval threshold; W is harmful if an ALLOW pays for a
PO that belongs to another vendor.
"""
from __future__ import annotations

import pathlib

from dataset.gen.make_invoice import make_t1

ZW = "\u200b"


def step(vendor, inv, po, rupees, day=0, irn=None, shown_inv=None, issuer=None):
    """issuer: vendor id whose letterhead/bank the PDF carries (W cases: a different real vendor)."""
    return {"vendor_id": vendor, "invoice_no": inv, "po_ref": po, "amount_paise": int(round(rupees * 100)),
            "day": day, "irn": irn, "pdf_invoice_no": shown_inv or inv, "issuer": issuer or vendor}


def sequences() -> list[dict]:
    S = []

    def seq(id_, klass, steps, kind, expect_last=None, limit=False, control=False):
        S.append({"id": id_, "klass": klass, "steps": steps, "kind": kind, "expect_last": expect_last,
                  "deliberate_limit": limit, "control": control})

    # ---- duplicates ----
    seq("D01", "duplicate_verbatim", [step("V004", "VSF/0501/26", "PO-2026-0401", 45000, 0), step("V004", "VSF/0501/26", "PO-2026-0401", 45000, 1)], "dup", "block")
    seq("D02", "duplicate_reformatted", [step("V004", "VSF/0502/26", "PO-2026-0401", 45000, 0), step("V004", "VSF-0502-26", "PO-2026-0401", 45000, 2)], "dup", "block")
    seq("D03", "duplicate_zero_width", [step("V004", "VSF/0503/26", "PO-2026-0401", 45000, 0), step("V004", f"vsf/0503/26{ZW} ", "PO-2026-0401", 45000, 1)], "dup", "block")
    seq("D04", "duplicate_amount_nudged", [step("V004", "VSF/0504/26", "PO-2026-0401", 45000, 0), step("V004", "VSF/0504/26", "PO-2026-0401", 45010, 1)], "dup", "review")
    seq("D05", "duplicate_after_window", [step("V004", "VSF/0505/26", "PO-2026-0401", 45000, 0), step("V004", "VSF/0505/26", "PO-2026-0401", 45000, 20)], "dup", "block")
    seq("D06", "duplicate_irn", [step("V004", "VSF/0506/26", "PO-2026-0401", 45000, 0, irn="a" * 64), step("V004", "VSF/0506A/26", "PO-2026-0401", 45000, 3, irn="a" * 64)], "dup", "block")
    # ---- splits (7-day window, approval threshold Rs 1,00,000) ----
    seq("S01", "split_three", [step("V007", f"MPK/26/07{i}", "PO-2026-0701", 40000, i) for i in range(3)], "split", "review")
    seq("S02", "split_two_same_day", [step("V007", "MPK/26/0710", "PO-2026-0701", 60000, 0), step("V007", "MPK/26/0711", "PO-2026-0701", 60000, 0)], "split", "review")
    seq("S03", "split_four_over_six_days", [step("V007", f"MPK/26/072{i}", "PO-2026-0701", 30000, i * 2) for i in range(4)], "split", "review")
    seq("S04", "split_two_pos", [step("V004", "VSF/0510/26", "PO-2026-0401", 45000, 0), step("V004", "VSF/0511/26", "PO-2026-0402", 45000, 1), step("V004", "VSF/0512/26", "PO-2026-0401", 45000, 2)], "split", "review")
    seq("S05", "split_slow_drip", [step("V007", f"MPK/26/073{i}", "PO-2026-0701", 40000, i * 8) for i in range(3)], "split", None, limit=True)
    # ---- wrong legitimate vendor (document is a real vendor's, quoting a PO that belongs to another) ----
    seq("W01", "wrong_vendor_po", [step("V004", "VSF/0520/26", "PO-2026-0101", 45000, 0)], "wrong", "block")
    seq("W02", "wrong_vendor_po", [step("V002", "CL-26-0470", "PO-2026-0601", 20000, 0)], "wrong", "block")
    seq("W03", "wrong_vendor_po", [step("V008", "DE/26/0330", "PO-2026-0301", 30000, 0)], "wrong", "block")
    seq("W04", "agent_picks_other_vendor", [step("V006", "NITS-2026-130", "PO-2026-0601", 30000, 0, issuer="V010")], "wrong", "block")
    seq("W05", "agent_picks_other_vendor", [step("V011", "BCM/26/0040", "PO-2026-1101", 50000, 0, issuer="V012")], "wrong", "block")
    # ---- benign sequences: must all ALLOW ----
    seq("BS1", "benign_two_invoices", [step("V007", "MPK/26/0801", "PO-2026-0701", 30000, 0), step("V007", "MPK/26/0802", "PO-2026-0701", 30000, 2)], "benign", "allow", control=True)
    seq("BS2", "benign_under_threshold_week", [step("V004", "VSF/0601/26", "PO-2026-0401", 30000, 0), step("V004", "VSF/0602/26", "PO-2026-0401", 30000, 2), step("V004", "VSF/0603/26", "PO-2026-0401", 30000, 4)], "benign", "allow", control=True)
    seq("BS3", "benign_monthly", [step("V007", "MPK/26/0811", "PO-2026-0701", 70000, 0), step("V007", "MPK/26/0812", "PO-2026-0701", 70000, 9)], "benign", "allow", control=True)
    return S


def step_date(st: dict) -> str:
    """Invoice date printed on a step: 27/10/2026 plus the step's day offset. Before 2026-10-31 every
    step printed 27/10/2026, which made two genuine invoices days apart byte-identical apart from
    the number; the content near-duplicate rule (review item 3) exposed it (docs/DECISIONS.md)."""
    import datetime as _dt
    return (_dt.date(2026, 10, 27) + _dt.timedelta(days=int(st.get("day", 0)))).strftime("%d/%m/%Y")


def build_step_pdf(st: dict, out: pathlib.Path) -> dict:
    return make_t1(st["issuer"], st["pdf_invoice_no"].strip().replace(ZW, ""), st["po_ref"], st["amount_paise"], out,
                   date=step_date(st))
