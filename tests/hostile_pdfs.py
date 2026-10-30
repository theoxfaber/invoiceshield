"""Builders for malformed / hostile PDFs used by the sandbox tests (generated at test time)."""
from __future__ import annotations

import zlib


def _pdf(objects: list[bytes]) -> bytes:
    """Assemble numbered objects into a PDF with a correct xref table."""
    out = bytearray(b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n")
    offsets = []
    for i, body in enumerate(objects, start=1):
        offsets.append(len(out))
        out += f"{i} 0 obj\n".encode() + body + b"\nendobj\n"
    xref = len(out)
    out += f"xref\n0 {len(objects) + 1}\n0000000000 65535 f \n".encode()
    for off in offsets:
        out += f"{off:010d} 00000 n \n".encode()
    out += f"trailer\n<< /Size {len(objects) + 1} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n".encode()
    return bytes(out)


def _stream(data: bytes, flate: bool = False) -> bytes:
    if flate:
        data = zlib.compress(data, 9)
        return f"<< /Length {len(data)} /Filter /FlateDecode >>\nstream\n".encode() + data + b"\nendstream"
    return f"<< /Length {len(data)} >>\nstream\n".encode() + data + b"\nendstream"


FONT = b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>"


def simple(text: str = "Hello") -> bytes:
    content = f"BT /F1 12 Tf 72 720 Td ({text}) Tj ET".encode()
    return _pdf([
        b"<< /Type /Catalog /Pages 2 0 R >>",
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>",
        _stream(content), FONT,
    ])


def declared_huge_page_count(count: int = 10_000_000) -> bytes:
    """Page tree that claims millions of pages (one real kid)."""
    content = b"BT /F1 12 Tf 72 720 Td (x) Tj ET"
    return _pdf([
        b"<< /Type /Catalog /Pages 2 0 R >>",
        f"<< /Type /Pages /Kids [3 0 R] /Count {count} >>".encode(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>",
        _stream(content), FONT,
    ])


def many_real_pages(n: int = 400) -> bytes:
    """A page tree whose kids really are n page references (the same page object, repeated)."""
    kids = " ".join(["3 0 R"] * n)
    return _pdf([
        b"<< /Type /Catalog /Pages 2 0 R >>",
        f"<< /Type /Pages /Kids [{kids}] /Count 1 >>".encode(),  # lies low: only the walk finds n
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>",
        _stream(b"BT /F1 12 Tf 72 720 Td (x) Tj ET"), FONT,
    ])


def flate_bomb(mb: int = 400) -> bytes:
    """A content stream that inflates from ~0.4 MB to `mb` MB of text operators."""
    chunk = b"BT /F1 12 Tf 72 720 Td (AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA) Tj ET\n"
    raw = chunk * ((mb * 1024 * 1024) // len(chunk))
    return _pdf([
        b"<< /Type /Catalog /Pages 2 0 R >>",
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>",
        _stream(raw, flate=True), FONT,
    ])


def recursive_pages() -> bytes:
    """A page tree that contains itself."""
    return _pdf([
        b"<< /Type /Catalog /Pages 2 0 R >>",
        b"<< /Type /Pages /Kids [2 0 R 3 0 R] /Count 2 >>",
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>",
    ])
