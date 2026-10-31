"""Durable model-call budget for the Vision Witness work (post-freeze, feat/meta-vision).

Caps (the same as the public sandbox's deployment):
  * at most VISION_CALLS_PER_DAY (50) real model calls in ANY rolling 24 hours (stricter than a calendar day
    in either UTC or IST);
  * at most VISION_SPEND_CAP_USD ($5) lifetime, counted at a pessimistic VISION_USD_PER_CALL ($0.02) per HTTP
    attempt, or the token-priced cost when it is higher.
State is a JSON file (VISION_BUDGET_STATE) written BEFORE each call; if it cannot be written the call is
refused (fail closed). Cached replays cost nothing and are not counted.
"""
from __future__ import annotations

import fcntl
import json
import os
import time
from contextlib import contextmanager
from pathlib import Path

from providers.llm import ChatResult

DEFAULT_STATE = Path(os.environ.get("VISION_BUDGET_STATE", "/workspace/hackathon/invoiceshield/meta-vision-budget.json"))
CALLS_PER_DAY = int(os.environ.get("VISION_CALLS_PER_DAY", "50"))
SPEND_CAP_USD = min(float(os.environ.get("VISION_SPEND_CAP_USD", "5.0")), 5.0)  # can be lowered, never raised here
USD_PER_CALL = max(float(os.environ.get("VISION_USD_PER_CALL", "0.02")), 0.001)


class Budget:
    def __init__(self, path: Path = DEFAULT_STATE, *, calls_per_day: int = CALLS_PER_DAY, spend_cap: float = SPEND_CAP_USD,
                 usd_per_call: float = USD_PER_CALL, clock=time.time):
        self.path, self.calls_per_day, self.spend_cap, self.usd_per_call, self.clock = Path(path), calls_per_day, spend_cap, usd_per_call, clock

    @contextmanager
    def _locked(self):
        self.path.parent.mkdir(parents=True, exist_ok=True)
        with open(str(self.path) + ".lock", "w") as lk:
            fcntl.flock(lk, fcntl.LOCK_EX)
            try:
                yield
            finally:
                fcntl.flock(lk, fcntl.LOCK_UN)

    def _read(self) -> dict:
        try:
            return json.loads(self.path.read_text())
        except (OSError, ValueError):
            return {"calls": [], "usd_total": 0.0}

    def _write(self, st: dict) -> bool:
        try:
            tmp = self.path.with_suffix(".tmp")
            tmp.write_text(json.dumps(st, indent=1))
            os.replace(tmp, self.path)
            return True
        except OSError:
            return False

    def status(self) -> dict:
        st = self._read()
        now = self.clock()
        last24 = [c for c in st["calls"] if now - c["t"] < 86400]
        return {"calls_total": len(st["calls"]), "calls_last_24h": len(last24), "usd_total": round(st["usd_total"], 4),
                "calls_per_day_cap": self.calls_per_day, "spend_cap_usd": self.spend_cap}

    def reserve(self, label: str) -> bool:
        try:
            return self._reserve(label)
        except OSError:  # cannot lock or write the ledger: fail closed, no call
            return False

    def _reserve(self, label: str) -> bool:
        with self._locked():
            st = self._read()
            now = self.clock()
            if sum(1 for c in st["calls"] if now - c["t"] < 86400) + 1 > self.calls_per_day:
                return False
            if st["usd_total"] + self.usd_per_call > self.spend_cap + 1e-9:
                return False
            st["calls"].append({"t": now, "label": label[:80], "usd": self.usd_per_call})
            st["usd_total"] += self.usd_per_call
            return self._write(st)

    def settle(self, usage: dict, *, usd_per_m_in: float = 0.0, usd_per_m_out: float = 0.0):
        """Raise the last call's cost to the token-priced figure when that is higher (never lower it)."""
        if not usage or not (usd_per_m_in or usd_per_m_out):
            return
        cost = usage.get("prompt_tokens", 0) / 1e6 * usd_per_m_in + usage.get("completion_tokens", 0) / 1e6 * usd_per_m_out
        with self._locked():
            st = self._read()
            if st["calls"] and cost > st["calls"][-1]["usd"]:
                st["usd_total"] += cost - st["calls"][-1]["usd"]
                st["calls"][-1]["usd"] = cost
                self._write(st)


class BudgetedProvider:
    """Wraps an LLM provider: cached replays are free; every real call (one HTTP attempt, no retries) is
    reserved against the Budget first. Past a cap the call is refused and returns an error result, which
    every caller already maps to REVIEW."""

    def __init__(self, make_provider, budget: Budget, label: str = "vision"):
        self.offline = make_provider(offline=True)
        self.live = make_provider(retries=1)
        self.budget, self.label = budget, label
        self.name = self.live.name
        self.real_calls = 0

    def chat(self, messages, **kw) -> ChatResult:
        r = self.offline.chat(messages, **kw)
        if r.ok:
            return r
        if not self.budget.reserve(self.label):
            return ChatResult(error="budget: model-call cap reached (fail closed)")
        self.real_calls += 1
        r = self.live.chat(messages, **kw)
        self.budget.settle(r.usage)
        return r
