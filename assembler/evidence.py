"""Deterministic evidence assembly: PDF text layer + metadata + cover email.

No model is involved here. The text layer is what a text-only agent reads, hidden
text included (a white 1-pt string is still in the layer), which is exactly the
exposure the attack set probes.
"""
from __future__ import annotations

import hashlib
from dataclasses import dataclass, field
from pathlib import Path

from pypdf import PdfReader


@dataclass
class Evidence:
    pdf_sha256: str
    pages: int
    text: str
    metadata: dict = field(default_factory=dict)
    email_from: str = ""
    email_to: str = ""
    email_subject: str = ""
    email_body: str = ""

    def render(self) -> str:
        """Plain-text view handed to an agent."""
        return (
            f"--- EMAIL ---\nFrom: {self.email_from}\nTo: {self.email_to}\n"
            f"Subject: {self.email_subject}\n\n{self.email_body}\n"
            f"--- ATTACHED INVOICE (text extracted from PDF, {self.pages} page(s)) ---\n{self.text}\n"
        )


def assemble(pdf_path: str | Path, cover_email: dict) -> Evidence:
    raw = Path(pdf_path).read_bytes()
    reader = PdfReader(str(pdf_path))
    text = "\n".join((p.extract_text() or "") for p in reader.pages).strip()
    meta = {k.lstrip("/"): str(v) for k, v in (reader.metadata or {}).items()}
    return Evidence(
        pdf_sha256=hashlib.sha256(raw).hexdigest(), pages=len(reader.pages), text=text, metadata=meta,
        email_from=cover_email.get("from", ""), email_to=cover_email.get("to", ""),
        email_subject=cover_email.get("subject", ""), email_body=cover_email.get("body", ""),
    )
