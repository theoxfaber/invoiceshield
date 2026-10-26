"""T1 invoice generator (reportlab). Fictional data only. Returns PDF path + region boxes + cover email."""
import json, pathlib
from reportlab.lib.pagesizes import A4
from reportlab.pdfgen import canvas

FIX = pathlib.Path(__file__).resolve().parents[1] / "fixtures"
MASTER = json.load(open(FIX / "master.json"))
PROF = json.load(open(FIX / "profiles.json"))


def _vendor(vid):
    return next(v for v in MASTER["vendors"] if v["vendor_id"] == vid)


def make_t1(vid, invoice_no, po_ref, amount_paise, out, bank=None, note=None, date="27/10/2026"):
    """bank: override dict {bank_name, ifsc, bank_account} (attack). Returns metadata dict."""
    v, p, b = _vendor(vid), PROF["vendors"][vid], PROF["buyer"]
    bank = bank or {k: v[k] for k in ("bank_name", "ifsc", "bank_account")}
    base = amount_paise / 100 / 1.18
    gst = amount_paise / 100 - base
    W, H = A4
    c = canvas.Canvas(str(out), pagesize=A4)
    regions = {}

    def line(y, text, size=10, x=40, bold=False):
        c.setFont("Helvetica-Bold" if bold else "Helvetica", size)
        c.drawString(x, y, text)

    line(H - 50, v["legal_name"], 16, bold=True)
    line(H - 66, p["address"], 9)
    line(H - 78, f"GSTIN: {v['gstin']}   Ph: {p['phone']}", 9)
    line(H - 110, "TAX INVOICE", 13, bold=True)
    line(H - 130, f"Invoice No: {invoice_no}      Date: {date}")
    line(H - 144, f"PO Ref: {po_ref}")
    line(H - 170, "Bill To:", bold=True)
    line(H - 184, b["legal_name"])
    line(H - 196, b["address"], 9)
    line(H - 208, f"GSTIN: {b['gstin']}", 9)
    line(H - 250, "Description", bold=True); line(H - 250, "Amount (INR)", x=440, bold=True)
    line(H - 268, f"{v['category']} as per {po_ref}"); line(H - 268, f"{base:,.2f}", x=440)
    line(H - 284, "IGST/CGST+SGST @ 18%"); line(H - 284, f"{gst:,.2f}", x=440)
    line(H - 304, "Total Payable", bold=True); line(H - 304, f"{amount_paise/100:,.2f}", x=440, bold=True)
    y0 = H - 360
    line(y0, "Bank details for remittance", bold=True)
    for i, (k, lab) in enumerate((("bank_name", "Bank"), ("bank_account", "A/c No"), ("ifsc", "IFSC"))):
        line(y0 - 16 * (i + 1), f"{lab}: {bank[k]}")
    # region box in PDF points, origin top-left
    regions["bank_box"] = [36, H - (y0 + 14), 330, 16 * 4 + 6]
    if note:
        line(y0 - 80, note, 9)
    line(60, "This is a computer generated invoice.", 8)
    c.save()
    cover = {
        "from": f"accounts@{v['email_domains'][0]}", "to": b["ap_email"],
        "subject": f"Invoice {invoice_no} against {po_ref}",
        "body": f"Dear Accounts Team,\n\nPlease find attached invoice {invoice_no} against {po_ref}. "
                f"{note or 'Kindly process payment as per terms.'}\n\nRegards,\n{v['legal_name']}",
    }
    return {"pdf": str(out), "vendor_id": vid, "invoice_no": invoice_no, "po_ref": po_ref,
            "amount_paise": amount_paise, "regions": regions, "cover_email": cover}


def make_attack_bank_change(out):
    a = PROF["attackers"]["X_bank_change"]
    return make_t1("V001", "SOS/26-27/0232", "PO-2026-0101", 5900000, out,
                   bank={k: a[k] for k in ("bank_name", "ifsc", "bank_account")}, note=a["story"])
