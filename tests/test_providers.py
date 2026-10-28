import json

import httpx
import pytest

from agent.schemas import CheckerReport, OffPayment, PaymentProposal
from providers.llm import NimGlimmer, with_reasoning
from providers.strict import extract_json, parse_model


def ok_body(content="", tool_calls=None, finish="stop"):
    msg = {"role": "assistant", "content": content, "reasoning_content": "thinking"}
    if tool_calls:
        msg["tool_calls"] = tool_calls
    return {"choices": [{"message": msg, "finish_reason": finish}], "usage": {"total_tokens": 10}}


def provider(handler, tmp_path, **kw):
    return NimGlimmer("k", transport=httpx.MockTransport(handler), cache_dir=tmp_path, retries=2, **kw)


def test_reasoning_line_goes_into_system_prompt():
    m = with_reasoning([{"role": "system", "content": "You are AP."}, {"role": "user", "content": "hi"}], "high")
    assert m[0]["content"].startswith("Reasoning strength: high\n\nYou are AP.")
    assert with_reasoning([{"role": "user", "content": "x"}], "low")[0] == {"role": "system", "content": "Reasoning strength: low"}


def test_payload_temperature_and_cache(tmp_path, monkeypatch):
    monkeypatch.setattr("time.sleep", lambda s: None)
    seen = []

    def h(req):
        body = json.loads(req.content)
        seen.append(body)
        return httpx.Response(200, json=ok_body('{"a":1}'))

    p = provider(h, tmp_path)
    r1 = p.chat([{"role": "user", "content": "x"}], max_tokens=100)
    assert r1.ok and not r1.cached and r1.content == '{"a":1}' and r1.reasoning == "thinking"
    assert seen[0]["temperature"] == 1.0 and seen[0]["max_tokens"] == 4000 and seen[0]["model"] == "meta/muse-glimmer-30b"
    r2 = p.chat([{"role": "user", "content": "x"}], max_tokens=100)
    assert r2.cached and len(seen) == 1
    r3 = p.chat([{"role": "user", "content": "x"}], max_tokens=100, sample=1)  # new sample = new call
    assert not r3.cached and len(seen) == 2


def test_failures_are_errors_not_cached(tmp_path, monkeypatch):
    monkeypatch.setattr("time.sleep", lambda s: None)
    calls = []

    def h(req):
        calls.append(1)
        return httpx.Response(503)

    p = provider(h, tmp_path)
    r = p.chat([{"role": "user", "content": "x"}])
    assert not r.ok and r.error == "http 503" and len(calls) == 2
    assert list(tmp_path.iterdir()) == []
    assert not provider(lambda q: httpx.Response(200, text="not json"), tmp_path).chat([{"role": "user", "content": "y"}]).ok
    assert provider(lambda q: httpx.Response(400), tmp_path, ).chat([{"role": "user", "content": "z"}]).error == "http 400"


def test_offline_and_missing_key(tmp_path):
    assert provider(lambda q: httpx.Response(200, json=ok_body("x")), tmp_path, offline=True).chat([{"role": "user", "content": "x"}]).error.startswith("offline")
    p = NimGlimmer("", cache_dir=tmp_path)
    assert p.chat([{"role": "user", "content": "x"}]).error == "no NVIDIA_API_KEY"


def test_tool_calls_parsed(tmp_path):
    tc = [{"id": "c1", "type": "function", "function": {"name": "propose_payment", "arguments": '{"vendor_id":"V001"}'}}]
    r = provider(lambda q: httpx.Response(200, json=ok_body("", tc, "tool_calls")), tmp_path).chat([{"role": "user", "content": "x"}])
    assert r.tool_calls == [{"id": "c1", "name": "propose_payment", "arguments": '{"vendor_id":"V001"}'}]


@pytest.mark.parametrize("text", [None, "", "   ", "garbage", '{"a":1} {"b":2}', "[1,2]", '{"a": NaN}',
                                  'Sure! {"a":1}', '{"a":1', "x" * 70000])
def test_extract_json_rejects(text):
    obj, err = extract_json(text)
    assert obj is None and err


def test_extract_json_accepts_fence():
    assert extract_json('```json\n{"a": 1}\n```') == ({"a": 1}, None)


GOOD = {"vendor_id": "V001", "invoice_no": "SOS/26-27/0231", "amount_paise": 4500000, "po_ref": "PO-2026-0101", "reason": "stationery"}


@pytest.mark.parametrize("mutate", [
    lambda d: d.update(bank_account="123"),          # bank field smuggled in
    lambda d: d.update(amount_paise="4500000"),      # string amount (strict)
    lambda d: d.update(amount_paise=45000.0),        # float rupees
    lambda d: d.update(amount_paise=0),
    lambda d: d.update(vendor_id="NEW"),
    lambda d: d.pop("po_ref"),
])
def test_proposal_strict(mutate):
    d = dict(GOOD)
    mutate(d)
    inst, err = parse_model(d, PaymentProposal)
    assert inst is None and err


def test_proposal_ok_and_checker_no_allow():
    assert parse_model(json.dumps(GOOD), PaymentProposal)[0].amount_paise == 4500000
    rep = {"extracted": {}, "signals": [{"type": "hidden_text", "severity": "allow"}]}
    assert parse_model(rep, CheckerReport)[0] is None
    rep["signals"][0]["severity"] = "block"
    assert parse_model(rep, CheckerReport)[0].signals[0].severity == "block"
    assert parse_model({"extracted": {}, "signals": [], "override": True}, CheckerReport)[0] is None
    assert parse_model({"account_number": "23276810695553", "ifsc": "BARB0MAKVAD", "amount_paise": 1, "payee_name": "x"}, OffPayment)[0]


def test_spark_provider_payload_and_selection(tmp_path, monkeypatch):
    from providers.llm import BasetenMuseSpark, get_provider
    seen = []

    def h(req):
        seen.append((str(req.url), json.loads(req.content)))
        return httpx.Response(200, json={"choices": [{"message": {"role": "assistant", "content": '{"a":1}',
                                          "reasoning_content": "r"}, "finish_reason": "stop"}]})

    p = BasetenMuseSpark("placeholder", transport=httpx.MockTransport(h), cache_dir=tmp_path)
    r = p.chat([{"role": "system", "content": "You are AP."}, {"role": "user", "content": "x"}], reasoning="low")
    url, body = seen[0]
    assert r.ok and r.content == '{"a":1}' and r.reasoning == "r"
    assert url == "https://inference.baseten.co/v1/chat/completions"
    assert body["model"] == "meta-models/muse-spark-open-1026"
    assert body["messages"][0]["content"] == "You are AP."  # no Glimmer reasoning line
    monkeypatch.delenv("LLM_PROVIDER", raising=False)
    assert get_provider(cache_dir=tmp_path).name == "muse-spark-open-baseten"
    monkeypatch.setenv("LLM_PROVIDER", "glimmer")
    assert get_provider(cache_dir=tmp_path).name == "glimmer-nim"
    with pytest.raises(ValueError):
        get_provider("gpt", cache_dir=tmp_path)
