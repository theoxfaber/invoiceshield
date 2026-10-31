"""Durable sandbox spend counter (sandbox/app.py PgStore). A free host without a persistent disk
restarts often, so the lifetime model-call (spend) cap and the daily model cap live in Postgres there.

Always-run tests: the fail-closed paths (no store, unreachable store, missing driver) never allow a
model call. Real-Postgres tests run when SANDBOX_TEST_DATABASE_URL points at a scratch database
(they drop and recreate their own table); otherwise they are skipped."""
from __future__ import annotations

import importlib
import os
import threading

import pytest

PG_URL = os.environ.get("SANDBOX_TEST_DATABASE_URL", "")
needs_pg = pytest.mark.skipif(not PG_URL, reason="set SANDBOX_TEST_DATABASE_URL to a scratch Postgres to run")


def load(monkeypatch, tmp_path, **env):
    for k in ("SANDBOX_DATABASE_URL", "DATABASE_URL", "SANDBOX_REQUIRE_DURABLE", "BASETEN_API_KEY", "SANDBOX_TRUST_PROXY", "SANDBOX_CLIENT_IP_HEADER"):
        monkeypatch.delenv(k, raising=False)
    for k, v in env.items():
        monkeypatch.setenv(k, str(v))
    monkeypatch.setenv("SANDBOX_STATE", str(tmp_path / "state.json"))
    import sandbox.app as app

    return importlib.reload(app)


def test_lowered_defaults(monkeypatch, tmp_path):
    for k in ("SANDBOX_SPEND_CAP_USD", "SANDBOX_MODEL_CALLS_PER_DAY", "SANDBOX_USD_PER_MODEL_CALL"):
        monkeypatch.delenv(k, raising=False)
    app = load(monkeypatch, tmp_path)
    assert app.SPEND_CAP_USD == 5.00 and app.MODEL_CALLS_PER_DAY == 50
    assert app.MODEL_CALLS_TOTAL == 250 and app.MODEL_CALLS_TOTAL * app.USD_PER_MODEL_CALL <= 5.00 + 1e-9


def test_require_durable_without_database_never_calls_model(monkeypatch, tmp_path):
    app = load(monkeypatch, tmp_path, SANDBOX_REQUIRE_DURABLE=1)
    lim, c = app.Limits(), app.Counter(tmp_path / "state.json")
    assert c.store is None
    assert lim.take_model_calls(1, c) is False
    assert "unavailable" in c.get()["durable_store"]


def test_unreachable_database_fails_closed(monkeypatch, tmp_path):
    # Port 1 on localhost refuses the connection at once (or the driver is missing): either way no call.
    app = load(monkeypatch, tmp_path, SANDBOX_REQUIRE_DURABLE=1, SANDBOX_DATABASE_URL="postgresql://u:p@127.0.0.1:1/db")
    lim, c = app.Limits(), app.Counter(tmp_path / "state.json")
    assert c.store is not None
    assert lim.take_model_calls(1, c) is False
    assert c.store.ping() is False and c.store.last_error
    got = c.get()
    assert "unavailable" in got["durable_store"] and got["attempts"] == 0
    assert c.store.add(attempts=1) is False  # dropped, never raises
    assert not (tmp_path / "state.json").exists()  # no silent fallback to the ephemeral file


def test_store_error_mid_transaction_fails_closed(monkeypatch, tmp_path):
    app = load(monkeypatch, tmp_path, SANDBOX_REQUIRE_DURABLE=1, SANDBOX_DATABASE_URL="postgresql://ignored")

    class Boom:
        closed = False

        def transaction(self):
            raise RuntimeError("connection lost")

        def close(self):
            self.closed = True

    c = app.Counter(tmp_path / "state.json")
    c.store._connect = lambda: Boom()
    assert app.Limits().take_model_calls(1, c) is False
    assert c.store.last_error == "RuntimeError"


@pytest.fixture
def pg(monkeypatch, tmp_path):
    import psycopg

    with psycopg.connect(PG_URL, autocommit=True) as conn:
        conn.execute("DROP TABLE IF EXISTS sandbox_counter")
    return lambda **env: load(monkeypatch, tmp_path, SANDBOX_DATABASE_URL=PG_URL, SANDBOX_REQUIRE_DURABLE=1, **env)


@needs_pg
def test_pg_lifetime_cap_survives_restart(pg, tmp_path):
    app = pg(SANDBOX_SPEND_CAP_USD=0.10, SANDBOX_USD_PER_MODEL_CALL=0.02, SANDBOX_MODEL_CALLS_PER_DAY=1000)
    assert app.MODEL_CALLS_TOTAL == 5
    lim, c = app.Limits(), app.Counter(tmp_path / "state.json")
    assert lim.take_model_calls(2, c) and lim.take_model_calls(2, c)
    assert not lim.take_model_calls(2, c)  # 4 + 2 > 5
    lim2, c2 = app.Limits(), app.Counter(tmp_path / "other.json")  # "restart" on a fresh disk
    assert lim2.take_model_calls(1, c2) and not lim2.take_model_calls(1, c2)
    assert c2.get()["model_calls_total"] == 5
    assert not (tmp_path / "state.json").exists() and not (tmp_path / "other.json").exists()


@needs_pg
def test_pg_daily_cap_is_durable_too(pg, tmp_path):
    app = pg(SANDBOX_MODEL_CALLS_PER_DAY=3)
    lim, c = app.Limits(), app.Counter(tmp_path / "state.json")
    assert lim.take_model_calls(2, c) and not lim.take_model_calls(2, c)
    lim2, c2 = app.Limits(), app.Counter(tmp_path / "state.json")  # restart does not reset today's count
    assert lim2.take_model_calls(1, c2) and not lim2.take_model_calls(1, c2)
    assert c2.get(lim2.day)["model_calls_today"] == 3 and lim2.snapshot()["model_calls_today"] == 3
    # a new UTC day gets a fresh daily budget, the lifetime count carries on
    assert c2.store.reserve_model_calls(1, "2099-01-01", 3, app.MODEL_CALLS_TOTAL) == 1
    assert c2.get()["model_calls_total"] == 4


@needs_pg
def test_pg_concurrent_reserves_never_pass_the_cap(pg, tmp_path):
    app = pg(SANDBOX_SPEND_CAP_USD=0.20, SANDBOX_USD_PER_MODEL_CALL=0.02, SANDBOX_MODEL_CALLS_PER_DAY=1000)
    assert app.MODEL_CALLS_TOTAL == 10
    counters = [app.Counter(tmp_path / f"s{i}.json") for i in range(4)]  # 4 "instances", 4 connections
    ok = []

    def worker(c):
        lim = app.Limits()
        for _ in range(10):
            if lim.take_model_calls(1, c):
                ok.append(1)

    ts = [threading.Thread(target=worker, args=(c,)) for c in counters]
    [t.start() for t in ts]
    [t.join() for t in ts]
    assert len(ok) == 10 and counters[0].get()["model_calls_total"] == 10


@needs_pg
def test_pg_public_counter_persists(pg, tmp_path):
    app = pg()
    c = app.Counter(tmp_path / "state.json")
    c.add(attempts=1, review=1, paid=0)
    c.add(attempts=1, block=1)
    got = app.Counter(tmp_path / "state.json").get()
    assert got["attempts"] == 2 and got["review"] == 1 and got["block"] == 1 and got["paid"] == 0
    assert got["durable_store"] == "postgres"


class _H:
    def __init__(self, app, headers, peer="10.0.0.1"):
        self.headers, self.client_address = headers, (peer, 1234)
        self.client_ip = app.Handler.client_ip.__get__(self)


def test_client_ip_behind_proxy_is_not_spoofable(monkeypatch, tmp_path):
    app = load(monkeypatch, tmp_path, SANDBOX_TRUST_PROXY=1, SANDBOX_CLIENT_IP_HEADER="CF-Connecting-IP")
    assert _H(app, {"CF-Connecting-IP": "9.9.9.9", "X-Forwarded-For": "1.2.3.4, 9.9.9.9"}).client_ip() == "9.9.9.9"
    assert _H(app, {"X-Forwarded-For": "1.2.3.4"}).client_ip() == "10.0.0.1"  # header missing: shared bucket
    app = load(monkeypatch, tmp_path, SANDBOX_TRUST_PROXY=1)
    assert _H(app, {"X-Forwarded-For": "6.6.6.6, 9.9.9.9"}).client_ip() == "9.9.9.9"  # rightmost, not the forged leftmost
    app = load(monkeypatch, tmp_path)
    assert _H(app, {"X-Forwarded-For": "6.6.6.6", "CF-Connecting-IP": "6.6.6.6"}).client_ip() == "10.0.0.1"
