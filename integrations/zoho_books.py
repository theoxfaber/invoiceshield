"""Zoho Books (API v3) adapter: vendor contacts and vendor payments.

NOT TESTED AGAINST A LIVE ZOHO BOOKS ORGANISATION. Payload shapes follow the public API v3 docs
(https://www.zoho.com/books/api/v3/contacts/ and /vendor-payments/, read 2026-10-30):
contacts carry contact_name, company_name, contact_type='vendor', gst_no, gst_treatment
(business_gst | business_none | overseas | consumer) and place_of_contact (state code, e.g. 'TN');
list responses carry page_context.has_more_page; POST /vendorpayments takes vendor_id, amount
(required), date, payment_mode, reference_number, description, paid_through_account_id and
bills[{bill_id, amount_applied}]. Auth header: 'Authorization: Zoho-oauthtoken <token>' and an
organization_id query parameter. Tests drive ZohoBooksClient only through httpx.MockTransport.

Bank details are deliberately not exported to or imported from Zoho contacts here: the bank
account that gets paid comes from the governor's vendor master, and changes to it go through the
maker-checker /v1/master-changes workflow, never through a books sync.
"""
from __future__ import annotations

import httpx

from .common import ApprovedPayment, MasterDiff, diff_against_master, minor_to_decimal_str

DEFAULT_BASE = "https://www.zohoapis.in/books/v3"  # India data centre; .com/.eu/... per org

# GSTIN state code -> Zoho place_of_contact code (subset; extend as needed).
PLACE = {"07": "DL", "09": "UP", "19": "WB", "24": "GJ", "27": "MH", "29": "KA", "32": "KL", "33": "TN", "36": "TS", "37": "AP"}

# custom field label that carries the InvoiceShield vendor_id on the Zoho contact
VENDOR_ID_FIELD = "invoiceshield_vendor_id"


def contact_payload(v: dict) -> dict:
    gstin = v.get("gstin")
    p = {
        "contact_name": v["legal_name"],
        "company_name": v["legal_name"],
        "contact_type": "vendor",
        "gst_treatment": "business_gst" if gstin else ("overseas" if not v.get("pan") else "business_none"),
        "custom_fields": [{"label": VENDOR_ID_FIELD, "value": v["vendor_id"]}],
    }
    if gstin:
        p["gst_no"] = gstin
        if gstin[:2] in PLACE:
            p["place_of_contact"] = PLACE[gstin[:2]]
    return p


def export_vendor_contacts(master: dict) -> list[dict]:
    """POST /contacts bodies for every vendor in the master (no bank details; see module doc)."""
    return [contact_payload(v) for v in master.get("vendors", [])]


def vendor_payment_payload(p: ApprovedPayment | dict, zoho_vendor_id: str, paid_through_account_id: str,
                           bill_id: str | None = None, payment_mode: str = "Bank Transfer") -> dict:
    p = p if isinstance(p, ApprovedPayment) else ApprovedPayment.from_dict(p)
    amount = float(minor_to_decimal_str(p.amount_minor))  # 2-decimal value; exact in JSON text
    body = {
        "vendor_id": zoho_vendor_id,
        "amount": amount,
        "date": p.date_ist().isoformat(),
        "payment_mode": payment_mode,
        "reference_number": p.decision_id,
        "description": f"InvoiceShield {p.decision_id}; invoice {p.invoice_key}; PO {p.po_ref}" + (f"; approved by {p.approver}" if p.approver else ""),
        "paid_through_account_id": paid_through_account_id,
    }
    if bill_id:
        body["bills"] = [{"bill_id": bill_id, "amount_applied": amount}]
    return body


def export_vendor_payments(payments: list, vendor_map: dict[str, str], paid_through_account_id: str,
                           bill_map: dict[str, str] | None = None) -> tuple[list[dict], list[dict]]:
    """(bodies, skipped). `vendor_map`: InvoiceShield vendor_id -> Zoho contact_id.
    `bill_map`: invoice_key -> Zoho bill_id, when the bills already exist in Zoho."""
    out, skipped = [], []
    for p in payments:
        p = p if isinstance(p, ApprovedPayment) else ApprovedPayment.from_dict(p)
        zid = vendor_map.get(p.vendor_id)
        if zid is None:
            skipped.append({"decision_id": p.decision_id, "reason": f"no Zoho contact mapped for {p.vendor_id}"})
            continue
        out.append(vendor_payment_payload(p, zid, paid_through_account_id, (bill_map or {}).get(p.invoice_key)))
    return out, skipped


def _vendor_id_of(contact: dict) -> str | None:
    for cf in contact.get("custom_fields") or []:
        if cf.get("label") == VENDOR_ID_FIELD or cf.get("api_name") == "cf_" + VENDOR_ID_FIELD:
            return str(cf.get("value")) if cf.get("value") else None
    return None


def contacts_to_records(contacts: list[dict]) -> list[dict]:
    recs = []
    for c in contacts:
        if c.get("contact_type", "vendor") != "vendor":
            continue
        recs.append({"vendor_id": _vendor_id_of(c), "zoho_contact_id": c.get("contact_id"),
                     "legal_name": c.get("company_name") or c.get("contact_name"), "gstin": c.get("gst_no") or None})
    return recs


def import_vendor_contacts(contacts: list[dict], master: dict) -> MasterDiff:
    """Diff Zoho vendor contacts against the vendor master (names/GSTIN only). Nothing applied."""
    return diff_against_master(master, contacts_to_records(contacts), "zoho_books")


def reconcile_payments(zoho_payments: list[dict], approved: list) -> dict:
    """Books-vs-gate check: every Zoho vendor payment should carry an InvoiceShield decision id
    as reference_number that the governor approved, for the same amount."""
    by_id = {}
    for p in approved:
        p = p if isinstance(p, ApprovedPayment) else ApprovedPayment.from_dict(p)
        by_id[p.decision_id] = p
    matched, unknown, amount_mismatch = [], [], []
    for z in zoho_payments:
        ref = z.get("reference_number") or ""
        gp = by_id.get(ref)
        if gp is None:
            unknown.append({"payment_id": z.get("payment_id"), "reference_number": ref, "amount": z.get("amount")})
            continue
        if minor_to_decimal_str(gp.amount_minor) != f"{float(z.get('amount', 0)):.2f}":
            amount_mismatch.append({"payment_id": z.get("payment_id"), "decision_id": ref, "zoho": z.get("amount"), "governor_minor": gp.amount_minor})
        else:
            matched.append(ref)
    seen = set(matched) | {m["decision_id"] for m in amount_mismatch}
    return {"matched": matched, "not_approved_by_gate": unknown, "amount_mismatch": amount_mismatch,
            "approved_not_in_books": sorted(set(by_id) - seen)}


class ZohoError(RuntimeError):
    pass


class ZohoBooksClient:
    """Thin client. Refuses to talk to the network unless built with a transport (tests) or
    allow_live=True, because no live organisation has been tested."""

    def __init__(self, organization_id: str, token: str, base_url: str = DEFAULT_BASE,
                 transport: httpx.BaseTransport | None = None, allow_live: bool = False, timeout: float = 20.0):
        if transport is None and not allow_live:
            raise ZohoError("ZohoBooksClient: not tested against a live Zoho Books org; pass a transport or allow_live=True deliberately")
        self.org = organization_id
        self._http = httpx.Client(base_url=base_url, transport=transport, timeout=timeout,
                                  headers={"Authorization": f"Zoho-oauthtoken {token}"})

    def close(self) -> None:
        self._http.close()

    def _call(self, method: str, path: str, **kw) -> dict:
        params = {"organization_id": self.org, **kw.pop("params", {})}
        r = self._http.request(method, path, params=params, **kw)
        try:
            j = r.json()
        except ValueError:
            raise ZohoError(f"{method} {path}: HTTP {r.status_code}, non-JSON body")
        if r.status_code >= 400 or j.get("code", 0) != 0:
            raise ZohoError(f"{method} {path}: HTTP {r.status_code} code={j.get('code')} {j.get('message')}")
        return j

    def _pages(self, path: str, key: str, params: dict, max_pages: int) -> list[dict]:
        out, page = [], 1
        while page <= max_pages:
            j = self._call("GET", path, params={**params, "page": page, "per_page": 200})
            out.extend(j.get(key, []))
            if not (j.get("page_context") or {}).get("has_more_page"):
                return out
            page += 1
        raise ZohoError(f"more than {max_pages} pages of {key}; refusing to continue")

    def list_vendor_contacts(self, max_pages: int = 50) -> list[dict]:
        return self._pages("/contacts", "contacts", {"contact_type": "vendor"}, max_pages)

    def list_vendor_payments(self, max_pages: int = 50) -> list[dict]:
        return self._pages("/vendorpayments", "vendorpayments", {}, max_pages)

    def create_vendor_payment(self, body: dict) -> dict:
        return self._call("POST", "/vendorpayments", json=body).get("vendorpayment", {})
