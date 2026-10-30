"""ui/console.html (the evidence console) must show only numbers read from eval/results.

Rebuilds the page data from the committed files (offline replay of recorded model calls; the build
itself asserts every replayed proposal and checker reply equals the committed row) and compares it
with the data embedded in the committed page. Thumbnails are masked (poppler versions rasterise
differently)."""
import json
import pathlib
import re
import shutil

import pytest

ROOT = pathlib.Path(__file__).resolve().parents[1]
PAGE = ROOT / "ui" / "console.html"


def embedded() -> dict:
    m = re.search(r'<script type="application/json" id="data">(.*?)</script>', PAGE.read_text(), re.S)
    return json.loads(m.group(1).replace("<\\/", "</"))


def mask(d: dict) -> dict:
    d = json.loads(json.dumps(d))
    for k in ("img", "img_orig"):
        d["failclosed"]["photo"][k] = "<img>"
    return d


def test_results_view_matches_score_files():
    d = embedded()["results"]
    score = json.loads((ROOT / "eval" / "results" / "heldout-v2-run2-55fb679.score.json").read_text())
    meta = json.loads((ROOT / "eval" / "results" / "heldout-v2-run2-55fb679.meta.json").read_text())
    assert d["heldout"]["run_id"] == "heldout-v2-run2-55fb679"
    assert d["heldout"]["commit"] == meta["commit"]
    for arm in ("rules", "checker_only", "both"):
        for k, v in d["heldout"]["arms"][arm].items():
            assert v == score[arm][k], (arm, k)


@pytest.mark.skipif(not shutil.which("pdftoppm"), reason="needs poppler-utils")
def test_console_regenerates_from_results():
    from ui.build.build_console import build_data
    assert mask(build_data(thumbs=False)) == mask(embedded()), "ui/console.html is stale: run python3 -m ui.build.build_console"


def test_review_demo_reproduces_recorded_verdicts():
    from shield.client import BIN
    if not BIN.exists():
        pytest.skip(f"needs the governor binary at {BIN}: run `make build` (or set GOVERNOR_BIN)")
    from ui.review_demo import QUEUE, start
    gov, key, rows, queued = start()
    try:
        assert all(r["matches"] for r in rows), [(r["id"], r["verdict"], r["reasons"]) for r in rows if not r["matches"]]
        assert [q[0] for q in queued] == list(QUEUE) and all(q[1] == "review" for q in queued), queued
        import httpx
        r = httpx.get(gov.base + "/v1/reviews", headers={"Authorization": f"Bearer {key}"}, trust_env=False)
        assert r.status_code == 200 and r.json()["count"] == len(QUEUE), r.text[:300]
        assert gov.http.get("/v1/reviews").status_code == 403   # the agent key cannot read the queue
    finally:
        gov.close()
