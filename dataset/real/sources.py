"""Public third-party invoice data used by eval/run_real.py.

Raw files are downloaded into data/third_party/raw/ (gitignored) and verified by sha256.
Only dataset/real/manifest.json (source, licence, sha256, counts) and derived, anonymised
labels (dataset/real/labels_*.json, identifiers hashed) are committed.

    python3 -m dataset.real.sources --fetch      # download + verify + unpack
    python3 -m dataset.real.sources --manifest   # rewrite dataset/real/manifest.json

Sources (licences checked 2026-10-30):

* MIDD, "Multi-layout Invoice Document Dataset" (Symbiosis Institute of Technology, Pune),
  Zenodo record 5113009, CC BY 4.0. Real Indian GST tax invoices, OCR'd, released as IOB
  token files (one token per row + tag). Tags: supplier/buyer name + GSTIN, invoice number,
  invoice date, grand total. No page images, no line breaks, no layout: the text we feed the
  extractor is the token stream joined with spaces. 4 "layouts" from a small set of suppliers,
  all billed to one buyer group, so the invoices are strongly clustered (see FINDINGS).
* Sparrow invoices (Katana ML), HuggingFace katanaml-org/invoices-donut-data-v1, MIT; built on
  Kozlowski & Weichbroth (2021) "Samples of electronic invoices", Mendeley Data
  doi:10.17632/tnj49gpmtz.2. Rendered invoice IMAGES with labels (invoice no, IBAN, items,
  totals). The CONTENT is generated (fake names, US-style $ amounts with decimal commas), so it
  is a public, real-image OCR test, not real business data. test + validation splits only.

Not used (and why): HF vanshbadjate07/invoice and AjitRawat/invoice (22 real Indian invoices,
no licence stated, so all rights reserved); Kaggle "Indian Grocery Tax Invoice Image Dataset"
and "GhostInk" (Kaggle download needs the user's account; skipped per instructions);
FATURA / FATURA2 (CC BY 4.0, but template-generated, non-Indian, no bank fields); SROIE /
CORD (receipts, not invoices).
"""
from __future__ import annotations

import argparse
import hashlib
import json
import pathlib
import subprocess
import urllib.request

REPO = pathlib.Path(__file__).resolve().parents[2]
RAW = REPO / "data" / "third_party" / "raw"
MANIFEST = pathlib.Path(__file__).resolve().parent / "manifest.json"

SOURCES = {
    "midd": {
        "title": "Multi-layout Invoice Document Dataset (MIDD)",
        "landing": "https://zenodo.org/records/5113009",
        "licence": "CC-BY-4.0",
        "attribution": "Baviskar, D., Ahirrao, S., Kotecha, K. (2021). Multi-layout Invoice Document Dataset (MIDD). Zenodo. doi:10.5281/zenodo.5113009",
        "files": {
            "midd/IOB.rar": ("https://zenodo.org/api/records/5113009/files/IOB.rar/content",
                             "d06421afbf734fc4ba7a75b46b9c786cdd20baba42bb7011e423f50a57dd4609"),
        },
    },
    "sparrow": {
        "title": "Sparrow invoice dataset (Katana ML), test + validation splits",
        "landing": "https://huggingface.co/datasets/katanaml-org/invoices-donut-data-v1",
        "licence": "MIT (dataset card); underlying Mendeley Data doi:10.17632/tnj49gpmtz.2",
        "attribution": "Katana ML, Sparrow invoices-donut-data-v1; Kozlowski M., Weichbroth P. (2021) Samples of electronic invoices, Mendeley Data V2",
        "files": {
            "sparrow/test.parquet": ("https://huggingface.co/datasets/katanaml-org/invoices-donut-data-v1/resolve/main/data/test-00000-of-00001-56af6bd5ff7eb34d.parquet",
                                     "712a9a65000d6a2cf04d41bc794843ca30452bb87473951b1adafd3610eab08b"),
            "sparrow/validation.parquet": ("https://huggingface.co/datasets/katanaml-org/invoices-donut-data-v1/resolve/main/data/validation-00000-of-00001-b8a5c4a6237baf25.parquet",
                                           "dcc1d76abc7a779c0a6724359ccabd4e384162a7d6ef3d73ed473b4eb4ee76b4"),
        },
    },
}


def sha256(p: pathlib.Path) -> str:
    return hashlib.sha256(p.read_bytes()).hexdigest()


def fetch() -> None:
    for name, src in SOURCES.items():
        for rel, (url, want) in src["files"].items():
            dst = RAW / rel
            if not dst.exists():
                dst.parent.mkdir(parents=True, exist_ok=True)
                urllib.request.urlretrieve(url, dst)
            got = sha256(dst)
            if got != want:
                raise SystemExit(f"{rel}: sha256 {got} != pinned {want}")
            print("ok", rel)
    iob = RAW / "midd" / "IOB"
    if not iob.exists():
        # RAR v4/v5; any unrar/7-Zip works (we used 7-Zip 24.09 7zz, linux-arm64).
        for tool in (["7zz", "x", "-y"], ["7z", "x", "-y"], ["unrar", "x", "-o+"]):
            try:
                subprocess.run([*tool, "IOB.rar"], cwd=RAW / "midd", check=True, capture_output=True)
                break
            except (FileNotFoundError, subprocess.CalledProcessError):
                continue
        else:
            raise SystemExit("need 7zz/7z/unrar to unpack data/third_party/raw/midd/IOB.rar")


def write_manifest(counts: dict) -> dict:
    m = {"generated_by": "python3 -m dataset.real.sources --manifest", "raw_dir": "data/third_party/raw/ (gitignored)",
         "sources": {}}
    for name, src in SOURCES.items():
        m["sources"][name] = {k: v for k, v in src.items() if k != "files"}
        m["sources"][name]["files"] = [{"path": rel, "url": url, "sha256": h} for rel, (url, h) in src["files"].items()]
        m["sources"][name]["count"] = counts.get(name)
        m["sources"][name]["labels"] = f"dataset/real/labels_{name}.json (identifiers salted-sha256, 16 hex)"
    MANIFEST.write_text(json.dumps(m, indent=1) + "\n")
    return m


if __name__ == "__main__":
    ap = argparse.ArgumentParser()
    ap.add_argument("--fetch", action="store_true")
    ap.add_argument("--manifest", action="store_true")
    a = ap.parse_args()
    if a.fetch:
        fetch()
    if a.manifest:
        from dataset.real.load import write_labels
        print(json.dumps(write_manifest(write_labels()), indent=1))
