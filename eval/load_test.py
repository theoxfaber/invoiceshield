"""Load test for the governor HTTP API (POST /v1/actions). No model calls, no API key.

    python3 -m eval.load_test                      # default: concurrency 1,8,32 for 15 s each
    python3 -m eval.load_test --levels 1,16 --seconds 10 --out eval/results/load-custom.json
    python3 -m eval.load_test --unsigned           # Phase 1 style: caller-supplied evidence (dev flag)

Evidence path (Phase 2): by default every request carries a freshly signed extractor envelope
(Ed25519, new nonce, issued_at = now, bound to the PDF's sha256), exactly the wire format of
extractor/service.py, and the governor is started with only the matching public key, so each
request pays for signature verification, freshness, nonce replay and document-hash checks. The
evidence inside the envelope is computed once per case, outside the sandbox, by the same
`governor_evidence` the extraction worker runs, and signing happens in the client before the
timer starts, so the numbers are the governor's cost, not the extractor's (extraction of a PDF is
~0.1-20 s and is not on the governor's hot path). `--unsigned` instead starts the governor with
GOVERNOR_DEV_UNSIGNED_EVIDENCE=1 (+ GOVERNOR_EVAL_CLOCK=1) and sends the old `evidence` field.

Bodies are real governor requests: the committed 24-case dev risk set (eval/riskset/manifest.json)
read by the deterministic assembler, as a hijacked agent would propose them (vendor, invoice no.,
printed total, PO) with an all-clear checker. Each request gets a unique invoice number so the
duplicate-key path does not dominate, the eval clock moves 8 days per request so the 7-day split
window does not fill, and the state is reset (POST /v1/eval/reset, not timed) after every pass over
the 23 bodies so PO balances do not run dry and turn everything into over_po_balance BLOCKs. Under
concurrency the resets interleave with in-flight requests, so the verdict mix is approximate; it is
recorded per level.

The governor is started fresh for each level (release build, eval clock on, logs to a temp file),
so levels do not share state. Client: asyncio + httpx on the same machine, so the client competes
with the server for CPU; server-side latency is read back from the governor's own /metrics
histogram and recorded next to the client-side numbers.

Raw results go to eval/results/load-<UTC timestamp>.json (or --out).
"""
from __future__ import annotations

import argparse
import asyncio
import datetime as dt
import json
import os
import pathlib
import platform
import secrets
import socket
import statistics
import subprocess
import sys
import tempfile
import time

import httpx

ROOT = pathlib.Path(__file__).resolve().parents[1]
BIN = pathlib.Path(os.environ.get("GOVERNOR_BIN", ROOT / "governor" / "target" / "release" / "governor-server"))
MASTER = ROOT / "dataset" / "fixtures" / "master.json"
CLEAR = json.dumps({"signals": []})


def build_bodies() -> list[dict]:
    import hashlib

    from assembler.evidence import assemble
    from assembler.extract import governor_evidence, printed_total_paise
    from extractor.service import Extractor
    from shield.pipeline import BUYER_GSTIN

    man = json.loads((ROOT / "eval/riskset/manifest.json").read_text())
    bodies = []
    for c in man:
        if not c["vendor_id"]:
            continue
        ev = assemble(ROOT / c["pdf"], c["cover_email"])
        amount = printed_total_paise(ev.text) or c["amount_paise"]
        bodies.append({
            "case": c["id"],
            "proposal": {"vendor_id": c["vendor_id"], "invoice_no": c["invoice_no"], "amount_paise": amount, "po_ref": c["po_ref"]},
            "evidence": governor_evidence(ev, BUYER_GSTIN),
            "checker_raw": CLEAR,
            "doc_sha256": hashlib.sha256((ROOT / c["pdf"]).read_bytes()).hexdigest(),
            "email_sha256": Extractor.email_sha256(c["cover_email"]),
        })
    return bodies


class Signer:
    """Signs envelopes in the extraction service's wire format (extractor/service.py) with a fresh key."""

    def __init__(self):
        from extractor import EXTRACTOR_VERSION, signing

        self._signing = signing
        self._version = EXTRACTOR_VERSION
        self._sk = signing.generate()
        self.public_hex = signing.public_hex(self._sk.public_key())
        self.key_id = signing.key_id(self._sk.public_key())
        # Separate checker key (post-review hardening: checker output is signed and bound).
        self._ck = signing.generate()
        self.checker_public_hex = signing.public_hex(self._ck.public_key())

    def body(self, b: dict, invoice_no: str) -> dict:
        payload = {
            "v": 1, "key_id": self.key_id, "doc_sha256": b["doc_sha256"], "email_sha256": b["email_sha256"],
            "extractor_version": self._version, "issued_at": int(time.time()), "nonce": secrets.token_hex(16),
            "status": "ok", "sandbox": {"wall_s": None, "network_namespace": None}, "evidence": b["evidence"],
        }
        from checker.checker import sign_checker_report

        env = self._signing.sign(self._sk, payload)
        return {"proposal": {**b["proposal"], "invoice_no": invoice_no, "document_sha256": b["doc_sha256"]},
                "signed_evidence": env, "signed_checker": sign_checker_report(self._ck, b["checker_raw"], env)}


def unsigned_body(b: dict, invoice_no: str) -> dict:
    return {"proposal": {**b["proposal"], "invoice_no": invoice_no}, "evidence": b["evidence"], "checker_raw": b["checker_raw"]}


def free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def pct(xs: list[float], p: float) -> float:
    if not xs:
        return float("nan")
    xs = sorted(xs)
    k = (len(xs) - 1) * p / 100
    lo, hi = int(k), min(int(k) + 1, len(xs) - 1)
    return xs[lo] + (xs[hi] - xs[lo]) * (k - lo)


def rss_kb(pid: int) -> int | None:
    try:
        for line in pathlib.Path(f"/proc/{pid}/status").read_text().splitlines():
            if line.startswith("VmRSS:"):
                return int(line.split()[1])
    except OSError:
        return None
    return None


def server_histogram(text: str, route: str = "/v1/actions") -> dict:
    """Parse the governor's latency histogram for one route into bucket counts and estimated quantiles."""
    buckets, count, total = [], 0, 0.0
    for line in text.splitlines():
        if line.startswith("governor_http_request_duration_seconds_bucket") and f'route="{route}"' in line:
            le = line.split('le="')[1].split('"')[0]
            buckets.append((float("inf") if le == "+Inf" else float(le), int(line.rsplit(" ", 1)[1])))
        elif line.startswith("governor_http_request_duration_seconds_count") and f'route="{route}"' in line:
            count = int(line.rsplit(" ", 1)[1])
        elif line.startswith("governor_http_request_duration_seconds_sum") and f'route="{route}"' in line:
            total = float(line.rsplit(" ", 1)[1])

    def q(p):  # upper bound of the bucket holding the p-quantile (Prometheus-style, no interpolation)
        need = count * p
        for le, c in buckets:
            if c >= need:
                return le
        return float("inf")

    return {"count": count, "mean_ms": (total / count * 1000) if count else None,
            "p50_le_ms": q(0.50) * 1000, "p95_le_ms": q(0.95) * 1000, "p99_le_ms": q(0.99) * 1000,
            "buckets_le_s_cumulative": [[le if le != float("inf") else "+Inf", c] for le, c in buckets]}


async def run_level(bodies: list[dict], concurrency: int, seconds: float, warmup: float, signed: bool = True,
                    db: str = "file") -> dict:
    key = secrets.token_hex(16)
    port = free_port()
    log = tempfile.NamedTemporaryFile(prefix="governor-load-", suffix=".log", delete=False)
    env = {**os.environ, "GOVERNOR_API_KEY": key, "GOVERNOR_MASTER": str(MASTER), "GOVERNOR_PORT": str(port),
           "GOVERNOR_EVAL_CLOCK": "1", "RUST_LOG": "warn"}
    dbdir = tempfile.TemporaryDirectory(prefix="governor-load-db-")
    # file: a real SQLite WAL database on local disk (a transaction per decision, as in production).
    env["GOVERNOR_DB"] = str(pathlib.Path(dbdir.name) / "governor.db") if db == "file" else ":memory:"
    for k in ("GOVERNOR_EXTRACTOR_PUBKEY", "GOVERNOR_DEV_UNSIGNED_EVIDENCE", "GOVERNOR_ANCHOR_KEY", "GOVERNOR_ANCHOR_KEY_FILE"):
        env.pop(k, None)
    signer = Signer() if signed else None
    if signer:
        env["GOVERNOR_EXTRACTOR_PUBKEY"] = signer.public_hex
        env["GOVERNOR_CHECKER_PUBKEY"] = signer.checker_public_hex
        # The synthetic envelopes do not come from a real sandbox run (network_namespace=None), so
        # the load test accepts that explicitly instead of signing a sandbox fact it did not observe.
        env["GOVERNOR_ALLOW_NO_NETNS"] = "1"
    else:
        env["GOVERNOR_DEV_UNSIGNED_EVIDENCE"] = "1"
    proc = subprocess.Popen([str(BIN)], env=env, stderr=log, stdout=subprocess.DEVNULL)
    base = f"http://127.0.0.1:{port}"
    limits = httpx.Limits(max_connections=concurrency, max_keepalive_connections=concurrency)
    lat: list[float] = []
    verdicts: dict[str, int] = {}
    errors: dict[str, int] = {}
    seq = 0
    resets = [0]
    try:
        async with httpx.AsyncClient(base_url=base, headers={"Authorization": f"Bearer {key}"}, limits=limits,
                                     timeout=30, trust_env=False) as client:
            for _ in range(100):
                try:
                    if (await client.get("/readyz")).status_code == 200:
                        break
                except httpx.HTTPError:
                    pass
                await asyncio.sleep(0.05)
            else:
                raise RuntimeError("governor did not become ready")
            rss_start = rss_kb(proc.pid)
            t_start = time.perf_counter()
            measure_from = t_start + warmup
            stop_at = measure_from + seconds

            async def worker():
                nonlocal seq
                while True:
                    now = time.perf_counter()
                    if now >= stop_at:
                        return
                    seq += 1
                    if seq % len(bodies) == 0:
                        await client.post("/v1/eval/reset")
                        resets[0] += 1
                    b = bodies[seq % len(bodies)]
                    inv = f"{b['proposal']['invoice_no']}-L{seq}"
                    body = signer.body(b, inv) if signer else unsigned_body(b, inv)
                    # Eval clock: 8 simulated days per request, so the 7-day split window never fills up and
                    # the mix stays close to the per-case verdicts (PO balances still deplete).
                    hdr = {"x-eval-now": str(1_793_000_000 + seq * 8 * 86_400)}
                    t0 = time.perf_counter()
                    try:
                        r = await client.post("/v1/actions", json=body, headers=hdr)
                        ok = r.status_code == 200
                        v = r.json().get("verdict", "?") if ok else f"http_{r.status_code}"
                    except httpx.HTTPError as e:
                        ok, v = False, type(e).__name__
                    t1 = time.perf_counter()
                    if t0 >= measure_from:
                        if ok:
                            lat.append((t1 - t0) * 1000)
                            verdicts[v] = verdicts.get(v, 0) + 1
                        else:
                            errors[v] = errors.get(v, 0) + 1

            await asyncio.gather(*(worker() for _ in range(concurrency)))
            elapsed = time.perf_counter() - measure_from
            metrics = (await client.get("/metrics")).text
            rss_end = rss_kb(proc.pid)
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            proc.kill()
        log.close()
        os.unlink(log.name)
        dbdir.cleanup()
    n = len(lat)
    return {
        "concurrency": concurrency, "evidence_path": "signed" if signed else "dev_unsigned", "db": db, "measured_seconds": round(elapsed, 3), "warmup_seconds": warmup,
        "requests_ok": n, "errors": errors, "throughput_rps": round(n / elapsed, 1) if elapsed else None,
        "client_latency_ms": {"p50": round(pct(lat, 50), 3), "p95": round(pct(lat, 95), 3), "p99": round(pct(lat, 99), 3),
                              "max": round(max(lat), 3) if lat else None, "mean": round(statistics.fmean(lat), 3) if lat else None},
        "server_latency_from_metrics": server_histogram(metrics),
        "verdicts": verdicts, "state_resets": resets[0], "server_rss_kb": {"start": rss_start, "end": rss_end},
    }


def main(argv=None) -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--levels", default="1,8,32")
    ap.add_argument("--seconds", type=float, default=15)
    ap.add_argument("--warmup", type=float, default=2)
    ap.add_argument("--out", default=None)
    ap.add_argument("--db", choices=["file", "memory"], default="file", help="SQLite file in a temp dir (default) or :memory:")
    ap.add_argument("--unsigned", action="store_true", help="dev path: GOVERNOR_DEV_UNSIGNED_EVIDENCE=1, caller-supplied evidence")
    a = ap.parse_args(argv)
    if not BIN.exists():
        print(f"missing {BIN}: run `cd governor && cargo build --release` (or set GOVERNOR_BIN)", file=sys.stderr)
        return 2
    bodies = build_bodies()
    commit = subprocess.run(["git", "rev-parse", "--short", "HEAD"], cwd=ROOT, capture_output=True, text=True).stdout.strip()
    result = {
        "kind": "governor_load_test", "endpoint": "POST /v1/actions",
        "evidence_path": ("dev_unsigned (GOVERNOR_DEV_UNSIGNED_EVIDENCE=1, GOVERNOR_EVAL_CLOCK=1, caller-supplied evidence)" if a.unsigned else
                          "signed (fresh Ed25519 extractor envelope per request: signature, key id, freshness, nonce, document sha256 verified; GOVERNOR_EVAL_CLOCK=1 for the ledger clock only)"),
        "state_store": ("SQLite WAL file on local disk (temp dir, fresh per level)" if a.db == "file" else "SQLite :memory:"),
        "started_utc": dt.datetime.now(dt.timezone.utc).isoformat(timespec="seconds"),
        "git_commit": commit, "binary": str(BIN.relative_to(ROOT)) if BIN.is_relative_to(ROOT) else str(BIN),
        "machine": {"note": "single shared sandbox VM; client and server on the same host; other jobs were running",
                    "cpus": os.cpu_count(), "arch": platform.machine(), "python": platform.python_version(),
                    "loadavg_at_start": os.getloadavg()},
        "workload": {"bodies": len(bodies), "cases": [b["case"] for b in bodies],
                     "notes": "unique invoice_no per request; eval clock +8 days per request; state reset after every 23 requests (untimed); checker all-clear"},
        "levels": [],
    }
    for c in [int(x) for x in a.levels.split(",") if x]:
        r = asyncio.run(run_level(bodies, c, a.seconds, a.warmup, signed=not a.unsigned, db=a.db))
        result["levels"].append(r)
        cl = r["client_latency_ms"]
        print(f"c={c:>3}  {r['throughput_rps']:>7} rps  p50 {cl['p50']:.2f} ms  p95 {cl['p95']:.2f} ms  p99 {cl['p99']:.2f} ms  "
              f"errors {sum(r['errors'].values())}  verdicts {r['verdicts']}  server mean {r['server_latency_from_metrics']['mean_ms']:.3f} ms")
    result["loadavg_at_end"] = os.getloadavg()
    out = pathlib.Path(a.out) if a.out else ROOT / "eval" / "results" / f"load-{dt.datetime.now(dt.timezone.utc):%Y%m%dT%H%M%SZ}.json"
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps(result, indent=1) + "\n")
    print("wrote", out.relative_to(ROOT) if out.is_relative_to(ROOT) else out)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
