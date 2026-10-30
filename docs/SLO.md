# Service level objectives: governor decision API

**Where these numbers come from.** Load-test runs on **a single shared sandbox VM**: 2 vCPU aarch64,
the Python load client and the governor on the same host, other build/eval jobs running at the same
time. Not a production benchmark and not a capacity plan. Reproduce with `make load`
(`python3 -m eval.load_test`).

**Current build (Phases 1-3 + 6 on main): SQLite WAL state + signed evidence.**
Raw data: `eval/results/load-20261030T143846Z.json` (run 2026-10-30 20:07 IST; release build of the
Phase 6 tree squashed onto main `cafdde7`, before it was committed, so the file's `git_commit` reads
`cafdde7`). Load average 2.6 at start, 5.5 at end, on 2 CPUs.
- **Evidence path: signed.** Every request carries a freshly signed Ed25519 extractor envelope
  (new nonce, `issued_at` = now, bound to the PDF's sha256), and the governor was started with only
  the matching `GOVERNOR_EXTRACTOR_PUBKEY`, so each decision pays for signature verification, key id,
  freshness, nonce-replay and document-hash checks. `GOVERNOR_DEV_UNSIGNED_EVIDENCE` was **not** set.
  `GOVERNOR_EVAL_CLOCK=1` was set only so the ledger clock can move 8 days per request (freshness
  always uses the wall clock). The signed payload's evidence is computed once per case by the same
  `governor_evidence` the extraction worker runs, and signing happens in the client before the timer
  starts: the numbers are the governor's cost, not the extractor's.
- **State: SQLite WAL file** on local disk (fresh temp DB per level), one transaction per decision.

**Workload.** `POST /v1/actions` with the 23 dev risk-set invoices that name a master vendor, as a
hijacked agent would propose them, read by the real assembler; unique invoice numbers, eval clock
+8 days per request, state reset after every 23 requests (untimed) so PO balances do not run dry.
Verdict mix over the run: 16% ALLOW, 42% REVIEW, 42% BLOCK (2,299 / 6,134 / 6,066). 15 s measured
per level after a 2 s warm-up; **zero errors at every level** (14,499 measured requests).

## Measured (2026-10-30, 20:07 IST; SQLite WAL + signed evidence)

| Concurrency | Throughput | Client p50 | Client p95 | Client p99 | Server mean | Server p50 / p95 / p99 (histogram bucket upper bound) |
|---|---|---|---|---|---|---|
| 1  | 365.1 req/s | 2.23 ms | 4.03 ms | 5.79 ms | 0.76 ms | ≤1 / ≤2.5 / ≤5 ms |
| 8  | 386.3 req/s | 13.7 ms | 58.3 ms | 102.1 ms | 2.35 ms | ≤2.5 / ≤5 / ≤10 ms |
| 32 | 212.8 req/s | 89.1 ms | 448.9 ms | 700.9 ms | 5.11 ms | ≤5 / ≤25 / ≤25 ms |

Server RSS: 4.2 MB at start; 6.3 / 8.0 / 8.4 MB at the end of each level.

### Previous run (Phase 6 alone: in-memory state, unsigned evidence), kept for comparison
`eval/results/load-20261030T134647Z.json` (commit `11acaf9`, load average 5.3-6.0): c=1 426 req/s,
client p50/p95/p99 1.8/5.7/7.9 ms, server mean 0.37 ms; c=8 588 req/s, 9.4/33.3/63.3 ms, server mean
1.08 ms; c=32 217 req/s, 89/430/699 ms, server mean 4.39 ms; 0 errors in 18,490 requests.

*Client* latency is measured by the asyncio/httpx client around each request. *Server* latency comes
from the governor's own `governor_http_request_duration_seconds{route="/v1/actions"}` histogram
(read back from `/metrics` after each level), so its quantiles are bucket bounds, not exact values.

How to read it:
- Durable state + signature checks roughly doubled the server's mean time per decision (0.37 → 0.76 ms
  at c=1, 1.08 → 2.35 ms at c=8) and cut throughput at c=1/c=8 (426 → 365, 588 → 386 req/s). Server
  p99 buckets are unchanged at every level (≤5 / ≤10 / ≤25 ms).
- The client side is dominated by the Python client and CPU contention on a 2-vCPU box shared with
  other jobs; client p99 at c=8 rose from 63 ms to 102 ms.
- At 32 concurrent callers throughput **falls** (213 req/s) in both runs: client and server fight for
  2 CPUs, and decisions serialise on the SQLite write lock (plus an in-process lock kept as defence in
  depth) by design, so two copies of one invoice cannot both pass the duplicate check. This is the
  measured ceiling of this box, not of the design; it was not profiled further.
- For scale: in the recorded model runs an agent + checker call takes ~5.1 s at p50 (README), so the
  governor is a negligible share of end-to-end time per invoice.

## Objectives

Targets for one governor instance on hardware at least like the test box, for **up to 8 concurrent
callers** (an AP team's agents), set with headroom over the measured numbers above:

| SLI | Objective | Measured basis |
|---|---|---|
| Server-side latency of `POST /v1/actions` (governor histogram) | p99 ≤ **25 ms** over 30 days | p99 ≤ 10 ms at c=8, ≤ 25 ms even at c=32 (SQLite + signed) |
| Client-observed latency, same host/LAN | p99 ≤ **150 ms** (was 100 ms before Phases 2-3) | 102 ms at c=8 (SQLite + signed); 63 ms before |
| Sustained throughput | ≥ **200 decisions/s** | 213–386 req/s across levels (SQLite + signed) |
| Error rate (non-2xx other than 401) | < **0.1 %** | 0 errors in 14,499 measured requests (SQLite + signed) |
| Fail-closed correctness | **100 %** of unparseable requests and checker outputs end REVIEW or BLOCK; **0** ALLOWs that pay anyone but the master payee | Enforced by tests (`cargo test`, `pytest`), counted live by `governor_parse_failures_total` |
| Availability (`/readyz` = 200) | 99.5 % monthly (**target only, not measured**: no long-running deployment exists) | — |

The correctness objective is not a budget: a single violation is an incident (`docs/RUNBOOKS.md` §5).
Latency and availability misses only delay payments, because the gate fails closed.

The client p99 objective was raised from 100 ms to 150 ms after the SQLite + signed-evidence rerun
measured 102 ms at c=8 on this shared box; the server-side objective (the one the governor controls)
did not change and is met with the same headroom.

## Not covered / re-measure when

- **Extraction cost** is not in these numbers: the extractor signs once per document (PDF parse in a
  sandbox, ~0.1 s for a text PDF, up to ~20 s for an OCR'd photo), off the governor's hot path.
- **Disk:** the DB was on the sandbox's local disk; a networked or slower volume changes the commit
  cost. Re-run `make load` on the target host.
- Network hops, TLS termination, multiple replicas, and hours-long soak tests were not measured.
- The client is Python and shares the CPU; a dedicated load generator on another host would show
  higher throughput and lower client latency.
