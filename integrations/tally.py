"""Tally (TallyPrime / Tally.ERP 9) XML adapter: vendor master and approved payments.

NOT TESTED AGAINST A LIVE TALLY. The shapes follow Tally's documented XML import envelope
(ENVELOPE > HEADER/TALLYREQUEST 'Import Data' > BODY > IMPORTDATA > REQUESTDESC/REQUESTDATA >
TALLYMESSAGE) with LEDGER masters under 'Sundry Creditors' and 'Payment' vouchers. Tests use
fixtures only (tests/fixtures/integrations/). Before any real use, import into a scratch
company and compare.

  export_vendor_masters(master)          -> XML string of LEDGER masters (bank details included)
  export_payment_vouchers(payments, ...) -> (XML string of Payment vouchers, skipped list)
  import_vendor_masters(xml, master)     -> MasterDiff (nothing applied; bank differences become
                                            proposed /v1/master-changes bodies)

Tally voucher sign convention used here: in a Payment voucher the party (debit) line has
ISDEEMEDPOSITIVE=Yes and a negative AMOUNT; the bank (credit) line ISDEEMEDPOSITIVE=No and a
positive AMOUNT. Only INR (the company's base currency) is exported; other currencies are
skipped and reported, because a forex voucher needs a rate the governor does not hold.
"""
from __future__ import annotations

import re
import xml.etree.ElementTree as ET

from .common import ApprovedPayment, MasterDiff, decimal_str_to_minor, diff_against_master, minor_to_decimal_str

MAX_IMPORT_BYTES = 20 * 1024 * 1024
VENDOR_GROUP = "Sundry Creditors"

# GSTIN state codes -> state names as Tally's LEDGERSTATENAME expects (subset in use + common ones).
GST_STATES = {
    "07": "Delhi", "09": "Uttar Pradesh", "19": "West Bengal", "24": "Gujarat", "27": "Maharashtra",
    "29": "Karnataka", "32": "Kerala", "33": "Tamil Nadu", "36": "Telangana", "37": "Andhra Pradesh",
}


def _sub(parent: ET.Element, tag: str, text: str | None = None, **attrs) -> ET.Element:
    e = ET.SubElement(parent, tag, attrs)
    if text is not None:
        e.text = text
    return e


def _envelope(report: str, company: str | None) -> tuple[ET.Element, ET.Element]:
    env = ET.Element("ENVELOPE")
    _sub(_sub(env, "HEADER"), "TALLYREQUEST", "Import Data")
    imp = _sub(_sub(env, "BODY"), "IMPORTDATA")
    desc = _sub(imp, "REQUESTDESC")
    _sub(desc, "REPORTNAME", report)
    if company:
        _sub(_sub(desc, "STATICVARIABLES"), "SVCURRENTCOMPANY", company)
    data = _sub(imp, "REQUESTDATA")
    return env, data


def _tostring(env: ET.Element) -> str:
    ET.indent(env, space=" ")
    return ET.tostring(env, encoding="unicode", xml_declaration=False)


def export_vendor_masters(master: dict, company: str | None = None) -> str:
    """One LEDGER per vendor. The InvoiceShield vendor_id is the ledger's second NAME (an alias),
    which is how import_vendor_masters maps a Tally ledger back to a vendor."""
    env, data = _envelope("All Masters", company)
    for v in master.get("vendors", []):
        msg = _sub(data, "TALLYMESSAGE")
        led = _sub(msg, "LEDGER", NAME=v["legal_name"], ACTION="Create")
        names = _sub(led, "NAME.LIST", TYPE="String")
        _sub(names, "NAME", v["legal_name"])
        _sub(names, "NAME", v["vendor_id"])
        _sub(led, "PARENT", VENDOR_GROUP)
        _sub(led, "MAILINGNAME", v["legal_name"])
        _sub(led, "ISBILLWISEON", "Yes")
        gstin = v.get("gstin")
        if gstin:
            _sub(led, "COUNTRYOFRESIDENCE", "India")
            state = GST_STATES.get(gstin[:2])
            if state:
                _sub(led, "LEDGERSTATENAME", state)
            _sub(led, "GSTREGISTRATIONTYPE", "Regular")
            _sub(led, "PARTYGSTIN", gstin)
        else:
            _sub(led, "GSTREGISTRATIONTYPE", "Unregistered/Consumer")
        if v.get("pan"):
            _sub(led, "INCOMETAXNUMBER", v["pan"])
        pay = _sub(led, "PAYMENTDETAILS.LIST")
        _sub(pay, "IFSCODE", v["ifsc"])
        _sub(pay, "BANKNAME", v["bank_name"])
        _sub(pay, "ACCOUNTNUMBER", v["bank_account"])
        _sub(pay, "PAYMENTFAVOURING", v["legal_name"])
        _sub(pay, "TRANSACTIONNAME", "Primary")
        _sub(pay, "SETASDEFAULT", "Yes")
    return _tostring(env)


def export_payment_vouchers(payments: list[ApprovedPayment | dict], master: dict, bank_ledger: str,
                            company: str | None = None) -> tuple[str, list[dict]]:
    """Payment vouchers for governor-approved payments. Returns (xml, skipped)."""
    names = {v["vendor_id"]: v["legal_name"] for v in master.get("vendors", [])}
    env, data = _envelope("Vouchers", company)
    skipped: list[dict] = []
    for p in payments:
        p = p if isinstance(p, ApprovedPayment) else ApprovedPayment.from_dict(p)
        if p.currency != "INR":
            skipped.append({"decision_id": p.decision_id, "reason": f"currency {p.currency}: forex vouchers need a rate; enter manually"})
            continue
        party = names.get(p.vendor_id)
        if party is None:
            skipped.append({"decision_id": p.decision_id, "reason": f"vendor {p.vendor_id} not in master"})
            continue
        amt = minor_to_decimal_str(p.amount_minor)
        msg = _sub(data, "TALLYMESSAGE")
        v = _sub(msg, "VOUCHER", VCHTYPE="Payment", ACTION="Create")
        _sub(v, "DATE", p.date_ist().strftime("%Y%m%d"))
        _sub(v, "VOUCHERTYPENAME", "Payment")
        # Our decision id is the voucher number: re-importing the same file cannot double-post
        # silently in a company that enforces unique voucher numbers.
        _sub(v, "VOUCHERNUMBER", p.decision_id)
        _sub(v, "PARTYLEDGERNAME", party)
        narration = f"InvoiceShield {p.decision_id}; invoice {p.invoice_key}; PO {p.po_ref}"
        if p.approver:
            narration += f"; approved by {p.approver}"
        _sub(v, "NARRATION", narration)
        dr = _sub(v, "ALLLEDGERENTRIES.LIST")
        _sub(dr, "LEDGERNAME", party)
        _sub(dr, "ISDEEMEDPOSITIVE", "Yes")
        _sub(dr, "AMOUNT", "-" + amt)
        bill = _sub(dr, "BILLALLOCATIONS.LIST")
        _sub(bill, "NAME", p.invoice_key)
        _sub(bill, "BILLTYPE", "Agst Ref")
        _sub(bill, "AMOUNT", "-" + amt)
        cr = _sub(v, "ALLLEDGERENTRIES.LIST")
        _sub(cr, "LEDGERNAME", bank_ledger)
        _sub(cr, "ISDEEMEDPOSITIVE", "No")
        _sub(cr, "AMOUNT", amt)
    return _tostring(env), skipped


_FORBIDDEN = re.compile(rb"<!\s*(DOCTYPE|ENTITY)", re.I)


def parse_safely(xml: str | bytes) -> ET.Element:
    """Parse untrusted XML from an accounting export. DTDs and entities are refused outright
    (XXE / entity-expansion bombs); size is capped."""
    b = xml.encode() if isinstance(xml, str) else xml
    if len(b) > MAX_IMPORT_BYTES:
        raise ValueError("Tally XML too large")
    if _FORBIDDEN.search(b):
        raise ValueError("Tally XML with a DOCTYPE/ENTITY declaration refused")
    return ET.fromstring(b)


def _text(e: ET.Element, tag: str) -> str | None:
    c = e.find(tag)
    return c.text.strip() if c is not None and c.text and c.text.strip() else None


def ledgers_from_xml(xml: str | bytes, vendor_group: str = VENDOR_GROUP) -> list[dict]:
    """Vendor-like LEDGER records from a Tally masters export (ours or Tally's own)."""
    root = parse_safely(xml)
    out = []
    for led in root.iter("LEDGER"):
        parent = _text(led, "PARENT")
        if parent is not None and parent != vendor_group:
            continue
        names = [n.text.strip() for n in led.iter("NAME") if n.text and n.text.strip()]
        name = led.get("NAME") or (names[0] if names else None)
        aliases = [n for n in names if n != name]
        vid = next((a for a in aliases if re.fullmatch(r"V\d{3,}", a)), None)
        rec = {"vendor_id": vid, "legal_name": name, "gstin": _text(led, "PARTYGSTIN"), "pan": _text(led, "INCOMETAXNUMBER")}
        pay = led.find("PAYMENTDETAILS.LIST")
        if pay is not None:
            rec.update(bank_account=_text(pay, "ACCOUNTNUMBER"), ifsc=_text(pay, "IFSCODE"), bank_name=_text(pay, "BANKNAME"))
        out.append(rec)
    return out


def import_vendor_masters(xml: str | bytes, master: dict, vendor_map: dict[str, str] | None = None) -> MasterDiff:
    """Diff a Tally masters export against the vendor master. `vendor_map` maps a Tally ledger
    name to a vendor_id for ledgers that do not carry the vendor_id alias. Nothing is applied."""
    recs = ledgers_from_xml(xml)
    for r in recs:
        if r["vendor_id"] is None and vendor_map and r["legal_name"] in vendor_map:
            r["vendor_id"] = vendor_map[r["legal_name"]]
    return diff_against_master(master, recs, "tally")


def vouchers_from_xml(xml: str | bytes) -> list[dict]:
    """Payment vouchers in a Tally export: number, date, party, amount (minor units)."""
    root = parse_safely(xml)
    out = []
    for v in root.iter("VOUCHER"):
        if (v.get("VCHTYPE") or _text(v, "VOUCHERTYPENAME")) != "Payment":
            continue
        party = _text(v, "PARTYLEDGERNAME")
        amt = None
        for le in v.iter("ALLLEDGERENTRIES.LIST"):
            if _text(le, "LEDGERNAME") == party and _text(le, "AMOUNT"):
                amt = abs(decimal_str_to_minor(_text(le, "AMOUNT")))
        out.append({"voucher_number": _text(v, "VOUCHERNUMBER"), "date": _text(v, "DATE"), "party": party, "amount_minor": amt})
    return out
