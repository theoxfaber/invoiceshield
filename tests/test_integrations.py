"""Phase 7 adapters: Tally XML, Zoho Books JSON, governor ledger source, payment-rail sandbox.

MOCK-ONLY. Fixtures in tests/fixtures/integrations/ are synthetic; Zoho calls go through
httpx.MockTransport. Nothing here was run against a live Tally, Zoho Books or bank.
"""
import json
import pathlib
import sqlite3
import xml.etree.ElementTree as ET

import httpx
import pytest

import integrations
from integrations import tally, zoho_books
from integrations.common import ApprovedPayment, decimal_str_to_minor, minor_to_decimal_str
from integrations.ledger_source import approved_payments
from integrations.payment_rail import PaymentInstruction, PaymentRail, RailRefused, SandboxRail

ROOT = pathlib.Path(__file__).resolve().parents[1]
FIX = ROOT / "tests" / "fixtures" / "integrations"
MASTER = json.loads((ROOT / "dataset" / "fixtures" / "master.json").read_text())
PAYMENTS = [ApprovedPayment.from_dict(d) for d in json.loads((FIX / "approved_payments.json").read_text())]


def test_labelled_not_live_tested():
    assert integrations.LIVE_TESTED is False
    for m in (tally, zoho_books):
        assert "NOT TESTED AGAINST A LIVE" in m.__doc__


@pytest.mark.parametrize("minor,s", [(0, "0.00"), (5, "0.05"), (100, "1.00"), (1234550, "12345.50"), (-250, "-2.50"), (10**15 + 1, "10000000000000.01")])
def test_money_roundtrip_is_exact(minor, s):
    assert minor_to_decimal_str(minor) == s
    assert decimal_str_to_minor(s) == minor


@pytest.mark.parametrize("bad", ["1.234", "abc", "", "1e5", "--1", "+-1"])
def test_money_parse_rejects(bad):
    with pytest.raises(ValueError):
        decimal_str_to_minor(bad)


def test_money_refuses_float():
    with pytest.raises(TypeError):
        minor_to_decimal_str(12.5)


# ---------------- Tally ----------------

def test_tally_vendor_master_export_shape():
    root = ET.fromstring(tally.export_vendor_masters(MASTER, company="Demo Co"))
    assert root.tag == "ENVELOPE" and root.findtext("HEADER/TALLYREQUEST") == "Import Data"
    assert root.findtext("BODY/IMPORTDATA/REQUESTDESC/REPORTNAME") == "All Masters"
    assert root.findtext("BODY/IMPORTDATA/REQUESTDESC/STATICVARIABLES/SVCURRENTCOMPANY") == "Demo Co"
    leds = root.findall(".//TALLYMESSAGE/LEDGER")
    assert len(leds) == len(MASTER["vendors"])
    v1 = next(l for l in leds if l.get("NAME") == "Sharma Office Supplies Pvt Ltd")
    assert v1.get("ACTION") == "Create" and v1.findtext("PARENT") == "Sundry Creditors"
    assert [n.text for n in v1.findall("NAME.LIST/NAME")] == ["Sharma Office Supplies Pvt Ltd", "V001"]
    assert v1.findtext("PARTYGSTIN") == "24ZZACS4821A1ZX" and v1.findtext("LEDGERSTATENAME") == "Gujarat"
    assert v1.findtext("PAYMENTDETAILS.LIST/ACCOUNTNUMBER") == "23276810695553"
    assert v1.findtext("PAYMENTDETAILS.LIST/IFSCODE") == "BARB0MAKVAD"
    foreign = next(l for l in leds if "V003" in [n.text for n in l.findall("NAME.LIST/NAME")])
    assert foreign.findtext("PARTYGSTIN") is None and foreign.findtext("GSTREGISTRATIONTYPE") == "Unregistered/Consumer"


def test_tally_masters_roundtrip_is_unchanged():
    d = tally.import_vendor_masters(tally.export_vendor_masters(MASTER), MASTER)
    assert sorted(d.unchanged) == sorted(v["vendor_id"] for v in MASTER["vendors"])
    assert not (d.bank_change_requests or d.new_vendors or d.field_mismatches or d.errors)


def test_tally_payment_vouchers():
    xml, skipped = tally.export_payment_vouchers(PAYMENTS, MASTER, bank_ledger="HDFC Current A/c")
    assert skipped == [{"decision_id": "D000003", "reason": "currency USD: forex vouchers need a rate; enter manually"}]
    vs = ET.fromstring(xml).findall(".//VOUCHER")
    assert [v.findtext("VOUCHERNUMBER") for v in vs] == ["D000001", "D000002"]
    v = vs[0]
    assert v.get("VCHTYPE") == "Payment" and v.findtext("DATE") == "20261030"
    assert v.findtext("PARTYLEDGERNAME") == "Sharma Office Supplies Pvt Ltd"
    dr, cr = v.findall("ALLLEDGERENTRIES.LIST")
    assert (dr.findtext("ISDEEMEDPOSITIVE"), dr.findtext("AMOUNT")) == ("Yes", "-12345.50")
    assert (dr.findtext("BILLALLOCATIONS.LIST/NAME"), dr.findtext("BILLALLOCATIONS.LIST/BILLTYPE")) == ("INV-2026-0042", "Agst Ref")
    assert (cr.findtext("LEDGERNAME"), cr.findtext("ISDEEMEDPOSITIVE"), cr.findtext("AMOUNT")) == ("HDFC Current A/c", "No", "12345.50")
    assert "approved by ravi" in vs[1].findtext("NARRATION")
    assert vs[1].findtext("DATE") == "20261031"  # IST calendar day
    # and the reader sees the same numbers
    assert [(r["voucher_number"], r["amount_minor"]) for r in tally.vouchers_from_xml(xml)] == [("D000001", 1234550), ("D000002", 8000000)]


def test_tally_import_never_applies_bank_changes():
    xml = (FIX / "tally_masters_export.xml").read_text()
    before = json.dumps(MASTER, sort_keys=True)
    d = tally.import_vendor_masters(xml, MASTER, vendor_map={"Visakha Steel Fabricators Pvt Ltd": "V004"})
    assert json.dumps(MASTER, sort_keys=True) == before, "the master is never modified"
    out = d.as_dict()
    assert out["applied"] is False
    assert out["unchanged"] == ["V002"]
    [bc] = out["bank_change_requests"]
    assert bc["vendor_id"] == "V001" and (bc["new_account"], bc["new_ifsc"]) == ("50100999988887", "HDFC0009999")
    assert bc["new_beneficiary_id"] is None, "a beneficiary must be registered before the change can be filed"
    assert set(bc) - {"_source", "_note"} == {"vendor_id", "new_account", "new_ifsc", "new_bank_name", "new_beneficiary_id"}, "same fields as POST /v1/master-changes"
    assert out["field_mismatches"] == [{"vendor_id": "V004", "field": "gstin", "master": "37ZZACV6143D1ZE", "tally": "37ZZACV6143D1ZF"}]
    assert [n["vendor_id"] for n in out["new_vendors"]] == ["V901"]
    assert "Electricity Expenses" not in json.dumps(out), "non-vendor ledgers are ignored"


def test_tally_import_unmapped_ledger_is_an_error_not_a_guess():
    d = tally.import_vendor_masters((FIX / "tally_masters_export.xml").read_text(), MASTER)
    assert any("Visakha Steel" in e for e in d.errors)


@pytest.mark.parametrize("evil", [
    '<?xml version="1.0"?><!DOCTYPE x [<!ENTITY a "aaaa"><!ENTITY b "&a;&a;&a;">]><ENVELOPE>&b;</ENVELOPE>',
    '<!DOCTYPE x SYSTEM "file:///etc/passwd"><ENVELOPE/>',
])
def test_tally_parser_refuses_dtds(evil):
    with pytest.raises(ValueError):
        tally.ledgers_from_xml(evil)


# ---------------- Zoho Books ----------------

def test_zoho_contact_payloads():
    by = {p["custom_fields"][0]["value"]: p for p in zoho_books.export_vendor_contacts(MASTER)}
    v1 = by["V001"]
    assert v1 == {"contact_name": "Sharma Office Supplies Pvt Ltd", "company_name": "Sharma Office Supplies Pvt Ltd", "contact_type": "vendor",
                  "gst_treatment": "business_gst", "gst_no": "24ZZACS4821A1ZX", "place_of_contact": "GJ",
                  "custom_fields": [{"label": "invoiceshield_vendor_id", "value": "V001"}]}
    assert by["V003"]["gst_treatment"] == "overseas" and "gst_no" not in by["V003"]
    assert all("bank" not in json.dumps(p).lower() for p in by.values()), "bank details never go to the books"


def test_zoho_vendor_payment_payloads():
    vmap = {"V001": "460000000026049", "V002": "460000000026050"}
    bodies, skipped = zoho_books.export_vendor_payments(PAYMENTS, vmap, "460000000000358", bill_map={"INV-2026-0042": "460000000040001"})
    assert skipped == [{"decision_id": "D000003", "reason": "no Zoho contact mapped for V003"}]
    b = bodies[0]
    assert b["vendor_id"] == "460000000026049" and b["amount"] == 12345.5 and json.dumps(b["amount"]) == "12345.5"
    assert (b["date"], b["reference_number"], b["paid_through_account_id"]) == ("2026-10-30", "D000001", "460000000000358")
    assert b["bills"] == [{"bill_id": "460000000040001", "amount_applied": 12345.5}]
    assert "bills" not in bodies[1] and "approved by ravi" in bodies[1]["description"]


def _mock(handler_log):
    pages = {1: json.loads((FIX / "zoho_contacts_page1.json").read_text()), 2: json.loads((FIX / "zoho_contacts_page2.json").read_text())}

    def handler(req: httpx.Request) -> httpx.Response:
        handler_log.append(req)
        assert req.headers["authorization"] == "Zoho-oauthtoken test-token"
        assert req.url.params["organization_id"] == "10234695"
        if req.method == "GET" and req.url.path.endswith("/contacts"):
            assert req.url.params["contact_type"] == "vendor"
            return httpx.Response(200, json=pages[int(req.url.params["page"])])
        if req.method == "GET" and req.url.path.endswith("/vendorpayments"):
            return httpx.Response(200, json=json.loads((FIX / "zoho_vendorpayments.json").read_text()))
        if req.method == "POST" and req.url.path.endswith("/vendorpayments"):
            body = json.loads(req.content)
            if body.get("amount", 0) <= 0:
                return httpx.Response(400, json={"code": 1002, "message": "Invalid value passed for amount"})
            return httpx.Response(201, json={"code": 0, "message": "The payment made to the vendor has been recorded.", "vendorpayment": {"payment_id": "460000000031099", **body}})
        return httpx.Response(404, json={"code": 5, "message": "not found"})
    return httpx.MockTransport(handler)


def test_zoho_client_refuses_live_by_default():
    with pytest.raises(zoho_books.ZohoError, match="not tested against a live"):
        zoho_books.ZohoBooksClient("1", "t")


def test_zoho_client_mock_flow():
    log = []
    c = zoho_books.ZohoBooksClient("10234695", "test-token", transport=_mock(log))
    contacts = c.list_vendor_contacts()
    assert len(contacts) == 4 and [r.url.params["page"] for r in log] == ["1", "2"], "follows page_context.has_more_page"
    d = zoho_books.import_vendor_contacts(contacts, MASTER).as_dict()
    assert d["unchanged"] == ["V001"]
    assert d["field_mismatches"] == [{"vendor_id": "V002", "field": "legal_name", "master": "Coastal Logistics", "zoho_books": "Coastal Logistics Pvt"}]
    assert d["errors"] and "Unmapped Vendor" in d["errors"][0]
    assert d["bank_change_requests"] == [] and d["applied"] is False

    body = zoho_books.vendor_payment_payload(PAYMENTS[0], "460000000026049", "460000000000358")
    created = c.create_vendor_payment(body)
    assert created["payment_id"] == "460000000031099" and created["reference_number"] == "D000001"
    with pytest.raises(zoho_books.ZohoError, match="1002"):
        c.create_vendor_payment({**body, "amount": 0})

    rec = zoho_books.reconcile_payments(c.list_vendor_payments(), PAYMENTS)
    assert rec["matched"] == ["D000001"]
    assert rec["amount_mismatch"] == [{"payment_id": "460000000031002", "decision_id": "D000002", "zoho": 80001.0, "governor_minor": 8000000}]
    assert [u["reference_number"] for u in rec["not_approved_by_gate"]] == ["manual-entry-77"]
    assert rec["approved_not_in_books"] == ["D000003"]
    c.close()


# ---------------- governor ledger source ----------------

def _governor_like_db(path):
    con = sqlite3.connect(path)
    con.executescript("""
    CREATE TABLE decisions (decision_id TEXT PRIMARY KEY, verdict TEXT, body TEXT, ts INTEGER, proposed_by TEXT);
    CREATE TABLE ledger (id INTEGER PRIMARY KEY AUTOINCREMENT, decision_id TEXT UNIQUE, vendor_id TEXT, invoice_key TEXT, amount_paise INTEGER,
                         currency TEXT, irn TEXT, po_ref TEXT, status TEXT, ts INTEGER, entry TEXT);
    CREATE TABLE review_resolutions (decision_id TEXT PRIMARY KEY, approver TEXT, approve INTEGER, ts INTEGER);
    """)
    rows = [("D1", "allow", "paid", 100, None), ("D2", "review", "paid", 200, ("ravi", 1)), ("D3", "review", "pending", 300, None),
            ("D4", "review", "rejected", 400, ("ravi", 0))]
    for did, verdict, status, ts, res in rows:
        con.execute("INSERT INTO decisions VALUES (?,?,?,?,?)", (did, verdict, json.dumps({"decision_id": did, "verdict": verdict, "payee": {"beneficiary_id": "BENE_V001", "bank_name": "x"}}), ts, "ap-agent"))
        con.execute("INSERT INTO ledger (decision_id, vendor_id, invoice_key, amount_paise, currency, po_ref, status, ts, entry) VALUES (?,?,?,?,?,?,?,?,?)",
                    (did, "V001", f"INV-{did}", 1000 * ts, "INR", "PO-2026-0101", status, ts, "{}"))
        if res:
            con.execute("INSERT INTO review_resolutions VALUES (?,?,?,?)", (did, res[0], res[1], ts + 1))
    con.commit()
    con.close()


def test_ledger_source_reads_only_paid_and_is_read_only(tmp_path):
    db = tmp_path / "governor.db"
    _governor_like_db(db)
    got = approved_payments(db)
    assert [(p.decision_id, p.approver, p.beneficiary_id) for p in got] == [("D1", None, "BENE_V001"), ("D2", "ravi", "BENE_V001")]
    assert [p.decision_id for p in approved_payments(db, since=150)] == ["D2"]
    # the exporters accept exactly these records
    xml, skipped = tally.export_payment_vouchers(got, MASTER, "Bank")
    assert not skipped and xml.count("<VOUCHER ") == 2
    # mode=ro: a connection opened the way the module opens it cannot write
    from integrations import ledger_source
    con = sqlite3.connect(db.resolve().as_uri() + "?mode=ro", uri=True)
    with pytest.raises(sqlite3.OperationalError):
        con.execute("DELETE FROM ledger")
    con.close()
    assert "mode=ro" in pathlib.Path(ledger_source.__file__).read_text()


# ---------------- payment rail ----------------

ALLOW = {"decision_id": "D000001", "verdict": "allow", "payee": {"beneficiary_id": "BENE_V001", "bank_name": "Bank of Baroda"}}


def test_rail_interface_and_sandbox_only():
    rail = SandboxRail()
    assert isinstance(rail, PaymentRail) and rail.live is False
    import integrations.payment_rail as pr
    src = pathlib.Path(pr.__file__).read_text()
    assert "import httpx" not in src and "import requests" not in src and "import socket" not in src, "the stub has no network code"


@pytest.mark.parametrize("decision,kw", [
    ({**ALLOW, "verdict": "block"}, {}),
    ({**ALLOW, "verdict": "review"}, {}),
    ({**ALLOW, "payee": None}, {}),
    (ALLOW, {"amount_minor": 0}),
    (ALLOW, {"amount_minor": 12.5}),
])
def test_instruction_only_from_payable_decisions(decision, kw):
    with pytest.raises(RailRefused):
        PaymentInstruction.from_decision(decision, **{"amount_minor": 100, **kw})


def test_sandbox_rail_idempotent_and_beneficiary_bound():
    rail = SandboxRail()
    ins = PaymentInstruction.from_decision(ALLOW, 1234550)
    assert not hasattr(ins, "account_number") and ins.beneficiary_id == "BENE_V001"
    with pytest.raises(RailRefused, match="not registered"):
        rail.submit(ins)
    rail.register_beneficiary("BENE_V001")
    r1 = rail.submit(ins)
    r2 = rail.submit(ins)
    assert r1.rail_ref == r2.rail_ref and r1.status == "accepted" and r2.replayed and not r1.replayed
    with pytest.raises(RailRefused, match="reused"):
        rail.submit(PaymentInstruction(**{**ins.__dict__, "amount_minor": 999}))
    rail.settle(r1.rail_ref)
    assert rail.status(r1.rail_ref).status == "settled"
    approved_review = PaymentInstruction.from_decision({**ALLOW, "decision_id": "D2", "verdict": "review"}, 500, resolved_approved=True)
    assert rail.submit(approved_review).rail_ref != r1.rail_ref
