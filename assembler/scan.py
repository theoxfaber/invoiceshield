"""Scan/photo path: rasterise a PDF, drop its text layer, OCR it locally.

make_scan() builds an image-only PDF from a generated invoice (a stand-in for a
scanned or photographed invoice). ocr_text() runs pdftoppm + tesseract. If OCR
output is too thin to trust, the caller must fall back to REVIEW (no text, no GSTIN).
"""
from __future__ import annotations

import subprocess
import tempfile
from pathlib import Path

from PIL import Image, ImageFilter


def _render(pdf: Path, out_dir: Path, dpi: int) -> list[Path]:
    subprocess.run(["pdftoppm", "-r", str(dpi), "-png", str(pdf), str(out_dir / "p")], check=True)
    return sorted(out_dir.glob("p*.png"))


def make_scan(src_pdf: str | Path, dst_pdf: str | Path, dpi: int = 200, blur: float = 0.6) -> Path:
    with tempfile.TemporaryDirectory() as d:
        pages = [Image.open(p).convert("L").filter(ImageFilter.GaussianBlur(blur)) for p in _render(Path(src_pdf), Path(d), dpi)]
        pages[0].save(str(dst_pdf), "PDF", resolution=dpi, save_all=True, append_images=pages[1:])
    return Path(dst_pdf)


def ocr_text(pdf: str | Path, dpi: int = 250) -> str:
    out = []
    with tempfile.TemporaryDirectory() as d:
        for p in _render(Path(pdf), Path(d), dpi):
            r = subprocess.run(["tesseract", str(p), "-", "--psm", "6"], capture_output=True, text=True)
            out.append(r.stdout)
    return "\n".join(out).strip()


def too_thin(text: str, min_chars: int = 80) -> bool:
    return len(text.strip()) < min_chars


def make_photo(src_pdf: str | Path, dst_pdf: str | Path, angle: float = 2.0, noise: float = 12.0,
               dpi: int = 200, seed: int = 7) -> Path:
    """Phone-photo stand-in: small rotation, uneven lighting, gaussian noise, blur."""
    import random
    rnd = random.Random(seed)
    with tempfile.TemporaryDirectory() as d:
        out = []
        for p in _render(Path(src_pdf), Path(d), dpi):
            im = Image.open(p).convert("L").rotate(angle, expand=True, fillcolor=255, resample=Image.BICUBIC)
            w, h = im.size
            px = im.load()
            for y in range(0, h):
                shade = int(25 * y / h)
                for x in range(w):
                    v = px[x, y] - shade + int(rnd.gauss(0, noise))
                    px[x, y] = max(0, min(255, v))
            out.append(im.filter(ImageFilter.GaussianBlur(0.8)))
        out[0].save(str(dst_pdf), "PDF", resolution=dpi, save_all=True, append_images=out[1:])
    return Path(dst_pdf)
