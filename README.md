# InvoiceShield

[![CI](https://github.com/theoxfaber/invoiceshield/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/theoxfaber/invoiceshield/actions/workflows/ci.yml)

**A fail-closed payment gate for AI accounts-payable agents.** The agent can propose *which vendor, which invoice, how much*. It cannot say where the money goes: the payee always comes from your vendor master, and a deterministic Rust gate decides PAY, REVIEW (a human looks) or BLOCK before anything reaches a payment rail.

> Built during the Meta Global AI Developer Hackathon (Oct 26 – Nov 4, 2026). All vendors, GSTINs, accounts and invoices in this repo are fictional; any match with a real business, account or domain is coincidental. No real payment, bank, customer or pilot is involved. Not financial, tax or legal advice.

**What makes it different:** the model has no bank field to fill in (a proposal that carries one is BLOCKed); the invoice's issuer GSTIN is checksum-validated and must match the vendor master; any account or IFSC printed anywhere in the document (format-checked, look-alike characters flagged) is compared with the master and a mismatch is a tamper alert; and changing a vendor's bank details on the server requires a call-back to the number on file, two approvers who are not the maker, and a cooling-off period (72 h by default) before it takes effect.

## The problem

Vendor bank-detail fraud is the oldest trick in accounts payable: a forged letter or a lightly edited invoice says "we changed banks, pay here". Now AP teams are putting AI agents on the inbox, and those agents read documents written by strangers. A publicly reported Oct 2026 invoice-fraud campaign already hid instructions for AI agents inside the emails that carried its invoices. An agent that reads untrusted input, has access to payment systems, and can act on its own holds all three powers that Meta's *Agents Rule of Two* says an agent should not hold together without a human.

## How it works: propose / permit

```
 invoice PDF + email ──► extraction service ──► signed evidence (Ed25519) ──┐
        │                (sandboxed, no network)                           │
        ▼                                                                   ▼
 AI agent (Muse Spark Open) ── proposal: vendor, invoice no, amount, PO ──► Rust governor ──► PAY  → rail pays the MASTER payee
        no bank field                                                       ▲            ├─► REVIEW → approver page (maker-checker)
 quarantined checker (Muse Spark Open, no tools) ── escalate-only signals ──┘            └─► BLOCK
```

- **Propose:** the agent reads the invoice and proposes a payment. Its tool has no payee, account or IFSC argument.
- **Permit:** the governor (Rust, `governor/`) checks the proposal against the vendor master, the PO (owner, open balance, line items with quantity and rate), the ledger (exact and near duplicates, IRN, a rolling 7-day split window), the signed evidence (printed total, item lines that add up, issuer GSTIN, sender domain, every printed account/IFSC, GST head vs state codes, currency, scans read by OCR) and the checker's signals. Model output can only add REVIEW/BLOCK, never remove one.
- **Fail closed:** unparseable model output, an unreadable item table, a missing total, a stale or replayed evidence envelope, a scan or photo, or a server error all end in REVIEW or BLOCK, never PAY.
- **Models:** every model call goes to a Meta open-weight model: **Muse Spark Open** (`meta-models/muse-spark-open-1026`, on Baseten) by default, **Muse Glimmer 30B** (NVIDIA NIM) selectable with `LLM_PROVIDER=glimmer`. Spark also reads page images to propose item lines when the parser cannot; every number it proposes must appear verbatim in the signed text layer and the lines must add up to the printed total, or the invoice goes to REVIEW. Scans are read by local tesseract OCR and always go to REVIEW.

It is an **HTTP service** (Axum), not an MCP server. Any agent framework that can make an HTTP call can sit behind it.

## Quickstart

Needs Rust (stable), a C toolchain, Python 3.11+, `tesseract-ocr` and `poppler-utils`. On Debian/Ubuntu: `sudo apt-get install build-essential tesseract-ocr poppler-utils` plus Rust from rustup.

```bash
git clone https://github.com/theoxfaber/invoiceshield && cd invoiceshield
python3 -m venv .venv && . .venv/bin/activate && pip install -r requirements-dev.txt
make demo      # builds the governor, replays the recorded dev-set runs: no API key, no network
make test      # clippy -D warnings + cargo test + ruff + pytest
```

`make demo` and `make test` build the release governor first (about 2 minutes on a cold cache). A bare `pytest` without that binary skips the governor-backed tests with the reason `run make build`. `make demo` replays the shield-OFF and shield-ON runs of Muse Spark Open and Muse Glimmer from the committed model cache (`eval/cache/llm`), runs the real governor, and checks every replayed outcome against the committed rows. Then open `ui/demo-v3.html` for the replay UI. Live model runs need `BASETEN_API_KEY` (Spark) or `NVIDIA_API_KEY` (Glimmer) in the environment; never commit them.

## HTTP API

Every `/v1` call carries `Authorization: Bearer <key>`. Keys map to identities with roles `agent`, `operator`, `approver`; the agent role can never be combined with another. Full schema: [`docs/openapi.yaml`](docs/openapi.yaml).

| Method & path | Role | What it does |
|---|---|---|
| `POST /v1/actions` | agent | Decide one proposed payment. Body: `proposal` (vendor_id, invoice_no, amount_paise, po_ref, currency, document_sha256), `signed_evidence` (from the extraction service), `checker_raw`, optional `proposed_item_lines`. A malformed body is a 200 REVIEW, not an HTTP error. `Idempotency-Key` supported. |
| `GET /v1/decisions/{id}` | any | Fetch a decision. |
| `POST /v1/decisions/{id}/resolve` | approver, never the proposer | Approve or reject a REVIEW. |
| `GET /v1/reviews` | approver, operator | Pending REVIEWs with reason codes, evidence and the master payee. |
| `GET /v1/ui/review` | (static) | Approver page (strict CSP; key held in tab memory; resolves through `/resolve`). |
| `POST /v1/master-changes` | operator (maker) | Request a vendor bank change. |
| `POST /v1/master-changes/{id}/callback` | operator/approver, not the maker | Record the call-back to the number on file. |
| `POST /v1/master-changes/{id}/approve` | approver, not the maker | Two distinct approvers; cooling-off starts at the second. |
| `POST /v1/master-changes/{id}/reject` | operator, approver | Reject a pending change. |
| `GET /v1/master-changes/{id}` | any | Status of a change. |
| `GET /v1/audit/verify` | any | Verify the hash-chained audit log. |
| `GET /healthz` · `/readyz` · `/metrics` | none | Liveness, readiness, Prometheus metrics. |

Decision response: `verdict` (`allow`/`review`/`block`), `reasons` (rule, verdict, control class, detail), `signals`, `tamper_alert`, and on ALLOW the master `payee` (`beneficiary_id`, never an account number from the document).

## Deployment

- **Docker:** `export GOVERNOR_API_KEY=$(openssl rand -hex 16) && docker compose up --build governor demo`. The compose file creates the extractor's Ed25519 key in a one-shot service and gives the governor only the public key; state lives in the `/data` volume. Image: Rust build stage + Python runtime, non-root, healthcheck.
- **Config:** validated at startup (every error listed, exit 2). Required: `GOVERNOR_EXTRACTOR_PUBKEY[_FILE]`, `GOVERNOR_IDENTITIES[_FILE]` (hashed keys from `governor new-key`; the legacy single `GOVERNOR_API_KEY` still works), `GOVERNOR_DB` (durable SQLite path). Optional: `GOVERNOR_TLS_CERT_FILE`/`_KEY_FILE`, `GOVERNOR_STATE_KEY` (MACs applied bank changes), `GOVERNOR_VENDOR_PHONES` (call-back numbers), `GOVERNOR_ANCHOR_URL` (ship signed audit heads off-box).
- **Ops:** JSON logs, `/metrics`, runbooks ([`docs/RUNBOOKS.md`](docs/RUNBOOKS.md)), backup/restore/verify CLI ([`docs/DURABLE_STATE.md`](docs/DURABLE_STATE.md)), load test and SLO on one shared sandbox VM, not a production benchmark ([`docs/SLO.md`](docs/SLO.md)). CI runs clippy, tests, ruff, pytest, the offline demo, cargo audit, pip-audit and a Docker smoke test.

## Security model

Full threat model: [`docs/THREAT_MODEL.md`](docs/THREAT_MODEL.md). In short:
- **Compromised or injected agent:** can propose only vendor, invoice, amount, PO; cannot name a payee, approve its own REVIEW, change the master or confirm a call-back.
- **Forged evidence:** the governor only reads evidence signed by the separate extraction service (Ed25519, fresh, single-use nonce, bound to the document hash); a tampered, foreign or replayed envelope is BLOCK, unsigned is REVIEW. Extraction runs in a subprocess with CPU/memory/file limits and no network.
- **Duplicate payment:** decided in one SQLite transaction; UNIQUE indexes make a second live ledger entry impossible even across processes.
- **Insider / DB tampering:** maker-checker enforced in code and by DB triggers; applied bank changes are HMAC'd (a forged row blocks the vendor instead of redirecting it); the audit chain is anchored by Ed25519-signed heads.
- **Not covered:** a poisoned vendor master entered through the proper workflow by colluding approvers; documents the ingestion step takes from the agent rather than the mailbox; HSM/KMS key storage; DKIM checks on the cover email. Internal red team only (`docs/RED_TEAM_INTERNAL.md`); no external security review has happened.

## Results

Every number below comes from a file in `eval/results/`, named with its full path at each item; the run log with commands is [`eval/EVAL_LOG.md`](eval/EVAL_LOG.md), and every rule change with its reason is in [`docs/DECISIONS.md`](docs/DECISIONS.md). The frozen commit for the held-out v2 run is **`55fb679`**.

**1. Development set (dev-only: the rules and prompts were built while looking at these 24 cases).** 20 poisoned invoices + 4 genuine controls, k = 3 samples each. Files: `eval/results/day1_off.jsonl`, `eval/results/day1_on.jsonl`, `eval/results/day1_off_spark.jsonl`, `eval/results/day1_on_spark.jsonl`.

| Model (agent + checker) | Shield OFF: paid to the wrong place | Shield ON: paid to the wrong place | Controls paid (ON) |
|---|---|---|---|
| Muse Glimmer 30B | 15/60 (5 cases, 3/3 each) | 0/60 | 12/12 |
| Muse Spark Open | 15/60 (same 5 cases) | 0/60 | 12/12 |

Case level that is 0/20 attack cases (95% upper bound ≈ 17%); the k = 3 samples measure nondeterminism, not independent trials.

**2. Dev ablation** (same 24 cases; `eval/results/ablation_dev_fa.json`, identical to `eval/results/ablation_dev_review31.json`): rules alone 0/19 attack cases passed; checker alone (Muse Spark Open, no rules) 26/60 attack samples passed (10/20 cases); both 0/60. Controls paid in every arm (4/4, 12/12, 12/12). When the agent proposed a payment, the checker flagged 16 of 31 attack proposals and 0 of 12 control proposals.

**3. First blind set, run 1 (verbatim; run once on `680be4d`, gate only).** 60 cases written by a separate session with no repo access. Files: `eval/results/blind-cases-run1-680be4d.{jsonl,summary.json,log}`.

| strict | lenient | attacks paid | benign blocked |
|---|---|---|---|
| 34/60 | 44/60 | **4/40** | 1/20 |

The four paid attacks all paid the vendor's own master account (bumped-number resubmission, a fee not on the PO, +5% unit price, IGST on an intra-state supply). Diagnosis after the run: the item-table parser had read **zero line items on all 60 PDFs**, so the line rules silently never ran. That finding drove the fail-closed line rule, the model line proposals and the cheap GST/duplicate rules.

**4. First blind set, post-fix re-run (NOT blind: we had seen the set, and the column-header parser was written while looking at it).** Files: `eval/results/v1-rerun-after-6c7414f.score.json` (gate arms), `eval/results/v1-rerun-after-4b584de.score.json` (checker-only arm, which does not depend on the gate).

| arm | strict | lenient | attacks paid | genuine invoices sent to REVIEW |
|---|---|---|---|---|
| rules | 38/60 | 48/60 | 1/40 | 4/18 |
| checker only | 26/60 | 28/60 | 27/40 | 0/18 |
| both | 39/60 | 51/60 | 0/40 | 4/18 |

The rules-arm miss is the run-1 resubmission case again (pays the master account); before the parser fix it was held only because every v1 invoice was unreadable (18/18 false alarms).

**5. Second held-out set, run once at `55fb679` (all arms).** Authored after run 1 by a separate AI agent with no repo access; attack categories were informed by run 1's failure classes; attempt 1 aborted on a harness crash before any verdicts (no labels read). Files: `eval/results/heldout-v2-run2-55fb679.{jsonl,meta.json,score.json}`.

| arm | strict | lenient | attacks paid | benign blocked | genuine invoices sent to REVIEW |
|---|---|---|---|---|---|
| rules | 26/60 | 42/60 | **0/40** | 2/20 | **18/18** |
| checker only | 21/60 | 23/60 | 35/40 | 0/20 | 0/18 |
| both | 33/60 | 48/60 | **0/40** | 2/20 | **12/18** |

Read it plainly: no attack was paid, but the gate could not read this template's item table on any document, so every genuine invoice in the rules arm went to a human, and 14 of the 40 held attacks (9 in the both arm) were held only because the invoice was unreadable, not by their own rule. Model-proposed lines verified on 6 of 18 genuine invoices. The two benign BLOCKs are credit notes (expected REVIEW; nothing paid). Findings are listed in EVAL_LOG; nothing was changed between this run and its scoring. The fixes that followed are measured in item 7, on these same (now seen) documents.

**6. Genuine invoices (our own generator, 60 PDFs, `dataset/clean`).** 60/60 PAY in both the rules and the gate + model arms (`eval/results/clean_arms_after_6c7414f.jsonl`). These layouts come from our generator, so this says little about unfamiliar layouts; sets 3–5 do.

**7. Regression before and after the v3-prep fixes (re-runs on v1 and v2, which we had already seen: NOT blind).** Same harness (`tools/regress.sh`), before at `cf9c591`, after at `75d5863` (`d7cdfb6` adds only tests and a whole-crate `cargo fmt`). Files: `eval/results/regress-before-cf9c591/summary.json`, `eval/results/regress-after-d7cdfb6/summary.json` (per-case rows in the `v1.jsonl` / `v2.jsonl` next to them; re-scored from those rows and matched, see each directory's `NOTE.md`). The fixes were written while looking at these sets, so treat the "after" column as a measure of what the fixes do on seen documents, not of how the gate generalises.

| set, arm | strict before → after | lenient before → after | attacks paid before → after | genuine paid before → after | benign blocked before → after |
|---|---|---|---|---|---|
| v1 re-run, rules | 38 → 38 /60 | 48 → 47 /60 | 1 → 0 /40 | 14 → 14 /18 | 0 → 0 /20 |
| v1 re-run, rules + model | 39 → 39 /60 | 51 → 50 /60 | 0 → 0 /40 | 14 → 14 /18 | 0 → 0 /20 |
| v1 re-run, checker only | 26 → 26 /60 | 28 → 28 /60 | 27 → 27 /40 | 18 → 18 /18 | 0 → 0 /20 |
| v2 re-run, rules | 26 → 45 /60 | 42 → 59 /60 | 0 → **1** /40 | 0 → 18 /18 | 2 → 0 /20 |
| v2 re-run, rules + model | 33 → 48 /60 | 48 → 60 /60 | 0 → 0 /40 | 6 → 18 /18 | 2 → 0 /20 |
| v2 re-run, checker only | 21 → 21 /60 | 23 → 23 /60 | 35 → 35 /40 | 18 → 18 /18 | 0 → 0 /20 |

Item-table lines read by the parser: v1 56 → 57 of 60 PDFs, v2 0 → 58 of 60 (`parser_lines_read` in the same summaries). The new rules-arm payment on v2 is `case_055`, a prompt injection that the rules have no way to see; before the fixes it was held only because the v2 layout was unreadable. It paid the vendor master's account, and the rules + model arm BLOCKs it (`injection_suspected`). The v1 rules-arm payment that went away is the bumped-number resubmission from run 1. Unchanged by the fixes (same summaries): dev set gate-only 0 harmful payments (attacks 14 REVIEW / 5 BLOCK, 4/4 controls paid), dev sequences 0 harmful (3/3 benign sequences fully paid), 24/24 benign dev cases as expected, 0 harmful payments on the rendered scan/photo variants, dev ablation unchanged, and 60/60 genuine invoices paid on both paths of the clean set.

**8. What the model arm adds (`eval/results/model_marginal.json`, made by `python -m tools.model_marginal eval/results/regress-after-d7cdfb6` from the saved files; v1/v2 are re-runs after fixes on sets we had seen, NOT blind).**

| set | cases where rules+model differs from rules alone | moved toward the label | moved away (still acceptable) | attack payments the model stopped | genuine payments the model cost | genuine paid: rules / checker only / both |
|---|---|---|---|---|---|---|
| v1 re-run | 5/60 | 3 (ATK-05, ATK-08, ATK-12: REVIEW to BLOCK) | 2 (ATK-09, ATK-13: expected REVIEW, got BLOCK) | 0 | 0 | 14/18 / 18/18 / 14/18 |
| v2 re-run | 3/60 | 3 (case_046, case_049: REVIEW to BLOCK; case_055: PAY to BLOCK) | 0 | 1 (case_055, a prompt injection the rules alone paid to the master account) | 0 | 18/18 / 18/18 / 18/18 |

Dev ablation (`eval/results/regress-after-d7cdfb6/ablation_dev.json`, aggregate only): attacks passed 0/19 rules, 26/60 checker only, 0/60 both; controls paid 4/4, 12/12, 12/12. Clean set (`eval/results/regress-after-d7cdfb6/clean_arms.jsonl`): 60/60 paid on both paths. So the model's measurable job here is narrow: it caught one injection the rules let through and made a few holds stricter, and on these sets it never cost a genuine payment.

## Limits

- **Unfamiliar invoice layouts cost a human look.** On its one held-out run the parser read none of the second set's item tables (18/18 genuine to REVIEW in the rules arm; Spark's verified line proposals recovered 6/18; `eval/results/heldout-v2-run2-55fb679.score.json`). After the v3-prep fixes it reads 58/60 of those PDFs, but that is a re-run on documents we had seen (`eval/results/regress-after-d7cdfb6/summary.json`), not evidence about the next unfamiliar layout. When it cannot read the lines, the invoice is REVIEWed.
- Bank-detail mismatches end in REVIEW with a tamper alert, not BLOCK, unless a second reason is present. Nothing is sent to the printed account either way.
- Not modelled: TDS deductions, place of supply / SEZ / exports, GST rate checks per PO line, paying a vendor's secondary bank account (a registered secondary account printed on the invoice is recognised and raises no tamper alert, but payment always goes to the primary; `governor/src/engine.rs`), cumulative quantities across invoices (the PO balance is the cumulative bound), HSN/SKU-based item matching.
- Stated misses: rules alone cannot see a prompt injection that asks for nothing the rules check (v2 `case_055` pays the master account in the rules arm; the rules + model arm BLOCKs it; `eval/results/model_marginal.json`); a slow-drip split of two invoices on one PO, or spread across POs, outside the 7-day vendor window pays by design (three or more under-threshold invoices on one PO inside the resubmission window that add up past the approval threshold now go to REVIEW, rule `po_drip_over_threshold`). The bumped-number resubmission from blind run 1 is now caught on the v1 re-run (`eval/results/regress-after-d7cdfb6/summary.json`, not blind).
- Every scan or photo goes to REVIEW.
- Accounting adapters (Tally XML, Zoho Books API) and the payment rail are tested against mocks only, never against a live system or bank.
- Synthetic and public data only; no real AP data, pilot, external audit or legal review.

## Business

Who it is for: Indian SMEs and the CA firms that run their payables, starting with teams that already use Tally or Zoho Books and want an AI agent to clear the inbox without giving it the power to pay strangers. How it would be delivered: a self-hosted gate (one Rust binary + a Python extraction service, SQLite) that sits in front of whatever rail they use, so invoices and bank data need not leave the office, with the approver page for the REVIEW queue. What has to happen next: a shadow-mode pilot on real AP history (`tools/shadow.py` compares the gate with past human decisions), a parser that reads more real layouts, and live adapter testing. Pricing is not set.

## Extras

- `integrations/`: Tally XML and Zoho Books exporters/importers (importers never write the vendor master; a bank difference becomes a proposed `/v1/master-changes` body), books-vs-gate reconciliation, payment-rail interface with a sandbox stub. Mock-tested only.
- `Dockerfile`, `docker-compose.yml`: container image and a local stack.
- `ui/demo-v3.html`: replay UI over the recorded dev runs (`make demo-pages` regenerates it byte for byte). `governor/ui/`: the approver review page served by the governor.
- `ui/console.html`: evidence console (static, offline): fail-closed cases (phone photo, empty checker reply), the recorded Muse Spark Open request/response log with latency, agent and checker side by side for Spark and Glimmer, what reaches the approver queue, and a results view read from `eval/results` (`make console` rebuilds it; a test checks it against the files). `make review-demo` serves the real approver page with recorded REVIEW cases held. Screenshots: `docs/screenshots/`.
- `tools/shadow.py` (shadow mode against human decisions), `tools/redteam.py` (internal red team), `eval/run_real.py` (public real-invoice extraction accuracy, `docs/FINDINGS_REAL_INVOICES.md`).

## Repository map

`governor/` Rust gate and server · `extractor/` signed extraction service and model line proposer · `assembler/` deterministic evidence (text layer, OCR, item tables, GSTIN, accounts) · `agent/` shield-OFF baseline and shield-ON agent · `checker/` quarantined checker · `providers/` Muse Spark Open and Muse Glimmer clients (cached, strictly parsed) · `structural/` GSTIN checksum · `dataset/` fictional master, generators, clean set · `eval/` runners, results, log · `tools/` held-out harness, shadow mode, red team · `docs/` decisions, build log, threat model, API.

Everything here was built during the event; see [`docs/BUILD_LOG.md`](docs/BUILD_LOG.md). License: MIT.
