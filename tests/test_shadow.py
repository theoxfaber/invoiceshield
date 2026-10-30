"""Shadow-mode harness on the synthetic sample CSV (no real AP data)."""
import pathlib

from tools.shadow import classify, kappa, run

REPO = pathlib.Path(__file__).resolve().parents[1]


def test_classify_matrix():
    assert classify("pay", "allow") == "agree"
    assert classify("pay", "review") == "shield_more_cautious"
    assert classify("pay", "block") == "disagree_shield_refused"
    assert classify("hold", "allow") == "disagree_shield_would_pay"
    assert classify("reject", "block") == "agree"


def test_kappa_perfect_and_chance():
    assert kappa([{"human": "pay", "shield": "allow"}, {"human": "reject", "shield": "block"}]) == 1.0
    assert kappa([{"human": "pay", "shield": "allow"}, {"human": "pay", "shield": "allow"}]) is None


def test_sample_csv_end_to_end():
    rep = run(REPO / "tools" / "shadow_sample.csv", REPO / "dataset" / "fixtures" / "master.json", "24ZZACB7319B1Z5")
    assert rep["rows"] == 22 and rep["valid"] == 21
    assert rep["counts"]["invalid_row"] == 1
    # nothing the team stopped would have been paid by the shield
    assert rep["counts"]["disagree_shield_would_pay"] == 0
    by_note = {d["row"]: d for d in rep["decisions"]}
    dup = [d for d in rep["decisions"] if d.get("human") == "reject" and "clean" in d["invoice_file"]]
    assert dup and all(d["shield"] == "block" and any(r.startswith("duplicate_invoice") for r in d["reasons"]) for d in dup)
    x04 = [d for d in rep["decisions"] if d["invoice_file"].endswith("X04.pdf")][0]
    assert x04["human"] == "pay" and x04["shield"] != "allow"   # the modelled team mistake is caught
    assert by_note and rep["agreement_strict"]["wilson95"][0] <= rep["agreement_strict"]["rate"]
