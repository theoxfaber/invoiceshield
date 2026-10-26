"""LLM provider interface + Muse Glimmer 30B on NVIDIA NIM.

Design rules (docs/DECISIONS.md):
- Glimmer runs at temperature 1.0 (Meta: don't run it greedy). Reasoning
  strength goes in the system prompt, not a parameter.
- NIM ignores response_format, so callers strict-parse every output
  (providers/strict.py). A failed call is returned as an error result, never
  raised into an ALLOW: callers map it to REVIEW.
- Every call is cached on disk by a hash of its full input plus a sample index,
  so eval runs k=3 samples once and replays them for the demo.
"""
from __future__ import annotations

import hashlib
import json
import os
import time
from dataclasses import asdict, dataclass, field
from pathlib import Path
from typing import Any, Protocol

import httpx

NIM_URL = "https://integrate.api.nvidia.com/v1/chat/completions"
GLIMMER = "meta/muse-glimmer-30b"
DEFAULT_CACHE = Path(__file__).resolve().parents[1] / "eval" / "cache" / "llm"


@dataclass
class ChatResult:
    content: str = ""
    reasoning: str = ""
    tool_calls: list[dict] = field(default_factory=list)
    finish_reason: str | None = None
    error: str | None = None
    usage: dict = field(default_factory=dict)
    latency_s: float = 0.0
    cached: bool = False
    cache_key: str = ""

    @property
    def ok(self) -> bool:
        return self.error is None


class LLMProvider(Protocol):
    name: str

    def chat(self, messages: list[dict], *, tools: list[dict] | None = None,
             reasoning: str = "medium", max_tokens: int = 4096, sample: int = 0) -> ChatResult: ...


def with_reasoning(messages: list[dict], strength: str) -> list[dict]:
    """Prefix the system prompt with Glimmer's reasoning-strength line."""
    line = f"Reasoning strength: {strength}"
    msgs = [dict(m) for m in messages]
    if msgs and msgs[0].get("role") == "system" and isinstance(msgs[0].get("content"), str):
        msgs[0]["content"] = f"{line}\n\n{msgs[0]['content']}"
    else:
        msgs.insert(0, {"role": "system", "content": line})
    return msgs


def cache_key(payload: dict, sample: int) -> str:
    blob = json.dumps({"payload": payload, "sample": sample}, sort_keys=True, ensure_ascii=False)
    return hashlib.sha256(blob.encode()).hexdigest()


class NimGlimmer:
    name = "glimmer-nim"

    def __init__(self, api_key: str | None = None, *, model: str = GLIMMER, url: str = NIM_URL,
                 cache_dir: Path | None = DEFAULT_CACHE, timeout_s: float = 240.0,
                 retries: int = 3, transport: httpx.BaseTransport | None = None,
                 offline: bool = False):
        self.api_key = api_key if api_key is not None else os.environ.get("NVIDIA_API_KEY", "")
        self.model, self.url, self.cache_dir = model, url, cache_dir
        self.retries, self.offline = retries, offline
        self.client = httpx.Client(timeout=timeout_s, transport=transport)

    def _payload(self, messages, tools, reasoning, max_tokens) -> dict:
        p: dict[str, Any] = {
            "model": self.model,
            "messages": with_reasoning(messages, reasoning),
            "temperature": 1.0,
            "max_tokens": max(4000, max_tokens),
            "stream": False,
        }
        if tools:
            p["tools"] = tools
            p["tool_choice"] = "auto"
        return p

    def chat(self, messages, *, tools=None, reasoning="medium", max_tokens=4096, sample=0) -> ChatResult:
        payload = self._payload(messages, tools, reasoning, max_tokens)
        key = cache_key(payload, sample)
        path = self.cache_dir / f"{key}.json" if self.cache_dir else None
        if path and path.exists():
            data = json.loads(path.read_text())
            return ChatResult(**{**data, "cached": True, "cache_key": key})
        if self.offline:
            return ChatResult(error="offline: no cached response", cache_key=key)
        if not self.api_key:
            return ChatResult(error="no NVIDIA_API_KEY", cache_key=key)
        res = self._call(payload)
        res.cache_key = key
        if path and res.ok:  # never cache failures; a retry later may succeed
            path.parent.mkdir(parents=True, exist_ok=True)
            d = asdict(res)
            d.pop("cached"), d.pop("cache_key")
            path.write_text(json.dumps(d, ensure_ascii=False))
        return res

    def _call(self, payload: dict) -> ChatResult:
        headers = {"Authorization": f"Bearer {self.api_key}", "Accept": "application/json"}
        last = "unknown error"
        for attempt in range(self.retries):
            t0 = time.monotonic()
            try:
                r = self.client.post(self.url, json=payload, headers=headers)
            except httpx.HTTPError as e:
                last = f"transport: {type(e).__name__}"
            else:
                if r.status_code == 200:
                    return parse_response(r, time.monotonic() - t0)
                last = f"http {r.status_code}"
                if r.status_code not in (429, 500, 502, 503, 504):
                    break
            time.sleep(min(30, 2 ** attempt * 3))
        return ChatResult(error=last)


def parse_response(r: httpx.Response, latency: float) -> ChatResult:
    try:
        body = r.json()
        choice = body["choices"][0]
        msg = choice.get("message") or {}
    except (ValueError, KeyError, IndexError, TypeError) as e:
        return ChatResult(error=f"bad response body: {type(e).__name__}", latency_s=latency)
    calls = []
    for tc in msg.get("tool_calls") or []:
        fn = (tc or {}).get("function") or {}
        calls.append({"id": tc.get("id"), "name": fn.get("name"), "arguments": fn.get("arguments")})
    return ChatResult(
        content=msg.get("content") or "",
        reasoning=msg.get("reasoning_content") or msg.get("reasoning") or "",
        tool_calls=calls,
        finish_reason=choice.get("finish_reason"),
        usage=body.get("usage") or {},
        latency_s=round(latency, 2),
    )
