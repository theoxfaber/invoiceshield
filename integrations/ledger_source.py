"""Approved payments, read from the governor's SQLite state (read-only).

The only source the exporters accept: ledger rows with status 'paid' (ALLOW, or a REVIEW an
approver resolved with approve=true), joined to the stored decision for the payee and to the
review resolution for the approver. Opened with mode=ro, so an export can never write state.
"""
from __future__ import annotations

import json
import pathlib
import sqlite3

from .common import ApprovedPayment

QUERY = """
SELECT l.decision_id, l.vendor_id, l.invoice_key, l.amount_paise, l.currency, l.po_ref, l.ts, d.body, r.approver
  FROM ledger l
  JOIN decisions d ON d.decision_id = l.decision_id
  LEFT JOIN review_resolutions r ON r.decision_id = l.decision_id AND r.approve = 1
 WHERE l.status = 'paid' AND (?1 IS NULL OR l.ts >= ?1) AND (?2 IS NULL OR l.ts < ?2)
 ORDER BY l.ts, l.id
"""


def approved_payments(db_path: str | pathlib.Path, since: int | None = None, until: int | None = None) -> list[ApprovedPayment]:
    uri = pathlib.Path(db_path).resolve().as_uri() + "?mode=ro"
    con = sqlite3.connect(uri, uri=True)
    try:
        rows = con.execute(QUERY, (since, until)).fetchall()
    finally:
        con.close()
    out = []
    for did, vid, inv, amt, cur, po, ts, body, approver in rows:
        payee = (json.loads(body).get("payee") or {}) if body else {}
        out.append(ApprovedPayment(decision_id=did, vendor_id=vid, invoice_key=inv, amount_minor=int(amt), currency=cur,
                                   po_ref=po, ts=int(ts), beneficiary_id=payee.get("beneficiary_id"), approver=approver))
    return out
