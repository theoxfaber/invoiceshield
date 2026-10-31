# Post-review hardening (branch `fix/review-hardening`)

An external adversarial code review of InvoiceShield (2026-10-31) found no critical issues and the
findings below. This branch fixes every one of them. It also fixes the follow-ups the review found in
the docs. Every fix fails closed: when a new check cannot be satisfied, the governor answers REVIEW
(a human looks), never PAY.

## Integrity note: these are post-freeze changes

- The sealed v3 scores (94/150 strict and the other figures in `docs/V3_RESULTS.md`) were measured
  on tag **`freeze-v3`**. They describe that tag, **not this branch**.
- No sealed or held-out eval set was rerun, inspected or tuned for this branch. `eval/results/` and
  the sealed sets are untouched.
- What was run:
  - the Rust unit and integration suites (`cargo test`), clippy and `cargo fmt --check`
  - the Python suite (`pytest`) and ruff
  - the offline dev replay (`python -m eval.demo`, committed dev-set cache only)

  The pull request's CI runs the same.
- The new rules can only add REVIEW/BLOCK outcomes. They cannot turn an earlier REVIEW or BLOCK into
  PAY.
- One visible change: on a host where the extraction sandbox cannot create a network namespace and
  `GOVERNOR_ALLOW_NO_NETNS` is not set, genuine invoices that used to PAY now go to REVIEW (finding 2).
  That is the intended fail-closed default, and one more reason the `freeze-v3` scores must not be
  quoted for this branch.

## Findings and fixes

### 1. MEDIUM: checker signals were unauthenticated (trust.rs, engine.rs)

**Finding.** `checker_raw` travelled in the agent's request body. A compromised agent could:
- send `{"signals":[]}`,
- drop the field and accept `checker_missing`,
- or swap in a clean report.

Any of these silently removed the model layer.

**Fix.**
- **Signed by the checker.** The checker signs its output with its **own** Ed25519 key
  (`checker.checker.sign_checker_report`). The payload is canonical JSON
  `{v:1, kind:"checker", key_id, doc_sha256, evidence_nonce, issued_at, raw}`, and the request carries
  it as `signed_checker`.
- **Separate key, strict check.** The governor (`trust.rs::checker_signals`) verifies it with
  `verify_strict` against `GOVERNOR_CHECKER_PUBKEY`, a separate key set. An extractor key is refused as
  a checker key, both at startup and per request.
- **Bound to this request.** The report must name the same document and the **same single-use evidence
  nonce** as the signed evidence in that request, and be fresh (inside the evidence freshness window).
  So it cannot be replayed onto other evidence or another document.
- **Failures are REVIEW signals, never PAY.** One signal per failure: `checker_missing`,
  `checker_unsigned`, `checker_signature_invalid`, `checker_payload_invalid`,
  `checker_document_mismatch`, `checker_nonce_mismatch`, `checker_stale`.
- **Valid reports still only escalate.** A valid signed report is parsed fail-closed
  (`parse_checker_signals`); it can raise a verdict, never lower it.
- **Legacy input is off by default.** An unsigned `checker_raw` is ignored (REVIEW `checker_unsigned`)
  unless the operator sets `GOVERNOR_CHECKER_UNSIGNED_OK=1`, which is warned about at startup.
- **No key, no trust.** Without `GOVERNOR_CHECKER_PUBKEY`, every signed decision gets a REVIEW checker
  signal; this is warned about at startup too.
- **Callers updated.** `shield/client.py::GovernorProc` (used by the evals and the public sandbox) and
  `eval/load_test.py` provision a checker key and send `signed_checker`.

**Honest limit.** The signature stops a compromised agent only if the checker key lives outside the
agent's process.
- In this repository's harness (`GovernorProc`, the public sandbox app), the checker runs in the same
  Python process as the code that calls it. That code is the harness, not a model agent with tools.
- A production deployment runs the checker as its own service holding its own key, as the extractor
  already does.

The governor-side binding is complete. Splitting the checker into its own process is a deployment step,
now documented.

**Tests.**
- `governor/tests/trust.rs`:
  - `checker_dropped_by_agent_reviews`
  - `unsigned_empty_checker_from_agent_reviews_never_allow`
  - `unsigned_checker_raw_next_to_signed_checker_is_ignored`
  - `forged_checker_signature_reviews`
  - `checker_signed_by_the_extractor_key_is_not_a_checker`
  - `edited_checker_payload_reviews`
  - `replayed_checker_payload_reviews`
  - `checker_for_another_document_reviews`
  - `stale_checker_and_bad_checker_schema_review`
  - `legacy_unsigned_checker_only_with_explicit_flag`
  - `no_checker_key_configured_fails_closed`
- `config.rs::checker_and_netns_policy_default_fail_closed`
- `tests/test_review_fixes.py`, end to end through the real binary:
  - `test_signed_checker_path_allows_a_clean_invoice`
  - `test_agent_supplied_empty_checker_is_review`
  - `test_dropped_checker_is_review`
  - `test_checker_replayed_onto_new_evidence_is_review`
  - `test_checker_signed_by_another_key_is_review`

### 2. MEDIUM: the "no network" sandbox silently fell back to a Python monkey-patch

**Finding.** When `unshare(CLONE_NEWUSER|CLONE_NEWNET)` failed (typical in Docker), the worker ran
with only `socket.socket` patched out, and nothing said so.

**Fix.**
- **Fail closed in the governor.** The governor now reads the **already-signed**
  `sandbox.network_namespace` field. Anything other than `true` (false, missing, wrong type) gives
  REVIEW `sandbox_network_not_isolated` before any rule runs.
- **Explicit policy.** `GOVERNOR_ALLOW_NO_NETNS=1` accepts such evidence. It is off by default and
  warned about at startup when on.
- **Loud fallback.**
  - `extractor/jail.py` writes a WARNING to stderr when unshare is refused.
  - `extractor/sandbox.py` logs a WARNING for every document extracted without a namespace.
- **Tests on restricted hosts.** `tests/conftest.py` sets `GOVERNOR_ALLOW_NO_NETNS=1` for test
  governors only on hosts that cannot create a namespace, and prints that in the pytest header. The
  policy tests pass `allow_no_netns` explicitly.

**Deployment note.** The public sandbox runs in a container on Render. If unshare is refused there,
every PAY becomes REVIEW until the deploy does one of two things:
- allows user namespaces, or
- sets `GOVERNOR_ALLOW_NO_NETNS=1` explicitly, accepting the reduced isolation.

This branch does not touch the `deploy/sandbox` branch.

**Tests.**
- `trust.rs`:
  - `sandbox_without_network_namespace_reviews_by_default` (false, missing, null, the string `"true"`)
  - `sandbox_without_network_namespace_allowed_only_when_configured`
- `tests/test_review_fixes.py`:
  - `test_governor_fails_closed_on_signed_netns_false[False/True]` (real binary, real Extractor signing)
  - `test_jail_applies_rlimits_and_reports_netns` (the warning is printed whenever the flag is 0)
  - `test_jail_netns_blocks_network_when_available`

### 3a. HIGH (Python): `preexec_fn` in a threaded process

**Finding.** `extractor/sandbox.py` used `subprocess.Popen(preexec_fn=...)` inside the
`ThreadingHTTPServer` extraction service, which risks a fork deadlock.

**Fix.** No shipped code uses `preexec_fn` any more.
- The parent does a plain fork+exec of `python -I extractor/jail.py '<json>' -- <tool>` with
  `start_new_session=True`. The netns flag pipe goes through `pass_fds`.
- The fresh, single-threaded wrapper sets RLIMIT_CPU/AS/FSIZE/NOFILE and core 0, attempts unshare,
  reports the flag, then `execv`s the worker.
- With `-I` it imports only the standard library.

**Tests.**
- `test_no_preexec_fn_in_shipped_python` (AST scan of every shipped .py file)
- `test_jail_applies_rlimits_and_reports_netns` (limits, own session)
- `test_extraction_from_many_threads_does_not_hang` (4 concurrent extractions from a thread pool)
- the existing `tests/test_extractor_sandbox.py` hostile-PDF suite (bombs, timeouts, memory)

### 3b. HIGH (Python): `render_pages` ran pdftoppm on attacker PDFs next to BASETEN_API_KEY

**Finding.** The public Fool-the-Shield app renders uploaded PDFs with pdftoppm in the same process
that holds the model API key.

**Fix.** `extractor/line_proposer.py::render_pages` runs pdftoppm through the same jail:
- **Environment:** an explicit, minimal `render_env()` (PATH, HOME, TMPDIR, LANG); nothing is inherited.
- **Limits:** `RENDER_LIMITS` (20 s CPU, 768 MB address space, 32 MB file size, 64 open files, no core dumps).
- **Process:** a wall-clock timeout that kills the whole process group, in its own session.
- **Files:** a private 0700 temp dir as cwd and HOME.
- **Network:** the same user+network namespace attempt, logged when refused.

Any failure returns no images. The model then reads the text layer, which is all the governor ever
verifies against anyway.

**Tests.**
- `test_render_pages_scrubs_secrets_and_applies_limits`: a spy tool records its env and limits.
  BASETEN_API_KEY and GOVERNOR_API_KEY are set in the parent and must not reach it. HOME must be the
  private 0700 dir, and the limits must match exactly.
- `test_render_pages_times_out_and_kills_the_group`
- `test_render_pages_tool_failure_returns_no_images`
- `test_render_pages_real_pdftoppm_still_renders` (skipped without poppler)

### 4. MEDIUM: the nonce store was in memory only (a restart reopened a 300 s replay window)

**Fix.** Schema v3 adds `used_nonces(nonce PRIMARY KEY, issued_at)` to the existing SQLite store.
- `Store::act` records each accepted evidence nonce **in the same transaction** as the decision.
  Replay is refused across restarts and across processes sharing one DB file.
- Rows older than the evidence max age are pruned on each write; that evidence is refused as stale
  anyway.
- A live-row cap fails closed: REVIEW `evidence_nonce_store_full`.
- The in-memory check stays as a first line.
- Eval reset deliberately does not clear used nonces.

**Tests.**
- `governor/tests/review_fixes.rs`:
  - `replay_after_restart_is_still_blocked`
  - `second_process_on_same_db_cannot_replay`
  - `expired_nonces_are_pruned_from_the_db`
- `trust.rs::verify_accept_returns_the_nonce_to_persist`
- `security.rs::v1_database_migrates_to_latest_and_keeps_its_data` (migration)

### 5. `anchor.rs` used `verify` where `trust.rs` uses `verify_strict`

**Fix.** `anchor::check_sig` now uses `verify_strict`, which rejects weak (small-order) keys and
non-canonical signatures.

**Tests.**
- `anchor::tests::weak_key_signature_rejected_by_strict_check`: the identity public key with the
  all-zero-scalar signature. The lenient `verify` accepts this forgery; `check_sig` must not.
- `anchor::tests::genuine_anchor_still_verifies`

## Follow-ups from the review (also fixed here)

### A human-REJECTED invoice became payable again (Limits)

**Problem.** Rejected ledger entries were invisible to every duplicate rule. After a human rejected a
REVIEW, the same invoice could be proposed again and judged as new. Once the 30/90-day look-back
windows had passed, nothing remembered the rejection at all.

**Fix.** A new rule, `previously_rejected` (BLOCK, STRUCTURAL, **no time window**). It fires when a
human-rejected entry that has not been reopened matches the new invoice on any of:
- same vendor and normalised invoice number (any amount),
- same IRN,
- same vendor and the same document text minus the invoice number.

Only an operator or approver can lift it, with `POST /v1/decisions/{id}/reopen` and a reason of 1–500
characters.
- **Maker-checker.** The identity that proposed the payment cannot reopen it. This is enforced in
  code and by a DB trigger.
- **Once, rejected only.** Only a rejected decision can be reopened, and only once (also a DB trigger).
- **Audited, nothing erased.** A reopen appends an audit record (who, why, when) and a row in the
  append-only `rejection_reopens` table. The rejected ledger row itself never changes.

**Tests** (`governor/tests/review_fixes.rs`):
- `rejected_invoice_stays_blocked_beyond_every_window` (20 s, 31 d, 91 d, 400 d, and a different amount)
- `reopen_requires_another_identity_and_is_audited`
- `reopen_survives_restart_and_rejection_without_reopen_too`
- `only_human_rejected_decisions_can_be_reopened`
- `reopen_rows_are_append_only_in_the_db`

### Sandbox caps: README ($5 / 50) vs code defaults ($9 / 100)

The Render deployment runs with a $5 lifetime spend cap (250 model calls at $0.02 a call) and 50 model
calls a day. The code defaults now match:
- `SANDBOX_SPEND_CAP_USD` is 5.00.
- `SANDBOX_MODEL_CALLS_PER_DAY` is 50.

Env overrides still work, and the $9.50 hard ceiling is unchanged. `docs/SANDBOX.md` is updated.

**Test:** `test_sandbox_default_caps_match_the_deployment` (defaults and overrides).

### `master-changes/{id}` role

`GET /v1/master-changes/{id}` requires the operator or approver role. The code was right; the README
now says so.

## Configuration added

| Variable | Default | Meaning |
|---|---|---|
| `GOVERNOR_CHECKER_PUBKEY` | unset (warned; every signed decision gets a REVIEW checker signal) | checker Ed25519 public key(s), comma-separated, 64 hex chars or base64; must differ from the extractor keys |
| `GOVERNOR_CHECKER_UNSIGNED_OK` | `0` | `1` reads an unsigned `checker_raw` (legacy, warned) |
| `GOVERNOR_ALLOW_NO_NETNS` | `0` | `1` accepts evidence extracted without a network namespace (warned) |

New endpoint: `POST /v1/decisions/{id}/reopen` (operator or approver), documented in
`docs/openapi.yaml`.

Schema version 3 adds the `used_nonces` and `rejection_reopens` tables. The migration runs
automatically.
