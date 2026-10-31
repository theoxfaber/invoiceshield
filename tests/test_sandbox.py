"""Sandbox caps (sandbox/app.py): per-IP rate limit, per-IP and global daily caps, the daily and the
lifetime (spend) model-call caps, and the < $10 spend ceiling. No network, no model, no governor."""
from __future__ import annotations

import importlib
import json

import pytest


def load(monkeypatch, tmp_path, **env):
    for k, v in env.items():
        monkeypatch.setenv(k, str(v))
    monkeypatch.setenv("SANDBOX_STATE", str(tmp_path / "state.json"))
    monkeypatch.delenv("BASETEN_API_KEY", raising=False)
    import sandbox.app as app

    return importlib.reload(app)


def test_spend_ceiling_is_clamped_below_10_usd(monkeypatch, tmp_path):
    app = load(monkeypatch, tmp_path, SANDBOX_SPEND_CAP_USD=500, SANDBOX_USD_PER_MODEL_CALL=0.02)
    assert app.SPEND_CAP_USD < 10
    assert app.MODEL_CALLS_TOTAL * app.USD_PER_MODEL_CALL < 10
    app = load(monkeypatch, tmp_path, SANDBOX_SPEND_CAP_USD="nan", SANDBOX_USD_PER_MODEL_CALL=-1)
    assert app.SPEND_CAP_USD < 10 and app.USD_PER_MODEL_CALL > 0


def test_no_model_key_means_no_model(monkeypatch, tmp_path):
    app = load(monkeypatch, tmp_path)
    assert app.USE_MODEL is False


def test_per_ip_burst_then_429(monkeypatch, tmp_path):
    app = load(monkeypatch, tmp_path, SANDBOX_IP_PER_MIN=1, SANDBOX_IP_BURST=2, SANDBOX_IP_PER_DAY=100, SANDBOX_DAILY_CAP=100)
    lim = app.Limits()
    assert lim.admit("1.1.1.1") is None and lim.admit("1.1.1.1") is None
    assert "slow down" in lim.admit("1.1.1.1")
    assert lim.admit("2.2.2.2") is None  # another visitor is not affected


def test_global_daily_cap(monkeypatch, tmp_path):
    app = load(monkeypatch, tmp_path, SANDBOX_IP_BURST=10, SANDBOX_DAILY_CAP=3)
    lim = app.Limits()
    assert [lim.admit(f"10.0.0.{i}") for i in range(3)] == [None, None, None]
    assert "daily request cap" in lim.admit("10.0.0.9")


def test_per_ip_daily_cap(monkeypatch, tmp_path):
    app = load(monkeypatch, tmp_path, SANDBOX_IP_BURST=10, SANDBOX_IP_PER_MIN=600, SANDBOX_IP_PER_DAY=2, SANDBOX_DAILY_CAP=100)
    lim = app.Limits()
    assert lim.admit("3.3.3.3") is None and lim.admit("3.3.3.3") is None
    assert "your daily limit" in lim.admit("3.3.3.3")


def test_lifetime_model_cap_persists_across_restarts(monkeypatch, tmp_path):
    app = load(monkeypatch, tmp_path, SANDBOX_SPEND_CAP_USD=0.10, SANDBOX_USD_PER_MODEL_CALL=0.02, SANDBOX_MODEL_CALLS_PER_DAY=1000)
    assert app.MODEL_CALLS_TOTAL == 5
    lim, c = app.Limits(), app.Counter(tmp_path / "state.json")
    assert lim.take_model_calls(2, c) and lim.take_model_calls(2, c)
    assert not lim.take_model_calls(2, c)  # 4 + 2 > 5: no call, the request goes on to REVIEW
    assert json.loads((tmp_path / "state.json").read_text())["model_calls_total"] == 4
    lim2, c2 = app.Limits(), app.Counter(tmp_path / "state.json")  # "restart"
    assert lim2.take_model_calls(1, c2) and not lim2.take_model_calls(1, c2)


def test_daily_model_cap(monkeypatch, tmp_path):
    app = load(monkeypatch, tmp_path, SANDBOX_MODEL_CALLS_PER_DAY=3)
    lim, c = app.Limits(), app.Counter(tmp_path / "state.json")
    assert lim.take_model_calls(2, c) and not lim.take_model_calls(2, c) and lim.take_model_calls(1, c)


def test_unpersistable_state_fails_closed(monkeypatch, tmp_path):
    app = load(monkeypatch, tmp_path)
    c = app.Counter(tmp_path / "missing-dir" / "state.json")
    assert c.reserve_lifetime_calls(1, 100) is False


@pytest.mark.parametrize("body", [b"", b"not a pdf"])
def test_multipart_rejects_non_pdf(monkeypatch, tmp_path, body):
    app = load(monkeypatch, tmp_path)
    assert app.multipart_file(body, "text/plain") is None
