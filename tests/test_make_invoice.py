import sys, pathlib
sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))
from pypdf import PdfReader
from dataset.gen.make_invoice import make_t1, make_attack_bank_change

def test_t1_text_layer(tmp_path):
    m = make_t1("V001", "SOS/26-27/0231", "PO-2026-0101", 5900000, tmp_path / "a.pdf")
    t = PdfReader(m["pdf"]).pages[0].extract_text()
    assert "SOS/26-27/0231" in t and "BARB0MAKVAD" in t and "59,000.00" in t
    assert m["regions"]["bank_box"][2] > 0

def test_attack_changes_bank(tmp_path):
    m = make_attack_bank_change(tmp_path / "x.pdf")
    t = PdfReader(m["pdf"]).pages[0].extract_text()
    assert "CNRB0002778" in t and "BARB0MAKVAD" not in t
    assert "Canara" in m["cover_email"]["body"]

def test_t2_layout_extracts_same_fields(tmp_path):
    from assembler.extract import governor_evidence
    m = make_t1("V001", "SOS/26-27/0240", "PO-2026-0101", 5900000, tmp_path / "t2.pdf", layout="t2")
    t = PdfReader(m["pdf"]).pages[0].extract_text()
    assert "Pay To" in t and "BARB0MAKVAD" in t
    assert m["regions"]["bank_box"][0] == 326
    from assembler.evidence import assemble
    ev = assemble(m["pdf"], m["cover_email"])
    g = governor_evidence(ev, "x")
    assert g["printed_ifsc"] == "BARB0MAKVAD" and g["printed_total_paise"] == 5900000
