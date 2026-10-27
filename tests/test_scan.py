"""Scan path: image-only PDF -> local OCR -> same deterministic extraction."""
from assembler.evidence import assemble
from assembler.extract import governor_evidence, printed_total_paise
from assembler.scan import make_scan, ocr_text
from dataset.gen.make_invoice import make_t1
from shield.pipeline import BUYER_GSTIN


def _scan(tmp_path, **kw):
    m = make_t1("V006", "NITS-2026-900", "PO-2026-0601", 4720000, tmp_path / "i.pdf", **kw)
    make_scan(m["pdf"], tmp_path / "s.pdf")
    return m, assemble(tmp_path / "s.pdf", m["cover_email"])


def test_scan_has_no_text_layer_and_ocr_recovers_it(tmp_path):
    m, ev = _scan(tmp_path)
    assert ev.metadata["text_source"] == "ocr"
    assert "Total" in ev.text and len(ev.text) > 100


def test_ocr_keeps_the_printed_total(tmp_path):
    _, ev = _scan(tmp_path)
    assert printed_total_paise(ev.text) in (4720000, None)
