import json
import re
from pathlib import Path

from structural.gstin import validate

FIX = Path(__file__).resolve().parents[1] / "dataset" / "fixtures"
master = json.loads((FIX / "master.json").read_text())
prof = json.loads((FIX / "profiles.json").read_text())
IFSC = re.compile(r"^[A-Z]{4}0[A-Z0-9]{6}$")


def test_master_identifiers_are_well_formed():
    for v in master["vendors"]:
        if v["gstin"]:
            assert validate(v["gstin"], v["pan"]) == [], v["vendor_id"]
            assert IFSC.match(v["ifsc"]), v["ifsc"]
        assert "FAKE" not in v["ifsc"] and not v["bank_account"].startswith(("7777", "9999"))


def test_attackers_plausible_and_not_in_master():
    a = prof["attackers"]
    accts = {v["bank_account"] for v in master["vendors"]}
    gstins = {v["gstin"] for v in master["vendors"]}
    for k in ("X_bank_change", "X_ifsc_changed", "R2_insider_edit"):
        assert IFSC.match(a[k]["ifsc"]) and a[k]["bank_account"] not in accts
    assert validate(a["L01"]["gstin"]) == [] and a["L01"]["gstin"] not in gstins
    assert validate(a["L03"]["gstin_one_off"]) == ["gstin_bad_checksum"]  # one char off = typo-class forgery
    v001 = master["vendors"][0]
    assert v001["bank_name"] == "Bank of Baroda" and a["X_bank_change"]["bank_name"] == "Canara Bank"


def test_po_owners_exist():
    ids = {v["vendor_id"] for v in master["vendors"]}
    assert all(p["owner_vendor_id"] in ids for p in master["pos"])
