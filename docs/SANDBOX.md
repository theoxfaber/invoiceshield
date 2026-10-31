# Sandbox: "try to get a payment out of the gate"

`sandbox/app.py` is a small stdlib HTTP front over the real pipeline: the sandboxed extraction service
(Ed25519-signed evidence), optionally Muse Spark Open (line proposals + the quarantined checker), and the
Rust governor, which decides PAY / REVIEW / BLOCK. A "fooled" AP agent proposes whatever the uploaded
document asks for. Nothing is ever paid: there is no payment rail, and on PAY the payee is the vendor
master's beneficiary id.

**Status (2026-10-31): deployment in progress (Render free web service + free Postgres, `render.yaml`).** See "Deployment" below.

## Hard limits

| limit | default | env | what happens past it |
|---|---|---|---|
| Synthetic data only | `dataset/fixtures/master.json` (fictional vendors) | `SANDBOX_MASTER` | the page tells visitors not to upload real invoices or personal data |
| Upload size | 2 MB, one PDF | `SANDBOX_MAX_BYTES` | HTTP 413 / 400 |
| Per-IP rate limit | token bucket, 4 per minute, burst 4 | `SANDBOX_IP_PER_MIN`, `SANDBOX_IP_BURST` | HTTP 429 |
| Per-IP daily cap | 40 requests | `SANDBOX_IP_PER_DAY` | HTTP 429 |
| Global daily cap | 500 requests | `SANDBOX_DAILY_CAP` | HTTP 429 for everyone until 00:00 UTC |
| Tracked IPs | 50,000 per day | (constant) | new IPs get HTTP 429 (fail closed) |
| Daily model calls | 50 | `SANDBOX_MODEL_CALLS_PER_DAY` | no model call; no checker report, so the governor answers **REVIEW** |
| Lifetime model calls (spend cap) | floor(5.00 / 0.02) = 250 calls | `SANDBOX_SPEND_CAP_USD` (clamped to at most 9.50), `SANDBOX_USD_PER_MODEL_CALL` | no model call ever again for this store; every request is **REVIEW** at most |
| Durable store unreachable | fail closed | `SANDBOX_DATABASE_URL`, `SANDBOX_REQUIRE_DURABLE=1` | no model call; every request is **REVIEW** at most |

- **Spend stays under $10.** On a public host the lifetime and daily model-call counts live in Postgres
  (`SANDBOX_DATABASE_URL`; table `sandbox_counter`), reserved atomically in one transaction (`SELECT ... FOR
  UPDATE`) before any model call, so a restart, a spin-down or a fresh container does not reset them, and
  concurrent instances cannot overshoot. With `SANDBOX_REQUIRE_DURABLE=1` (set in `render.yaml` and the
  sandbox image) no model is called unless that store answered: no database URL, a missing driver, a
  connection error or an error mid-transaction all mean no checker report, so the governor answers REVIEW.
  The public counter (attempts, paid, ...) is kept in the same table. Without a database URL (local
  compose) the counts are kept in the state file (`SANDBOX_STATE`, a volume in compose). The spend ceiling cannot be raised past $9.50 by configuration
  (`HARD_SPEND_CEILING_USD` in the code). If the count cannot be written, the call is refused (fail closed).
  The $0.02 per call figure is a pessimistic estimate, not a measured price: check it against the provider's
  bill after the first calls and lower `SANDBOX_SPEND_CAP_USD` (or raise the per-call figure) if calls cost more.
  As a second guard, set a hard budget/limit on the provider account itself.
- **Keys.** The governor API key is generated per boot inside the process and never leaves it. The model key
  (`BASETEN_API_KEY`) is read from the server environment only; it is never sent to the browser, logged or
  stored. The extraction subprocess gets an environment with every `*_API_KEY` / `*_TOKEN` removed. With no
  key set, no model is called and no request can be PAY.
- **Isolation per attempt.** The governor runs in eval mode with an in-memory ledger reset before every
  attempt (one lock), so attempts do not affect each other.
- **No request logging** of IPs beyond the in-memory counters (which roll over daily).
- **Client IP behind a proxy.** With `SANDBOX_TRUST_PROXY=1` the rate limiter keys on
  `SANDBOX_CLIENT_IP_HEADER` (`CF-Connecting-IP` on Render, set by Cloudflare and not forgeable by the
  client); without that header setting it uses the rightmost `X-Forwarded-For` entry, never the leftmost,
  which the client controls (Render appends to a client-sent `X-Forwarded-For`).

## Public counter

`GET /api/counter` returns `attempts`, `paid`, `review`, `block`, `paid_to_wrong_account`,
`foreign_bank_details_printed`, `foreign_bank_details_paid_to_master`, `model_capped`, `model_calls_total`,
plus today's request and model-call counts and the caps. `paid_to_wrong_account` counts PAY decisions whose
payee is not the master beneficiary of the proposed vendor; by construction it is expected to stay 0.

## Run it locally

```bash
python -m sandbox.app                                   # http://127.0.0.1:8000 (PORT overrides)
docker compose --profile sandbox up --build sandbox     # http://127.0.0.1:8001, state in the sandbox-data volume
```

Durable-counter tests: `tests/test_sandbox_durable.py`. The fail-closed tests always run; the real-Postgres
tests (restart, daily cap, 4 concurrent instances never passing the cap, public counter) run when
`SANDBOX_TEST_DATABASE_URL` points at a scratch database:

```bash
pip install -r sandbox/requirements.txt
SANDBOX_TEST_DATABASE_URL=postgresql://postgres@127.0.0.1:5432/postgres python -m pytest tests/test_sandbox_durable.py
```

Note: `docker-compose.yml` still passes its own local defaults (9.00 USD, 100 calls/day) to the compose
sandbox; the public deployment uses `render.yaml`.

CI (`.github/workflows/ci.yml`, job `compose`) starts it through compose with no model key, checks
`/api/counter`, and posts two synthetic samples (`bank-swap`, `genuine-1`): neither may be PAY, and the
counter must show 2 attempts and 0 paid.

## Deployment

`render.yaml` (Render Blueprint, free plan): one Docker web service built from `sandbox/Dockerfile`
(`python -m sandbox.app`, `PORT` from Render, `SANDBOX_TRUST_PROXY=1`, health check `/healthz`) and one
free Postgres for the durable counter (internal connection only). `BASETEN_API_KEY` is set as a secret
environment variable in the Render dashboard (`sync: false`), never in the repo. Render's free Postgres
expires 30 days after creation; when it does, the store is unreachable and the sandbox fails closed
(no model calls, REVIEW at most) until a new database URL is set.
