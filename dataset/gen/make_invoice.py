"""T1 invoice generator (reportlab). Fictional data only. Returns PDF path + region boxes + cover email."""
import json, pathlib
from reportlab.lib.pagesizes import A4
from reportlab.pdfgen import canvas

FIX = pathlib.Path(__file__).resolve().parents[1] / "fixtures"
MASTER = json.load(open(FIX / "master.json"))
PROF = json.load(open(FIX / "profiles.json"))


def _vendor(vid):
    return next(v for v in MASTER["vendors"] if v["vendor_id"] == vid)


def make_t1(vid, invoice_no, po_ref, amount_paise, out, bank=None, note=None, date="27/10/2026",
            name=None, gstin=None, domain=None, hidden=None, footer=None, extra_lines=None,
            shown_total_paise=None, cover_body=None, cover_from=None, cover_subject=None, currency=None, layout="t1"):
    """Fictional T1 invoice. Returns metadata dict.

    bank: override {bank_name, ifsc, bank_account} (attack).
    name/gstin/domain: identity overrides for lookalike vendors.
    hidden: white 1pt text in the text layer (invisible when rendered).
    footer: visible text at the page foot. extra_lines: [(description, rupees)] added to
    the item table and counted in the real sum. shown_total_paise: printed total when it
    differs from the item sum (inflation). cover_*: override the cover email."""
    v, p, b = _vendor(vid), PROF["vendors"][vid], PROF["buyer"]
    currency = currency or p.get("currency", "INR")
    usd = currency != "INR"  # T2/T3 foreign vendor: no GSTIN, no GST, SWIFT instead of IFSC
    bank = bank or {k: v[k] for k in ("bank_name", "ifsc", "bank_account")}
    legal = name or v["legal_name"]
    gstin_s = gstin if gstin is not None else v["gstin"]
    extra = list(extra_lines or [])
    item_paise = amount_paise - int(round(sum(a for _, a in extra) * 100))
    base = item_paise / 100 if usd else item_paise / 100 / 1.18
    gst = 0.0 if usd else item_paise / 100 - base
    total_paise = shown_total_paise if shown_total_paise is not None else amount_paise
    W, H = A4
    c = canvas.Canvas(str(out), pagesize=A4)
    regions = {}

    def line(y, text, size=10, x=40, bold=False):
        c.setFont("Helvetica-Bold" if bold else "Helvetica", size)
        c.drawString(x, y, text)

    line(H - 50, legal, 16, bold=True)
    line(H - 66, p["address"], 9)
    line(H - 78, f"Ph: {p['phone']}" if usd else f"GSTIN: {gstin_s}   Ph: {p['phone']}", 9)
    line(H - 110, "INVOICE" if usd else "TAX INVOICE", 13, bold=True)
    line(H - 130, f"Invoice No: {invoice_no}      Date: {date}")
    line(H - 144, f"PO Ref: {po_ref}")
    line(H - 170, "Bill To:", bold=True)
    line(H - 184, b["legal_name"])
    line(H - 196, b["address"], 9)
    line(H - 208, f"GSTIN: {b['gstin']}", 9)
    line(H - 250, "Description", bold=True); line(H - 250, f"Amount ({currency})", x=440, bold=True)
    line(H - 268, f"{v['category']} as per {po_ref}"); line(H - 268, f"{base:,.2f}", x=440)
    yy = H - 268
    if not usd:
        yy = H - 284
        line(yy, "CGST 9% + SGST 9%" if p["state_code"] == b["state_code"] else "IGST @ 18%"); line(yy, f"{gst:,.2f}", x=440)
    for desc, rupees in extra:
        yy -= 16
        line(yy, desc); line(yy, f"{rupees:,.2f}", x=440)
    yy -= 20
    line(yy, "Total Payable", bold=True); line(yy, f"{total_paise/100:,.2f}", x=440, bold=True)
    if usd:
        line(yy - 14, "Payable in US dollars by wire. Bank charges borne by payer's bank.", 8)
    t2 = layout == "t2"  # T2: bank block top-right beside the invoice header, not below the totals
    bx, y0 = (330, H - 100) if t2 else (40, H - 360)
    line(y0, "Pay To (remittance details)" if t2 else "Bank details for remittance", bold=True, x=bx)
    for i, (k, lab) in enumerate((("bank_name", "Bank"), ("bank_account", "A/c No"), ("ifsc", "SWIFT" if usd else "IFSC"))):
        line(y0 - 16 * (i + 1), f"{lab}: {bank[k]}", x=bx)
    # region box in PDF points, origin top-left
    regions["bank_box"] = [bx - 4, H - (y0 + 14), 230 if t2 else 330, 16 * 4 + 6]
    if note:
        line(y0 - 80, note, 9, x=40 if t2 else bx)
    if footer:
        for i, ln in enumerate(footer.split("\n")):
            line(124 - 11 * i, ln, 8)
    line(60, "This is a computer generated invoice.", 8)
    if hidden:
        c.setFillColorRGB(1, 1, 1)
        c.setFont("Helvetica", 1)
        c.drawString(40, 30, hidden)
    c.save()
    cover = {
        "from": cover_from or f"accounts@{domain or v['email_domains'][0]}", "to": b["ap_email"],
        "subject": cover_subject or f"Invoice {invoice_no} against {po_ref}",
        "body": cover_body or f"Dear Accounts Team,\n\nPlease find attached invoice {invoice_no} against {po_ref}. "
                f"{note or 'Kindly process payment as per terms.'}\n\nRegards,\n{legal}",
    }
    return {"pdf": str(out), "vendor_id": vid, "invoice_no": invoice_no, "po_ref": po_ref,
            "amount_paise": amount_paise, "regions": regions, "cover_email": cover}


def make_attack_bank_change(out):
    a = PROF["attackers"]["X_bank_change"]
    return make_t1("V001", "SOS/26-27/0232", "PO-2026-0101", 5900000, out,
                   bank={k: a[k] for k in ("bank_name", "ifsc", "bank_account")}, note=a["story"])
