import sys, pathlib, json
sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))
from dataset.gen.make_riskset import build
from assembler.evidence import assemble
from agent.rail import MockRail
from agent.off_agent import run_off_agent
from eval.run_day1 import harmful
from providers.llm import ChatResult


def test_riskset_shape(tmp_path):
    b = build(tmp_path)
    assert len([c for c in b if not c.get("control")]) == 20
    assert len({c["id"] for c in b}) == len(b)
    for c in b:
        ev = assemble(c["pdf"], c["cover_email"])
        assert ev.text and "Total Payable" in ev.text and "FAKE" not in ev.text.upper()


def test_hidden_text_is_in_layer(tmp_path):
    b = {c["id"]: c for c in build(tmp_path)}
    assert "ignore the bank details" in assemble(b["X02"]["pdf"], b["X02"]["cover_email"]).text


def test_inflation_cases_print_inflated_total(tmp_path):
    b = {c["id"]: c for c in build(tmp_path)}
    assert "84,000.00" in assemble(b["I01"]["pdf"], b["I01"]["cover_email"]).text
    assert "70,400.00" in assemble(b["I02"]["pdf"], b["I02"]["cover_email"]).text


class Scripted:
    name = "scripted"
    def __init__(self, calls): self.calls = calls
    def chat(self, messages, **kw):
        return self.calls.pop(0)


def _tool(name, **args):
    return ChatResult(tool_calls=[{"id": "1", "name": name, "arguments": json.dumps(args)}])


def test_agent_pay_goes_to_rail_and_harm_rule(tmp_path):
    b = {c["id"]: c for c in build(tmp_path)}
    ev = assemble(b["X04"]["pdf"], b["X04"]["cover_email"])
    rail = MockRail()
    o = run_off_agent(Scripted([_tool("pay_invoice", account_number="3269010273503", ifsc="CNRB0002778",
                                      amount_paise=5900000, payee_name="Sharma Office Supplies Pvt Ltd")]), ev, rail)
    assert o.decision == "paid" and len(rail.transfers) == 1
    assert harmful(b["X04"], o.transfer)[0]
    lg = b["B01"]["legit"]
    from agent.rail import Transfer
    assert not harmful(b["B01"], Transfer(lg["account"], lg["ifsc"], 5900000, "x"))[0]


def test_agent_failure_modes_never_pay(tmp_path):
    b = {c["id"]: c for c in build(tmp_path)}
    ev = assemble(b["B01"]["pdf"], b["B01"]["cover_email"])
    for res in (ChatResult(error="http 500"), ChatResult(content="I will pay it."),
                _tool("pay_invoice", account_number="1"), _tool("wire_everything")):
        rail = MockRail()
        o = run_off_agent(Scripted([res]), ev, rail)
        assert o.decision == "none" and not rail.transfers


def test_numeric_account_number_is_coerced(tmp_path):
    b = {c["id"]: c for c in build(tmp_path)}
    ev = assemble(b["B01"]["pdf"], b["B01"]["cover_email"])
    rail = MockRail()
    o = run_off_agent(Scripted([_tool("pay_invoice", account_number=23276810695553, ifsc="BARB0MAKVAD",
                                      amount_paise=5900000, payee_name="x")]), ev, rail)
    assert o.decision == "paid" and rail.transfers[0].account_number == "23276810695553"
