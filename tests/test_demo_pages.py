"""The committed replay page ui/demo-v3.html (the one the submission video records) must be
regenerable from repo sources: ui/demo.html (built by ui/build_demo.py) ->
ui/build/build_demo_v2.py (intermediate, not committed) -> ui/build/build_v3.py.

Byte-identical with the poppler version the committed page was built with (pdftoppm 25.03, recorded
below). Other poppler versions rasterise the invoice thumbnails differently, so there the embedded
base64 blobs (images, fonts) are masked and box coordinates compared to 2 decimals; all code, text and
replay data must still match exactly."""
import pathlib
import re
import shutil
import subprocess
import sys

import pytest

ROOT = pathlib.Path(__file__).resolve().parents[1]
BUILT_WITH_POPPLER = "25.03"


def poppler_version() -> str:
    out = subprocess.run(["pdftoppm", "-v"], capture_output=True, text=True)
    m = re.search(r"(\d+\.\d+)", out.stderr + out.stdout)
    return m.group(1) if m else "?"


def normalize(b: bytes) -> str:
    s = b.decode("utf-8")
    s = re.sub(r"[A-Za-z0-9+/]{200,}={0,2}", "<b64>", s)
    return re.sub(r"\d+\.\d{3,}", lambda m: f"{float(m.group(0)):.2f}", s)


@pytest.mark.skipif(not (shutil.which("pdftoppm") and shutil.which("pdftotext")), reason="needs poppler-utils")
def test_demo_v3_regenerates_from_ui_build():
    page = ROOT / "ui" / "demo-v3.html"
    committed = page.read_bytes()
    try:
        for mod in ("ui.build.build_demo_v2", "ui.build.build_v3"):
            subprocess.run([sys.executable, "-m", mod], cwd=ROOT, check=True, capture_output=True)
        rebuilt = page.read_bytes()
        if poppler_version() == BUILT_WITH_POPPLER:
            assert rebuilt == committed, "ui/demo-v3.html no longer regenerates byte-for-byte from ui/build/"
        else:
            assert normalize(rebuilt) == normalize(committed), (
                f"ui/demo-v3.html differs beyond rasterisation (poppler {poppler_version()} vs {BUILT_WITH_POPPLER})")
    finally:
        page.write_bytes(committed)
