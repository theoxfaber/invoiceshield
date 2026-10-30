#!/usr/bin/env python3
"""Build ui/console.html: the evidence console (static, offline, no API key).

Five views, each built only from committed files; nothing here decides a verdict:
  1. Fail closed      a phone-photo invoice and an empty checker reply, both ending in REVIEW
                      (eval/results/variants_gate_only_phase1.jsonl, day1_on_spark_usdagent.jsonl,
                      demo-opening-case.json)
  2. Model calls      the exact request sent to Muse Spark Open and the response it returned, with the
                      latency measured at call time. Recorded calls: the request is rebuilt by
                      replaying the case offline through the same agent/checker code, and the response
                      is the cached reply under eval/cache/llm/<sha256(request)>.json.
  3. Agent | checker  the agent's proposal and the quarantined checker's reply side by side, for
                      Muse Spark Open and Muse Glimmer 30B on the same case (same cache)
  4. Approver         what reaches the approver queue, and what never does (BLOCK is final)
  5. Results          the post-freeze held-out v2 run read from its score/meta files, plus the dev,
                      blind run-1 and v1 post-fix numbers with their labels

The build fails if a replayed proposal or checker reply differs from the committed result row, so
the page cannot show a call that is not the one recorded.

    python3 -m ui.build.build_console          # writes ui/console.html
"""
from __future__ import annotations

import base64
import io
import json
import pathlib
import subprocess
import tempfile
from dataclasses import asdict

W = pathlib.Path(__file__).resolve().parent
REPO = W.parents[1]
R = REPO / "eval" / "results"
OUT = REPO / "ui" / "console.html"

HELDOUT_RUN = "heldout-v2-run2-55fb679"
ON_FILES = {"spark": "day1_on_spark_usdagent.jsonl", "glimmer": "day1_on_usdagent.jsonl"}
CALL_CASES = [("X02", 0), ("L02", 0), ("X04", 0)]   # Spark call log (agent + checker each)
SIDE_CASES = [("X02", 0), ("X04", 0)]              # Spark vs Glimmer, same case and sample
EMPTY_CHECKER = ("X_hi", 0)                        # recorded "empty output" checker reply


def jl(name: str) -> list[dict]:
    return [json.loads(x) for x in (R / name).read_text().splitlines() if x.strip()]


def row(rows: list[dict], cid: str, sample: int | None = None) -> dict:
    return next(r for r in rows if r["id"] == cid and (sample is None or r.get("sample") == sample))


# ---------------------------------------------------------------- recorded model calls (offline replay)
def capture(provider_name: str, cid: str, sample: int) -> dict:
    """Replay one dev case offline and capture each request payload + cached response."""
    from providers.llm import get_provider
    from shield.pipeline import load_master, model_phase

    manifest = json.loads((REPO / "eval" / "riskset" / "manifest.json").read_text())
    case = dict(next(c for c in manifest if c["id"] == cid))
    case["pdf"] = str(REPO / case["pdf"])
    master = load_master()
    vendors = {v["vendor_id"]: v for v in master["vendors"]}
    prov = get_provider(provider_name, offline=True)
    log: list[dict] = []
    orig = prov.chat

    def chat(messages, *, tools=None, reasoning="medium", max_tokens=4096, sample=0):
        payload = prov._payload(messages, tools, reasoning, max_tokens)
        res = orig(messages, tools=tools, reasoning=reasoning, max_tokens=max_tokens, sample=sample)
        log.append({"payload": payload, "response": asdict(res)})
        return res

    prov.chat = chat
    mp = model_phase(prov, case, vendors, sample)
    rec = row(jl(ON_FILES[provider_name]), cid, sample)
    # The page may only show what was recorded: the replayed proposal and checker reply must match.
    got_args = mp.proposal.args if mp.proposal.kind == "propose" else {}
    assert mp.proposal.kind == rec["agent_kind"], (provider_name, cid, mp.proposal.kind, rec["agent_kind"])
    assert got_args == (rec["agent_args"] or {}), (provider_name, cid, got_args, rec["agent_args"])
    if mp.checker is not None:
        assert mp.checker.raw_for_governor == rec["checker_raw"], (provider_name, cid)
    calls = []
    for i, c in enumerate(log):
        role = "agent" if i == 0 else ("checker" if i == 1 else "line proposer")
        resp = c["response"]
        calls.append({
            "role": role, "endpoint": "POST " + prov.url, "provider": prov.name, "model": prov.model,
            "cache_file": f"eval/cache/llm/{resp['cache_key']}.json", "cached": resp["cached"],
            "request": c["payload"],
            "response": {k: resp[k] for k in ("content", "tool_calls", "finish_reason", "usage", "error")}
                        | {"reasoning": resp["reasoning"]},
            "latency_s": resp["latency_s"],
        })
    return {"case": cid, "sample": sample, "provider": prov.name, "model": prov.model, "klass": rec["klass"],
            "outcome": rec["outcome"], "reasons": rec["reasons"], "agent_kind": rec["agent_kind"],
            "agent_args": rec["agent_args"], "checker_raw": rec["checker_raw"], "checker_error": rec["checker_error"],
            "row_latency_s": rec["latency_s"], "source": f"eval/results/{ON_FILES[provider_name]}", "calls": calls}


# ---------------------------------------------------------------- thumbnails
def thumb(pdf: pathlib.Path, width: int = 520) -> str:
    from PIL import Image
    with tempfile.TemporaryDirectory() as t:
        subprocess.run(["pdftoppm", "-r", "60", "-f", "1", "-l", "1", "-png", str(pdf), f"{t}/p"], check=True)
        png = sorted(pathlib.Path(t).glob("p*.png"))[0]
        im = Image.open(png).convert("RGB")
        im.thumbnail((width, width * 2))
        buf = io.BytesIO()
        im.save(buf, "JPEG", quality=72, optimize=True)
    return "data:image/jpeg;base64," + base64.b64encode(buf.getvalue()).decode()


# ---------------------------------------------------------------- data
def build_data() -> dict:
    var = jl("variants_gate_only_phase1.jsonl")
    base = jl("day1_gate_only_fa.jsonl")
    vman = {c["id"]: c for c in json.loads((REPO / "eval" / "variants" / "manifest.json").read_text())}
    photo, t2, orig = row(var, "B01-photo"), row(var, "B01-t2"), row(base, "B01")
    spark_on = jl(ON_FILES["spark"])
    empty = row(spark_on, *EMPTY_CHECKER)
    n_empty = [f"{r['id']} sample {r['sample']}" for r in spark_on if r.get("checker_error") == "empty output"]
    oc = json.loads((R / "demo-opening-case.json").read_text())

    failclosed = {
        "photo": {"id": "B01-photo", "invoice_no": vman["B01-photo"]["invoice_no"], "verdict": photo["verdict"],
                  "reasons": photo["reasons"], "tamper_alert": photo["tamper_alert"],
                  "img": thumb(REPO / vman["B01-photo"]["pdf"]), "img_orig": thumb(REPO / "eval" / "riskset" / "B01.pdf"),
                  "orig_verdict": orig["verdict"], "orig_reasons": orig["reasons"], "t2_verdict": t2["verdict"],
                  "source": "eval/results/variants_gate_only_phase1.jsonl (B01-photo, B01-t2); eval/results/day1_gate_only_fa.jsonl (B01)"},
        "empty": {"id": empty["id"], "sample": empty["sample"], "outcome": empty["outcome"], "reasons": empty["reasons"],
                  "checker_ok": empty["checker_ok"], "checker_error": empty["checker_error"],
                  "checker_raw": empty["checker_raw"], "agent_args": empty["agent_args"], "latency_s": empty["latency_s"],
                  "provider": empty["provider"], "model": empty["model"], "all_empty": n_empty,
                  "source": f"eval/results/{ON_FILES['spark']}"},
        "oc1": {"verdict": oc["arms"]["both"]["verdict"], "signals": oc["arms"]["both"]["signals"],
                "reasons": oc["arms"]["both"]["reasons"], "commit": oc["commit"][:7],
                "source": "eval/results/demo-opening-case.json (arm: both)"},
    }

    calls = [capture("spark", c, s) for c, s in CALL_CASES]
    side = [{"case": c, "sample": s, "spark": capture("spark", c, s), "glimmer": capture("glimmer", c, s)}
            for c, s in SIDE_CASES]
    for sd in side:   # the call log already carries these; keep the page small
        for p in ("spark", "glimmer"):
            for call in sd[p]["calls"]:
                call.pop("request")

    # ---- approver: what the queue shows and what it never shows
    approver = {
        "review": [{"id": r["id"], "sample": r["sample"], "reasons": r["reasons"], "args": r["agent_args"]}
                   for r in spark_on if r["sample"] == 0 and r["outcome"] == "review"],
        "blocked": [{"id": "L02", "reasons": row(spark_on, "L02", 0)["reasons"], "source": f"eval/results/{ON_FILES['spark']}"},
                    {"id": "OC1", "reasons": [f"{x['rule']}:{x['verdict']}" for x in oc["arms"]["both"]["reasons"]],
                     "source": "eval/results/demo-opening-case.json"}],
    }

    # ---- results, every number read from a file
    score = json.loads((R / f"{HELDOUT_RUN}.score.json").read_text())
    meta = json.loads((R / f"{HELDOUT_RUN}.meta.json").read_text())
    keys = ("strict", "lenient", "attacks", "attack_paid", "benign", "benign_blocked", "clean_benign", "clean_benign_false_alarm", "cases")
    arms = lambda s: {a: {k: s[a][k] for k in keys} for a in s}  # noqa: E731
    v1g, v1c = (json.loads((R / f).read_text()) for f in ("v1-rerun-after-6c7414f.score.json", "v1-rerun-after-4b584de.score.json"))
    v1 = arms({"rules": v1g["rules"], "checker_only": v1c["checker_only"], "both": v1g["both"]})
    run1 = json.loads((R / "blind-cases-run1-680be4d.summary.json").read_text())
    abl = json.loads((R / "ablation_dev_fa.json").read_text())
    dev = {}
    for p, (off_f, on_f) in {"Muse Spark Open": ("day1_off_spark.jsonl", "day1_on_spark.jsonl"),
                             "Muse Glimmer 30B": ("day1_off.jsonl", "day1_on.jsonl")}.items():
        off, on = jl(off_f), jl(on_f)
        dev[p] = {"off_wrong": sum(1 for r in off if not r["control"] and r["decision"] == "paid" and r.get("harmful")),
                  "attack_samples": sum(1 for r in off if not r["control"]),
                  "on_wrong": sum(1 for r in on if not r["control"] and r.get("harmful")),
                  "ctl_paid_on": sum(1 for r in on if r["control"] and r["outcome"] == "paid"),
                  "ctl_samples": sum(1 for r in on if r["control"]), "files": f"eval/results/{off_f}, eval/results/{on_f}"}
    results = {
        "heldout": {"run_id": HELDOUT_RUN, "commit": meta["commit"], "ran_at_ist": meta["ran_at_ist"], "note": meta["note"],
                    "harness_errors": meta["harness_errors"], "files_checked": meta["files_checked"],
                    "arms": arms(score), "files": f"eval/results/{HELDOUT_RUN}.score.json, eval/results/{HELDOUT_RUN}.meta.json"},
        "v1_postfix": {"arms": v1, "commits": {"rules": "6c7414f", "both": "6c7414f", "checker_only": "4b584de"},
                       "files": "eval/results/v1-rerun-after-6c7414f.score.json, eval/results/v1-rerun-after-4b584de.score.json"},
        "run1": {"strict": run1["strict_match"], "lenient": run1["lenient_match"], "attack_paid": run1["attack_paid"],
                 "attacks": run1["attacks"], "benign_blocked": run1["benign_blocked"], "benign": run1["benign"],
                 "files": "eval/results/blind-cases-run1-680be4d.summary.json"},
        "dev": dev,
        "ablation": {a: {k: abl[a][k] for k in ("attack_rows", "attacks_passed", "attack_cases", "attack_cases_passed_any_sample", "controls_paid", "control_rows")}
                     for a in ("rules", "checker_only", "both")} | {"files": "eval/results/ablation_dev_fa.json"},
    }
    return {"failclosed": failclosed, "calls": calls, "side": side, "approver": approver, "results": results}


def font(name: str) -> str:
    return "data:font/ttf;base64," + base64.b64encode((W / "fonts" / name).read_bytes()).decode()


def main() -> None:
    data = build_data()
    tpl = (W / "console.template.html").read_text()
    blob = json.dumps(data, ensure_ascii=False, separators=(",", ":")).replace("</", "<\\/")
    out = (tpl.replace("/*FONT_GEIST*/", font("Geist-VF.ttf")).replace("/*FONT_MONO*/", font("GeistMono-VF.ttf"))
              .replace("/*DATA*/", blob))
    OUT.write_text(out)
    print("wrote", OUT.relative_to(REPO), len(out), "bytes")


if __name__ == "__main__":
    main()
