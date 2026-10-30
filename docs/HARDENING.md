# Production hardening tracker

Started 2026-10-30 18:05 IST at the user's request ("finish all the phases"), from an external code review (Claude). Hackathon deadline is unchanged: Nov 5 07:30 IST. Submission readiness beats any phase: never leave `main` broken, never edit verdict rules after the frozen held-out run.

Coordination: each workstream works in its own git worktree on its own branch, rebases on `main`, runs `cargo test` + `pytest`, then fast-forwards/merges into `main` and pushes. Mark status here when you start (IN PROGRESS + branch) and finish (DONE + commit). Don't edit files another IN PROGRESS stream owns unless needed; rebase and resolve instead.

| Phase | Item | Owner/branch | Status |
|---|---|---|---|
| 1 | Currency on proposal/PO/evidence; REVIEW on mismatch; per-currency thresholds | main | IN PROGRESS (Phase 1 agent, 2026-10-30 18:10 IST) |
| 1 | Round-off rule decided before held-out run (non-tax "Round off" line < ₹1 exempt from charge_not_on_po) | main | IN PROGRESS (Phase 1 agent, 2026-10-30 18:10 IST) |
| 1 | Padded-fee matching: qty × unit price vs PO lines, else REVIEW (no substring match) | main | IN PROGRESS (Phase 1 agent, 2026-10-30 18:10 IST) |
| 1 | ifsc() normalizer, bounded decisions map, auth on /v1/audit/verify | main | IN PROGRESS (Phase 1 agent, 2026-10-30 18:10 IST) |
| 2 | Governor (or signed extraction service) owns evidence; Ed25519-signed evidence verified by governor | hardening/trust | IN PROGRESS (wt-trust, started 18:10 IST) |
| 2 | Hostile-input parsing: sandboxed extractor with CPU/mem/time limits; fuzz tests | hardening/trust | IN PROGRESS (wt-trust, started 18:10 IST) |
| 3 | SQLite (WAL) ledger, audit log, decisions; decision + ledger in one txn | hardening/state | IN PROGRESS (hardening/state, wt-state) |
| 3 | Idempotency keys on /v1/actions; restart/crash-recovery test; concurrent identical requests → one payment | hardening/state | IN PROGRESS (hardening/state, wt-state) |
| 3 | External anchoring: periodic signed head hash | hardening/state | IN PROGRESS (hardening/state, wt-state) |
| 3 | DB uniqueness constraint instead of in-process mutex (documented choice) | hardening/state | IN PROGRESS (hardening/state, wt-state) |
| 4 | Per-identity auth (operator/agent/approver keys), maker-checker on identities, master_change wired with callback, 2 approvers, cooling-off | hardening/security | TODO |
| 4 | TLS option, rate limiting, secrets via env/file, cargo audit / pip-audit / Dependabot | hardening/security | TODO |
| 4 | docs/THREAT_MODEL.md: attacker-controls-X scenarios and what the gate still guarantees | hardening/security | TODO |
| 5 | Frozen held-out set run once (after Phase 1 merge), raw result | build loop | TODO |
| 5 | Blind adaptive cases written without repo access | blind writer | TODO |
| 5 | Real/public anonymized invoices eval + FP rate with confidence intervals | hardening/real-eval | IN PROGRESS (Phase 5 agent, 2026-10-30 18:10 IST) |
| 5 | Internal red-team report; shadow-mode harness (external review + live pilot need real users: documented as next steps) | hardening/real-eval | IN PROGRESS (Phase 5 agent, 2026-10-30 18:10 IST) |
| 6 | GitHub Actions CI, Dockerfile, make demo, pyproject.toml | hardening/ops | IN PROGRESS |
| 6 | Structured logs, /metrics, /healthz, config validation, versioned OpenAPI schema, backup/restore test, runbooks, load test + SLO | hardening/ops | IN PROGRESS |
| 6 | Repo prune (dedupe demos; keep the one the video uses) | hardening/ops | IN PROGRESS |
| 7 | Accounting adapters (Tally XML / Zoho Books API shape) with mock tests; review UI for REVIEW cases; payment-rail adapter (sandbox only with user OK) | hardening/product | TODO |
| 7 | docs/COMPLIANCE.md: DPDP Act notes, retention policy (not legal advice) | hardening/product | TODO |

Honest limits: a weeks-long shadow pilot with real AP staff, an external security review, real customer invoices and legal sign-off can't be produced inside the event. We build the harness/docs for each and say so in the README; we never claim they happened.
