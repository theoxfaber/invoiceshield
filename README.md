# InvoiceShield

> Hackathon submission (Meta Global AI Developer Hackathon, Oct 26 – Nov 4, 2026). **Synthetic data only. No financial or compliance claims.** Work in progress.

An AI agent pays vendor invoices. InvoiceShield sits between the agent and the money: the agent can only say *which vendor, which invoice, how much*. The payee account always comes from your vendor master, never from the document being read, and a fail-closed Rust gate re-checks every field.

Built for Indian small businesses and CA firms hit by vendor bank-detail-change fraud. Applies Meta's own guidance for agents (the "Agents Rule of Two" and the pattern where the model proposes and only a separate policy layer can grant permission) to accounts payable.

## Status
- `governor/`: Rust gate (payee from master, PO ownership and balance, approval threshold and cap, duplicate key with Unicode normalisation and IRN, rolling 7-day per-vendor split detection, issuer binding on GSTIN and sender domain, bank-detail-change tamper alert, escalate-only checker merge, hash-chained audit). `cargo test` in `governor/`.
- `structural/`: GSTIN mod-36 checksum and PAN binding.
- Agent, checker, invoice generator and red-team eval: in progress. See `docs/BUILD_LOG.md`.

Everything here was built during the event; see `docs/BUILD_LOG.md`.
