"""Offline demo: replay the recorded dev-set runs with no API key and no network.

    python3 -m eval.demo              # Muse Spark Open + Muse Glimmer, shield OFF and ON
    python3 -m eval.demo --strict     # exit 1 if any replayed outcome differs from the committed rows

Model replies come from the committed cache (eval/cache/llm, keyed by request hash); the Rust governor
runs for real (governor/target/release/governor-server). The committed result files in eval/results are
never touched: rows are written to a temp directory and compared, row by row, with the committed ones on
the fields that matter (decision/outcome and harm), ignoring latency and cache flags.

Then open ui/demo-v3.html in a browser for the replay UI (the page the submission video records).
"""
from __future__ import annotations

import argparse
import contextlib
import io
import json
import pathlib
import sys
import tempfile

ROOT = pathlib.Path(__file__).resolve().parents[1]
RESULTS = ROOT / "eval" / "results"
PROVIDERS = {"spark": "_spark", "glimmer": ""}
# Committed rows the ON replay is compared with: the runs made with the current ON agent prompt
# (propose_payment has a `currency` argument since the hardening/usd-agent change). The older
# day1_on*.jsonl rows were recorded with the previous prompt and no longer replay from the cache.
ON_RESULTS = {"spark": "day1_on_spark_usdagent.jsonl", "glimmer": "day1_on_usdagent.jsonl"}


def rows(path: pathlib.Path) -> dict:
    out = {}
    for line in path.read_text().splitlines():
        if line.strip():
            r = json.loads(line)
            out[(r["id"], r["sample"])] = r
    return out


def replay(module, provider: str, tmp: pathlib.Path) -> str:
    """Run an eval module's main() offline with its RESULTS redirected to tmp. Returns its stdout."""
    module.RESULTS = tmp
    argv, sys.argv = sys.argv, [module.__name__, "--offline", "--provider", provider]
    buf = io.StringIO()
    try:
        with contextlib.redirect_stdout(buf):
            module.main()
    finally:
        sys.argv = argv
    return buf.getvalue()


def compare(new: dict, old: dict, key: str) -> list[str]:
    diffs = []
    for k in sorted(set(new) | set(old)):
        a, b = new.get(k), old.get(k)
        if a is None or b is None:
            diffs.append(f"{k}: {'missing in replay' if a is None else 'not in committed rows'}")
        elif (a.get(key), bool(a.get("harmful"))) != (b.get(key), bool(b.get("harmful"))):
            diffs.append(f"{k}: replay {a.get(key)}{'*' if a.get('harmful') else ''} vs committed {b.get(key)}{'*' if b.get('harmful') else ''}")
    return diffs


def main(argv=None) -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--strict", action="store_true", help="exit 1 when a replayed outcome differs from the committed rows")
    ap.add_argument("--providers", default="spark,glimmer")
    a = ap.parse_args(argv)
    from eval import run_day1, run_day1_on

    drift = 0
    with tempfile.TemporaryDirectory(prefix="invoiceshield-demo-") as t:
        tmp = pathlib.Path(t)
        for p in [x for x in a.providers.split(",") if x]:
            suffix = PROVIDERS[p]
            replay(run_day1, p, tmp)
            replay(run_day1_on, p, tmp)
            off, on = rows(tmp / f"day1_off{suffix}.jsonl"), rows(tmp / f"day1_on{suffix}.jsonl")
            atk_off = [r for r in off.values() if not r["control"]]
            atk_on = [r for r in on.values() if not r["control"]]
            ctl_on = [r for r in on.values() if r["control"]]
            wrong_off = sum(1 for r in atk_off if r.get("harmful"))
            wrong_on = sum(1 for r in atk_on if r.get("harmful"))
            ctl_paid = sum(1 for r in ctl_on if r["outcome"] == "paid")
            outcomes = {}
            for r in atk_on:
                outcomes[r["outcome"]] = outcomes.get(r["outcome"], 0) + 1
            d = compare(off, rows(RESULTS / f"day1_off{suffix}.jsonl"), "decision") + \
                compare(on, rows(RESULTS / ON_RESULTS[p]), "outcome")
            drift += len(d)
            print(f"[{p}] shield OFF: paid to the wrong place {wrong_off}/{len(atk_off)} attack samples")
            print(f"[{p}] shield ON : paid to the wrong place {wrong_on}/{len(atk_on)}  "
                  f"(ON outcomes {dict(sorted(outcomes.items()))}); genuine controls paid {ctl_paid}/{len(ctl_on)}")
            print(f"[{p}] matches committed eval/results rows: {'yes' if not d else f'NO, {len(d)} differences'}")
            for line in d[:10]:
                print("   ", line)
    print("Replay UI: open ui/demo-v3.html in a browser (no server needed).")
    if drift and a.strict:
        print(f"DRIFT: {drift} replayed outcomes differ from committed rows", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
