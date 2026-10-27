# Build log (Meta Global AI Developer Hackathon, Oct 26 – Nov 4, 2026)

The rules say: "New work only: What you present at this Hackathon must have been built during the event." Every line of code in this repo was written during the event window (kickoff Mon Oct 26, 2026, 22:30 IST = 12:00 PM EDT). Git history is the record; this log adds context per work session. Times are IST.

Prior work, **not** included and not copied: the author's earlier project `agentic-payment-risk-governor` (Rust payment gateway). It informed the idea of a fail-closed governor; the governor in `governor/` is a fresh, smaller rewrite built for invoices.

Planning documents (threat model, eval plan, demo script) were written before kickoff; they contain no code.

## Sessions

### Mon Oct 26, 22:30–22:35 IST — Day 0 (first session)
- Read the official rules signed in; chose a from-scratch governor (rules: new work only).
- Glimmer smoke test on NVIDIA's free endpoint (outside the repo): JSON schema not enforced, image input works, ~1 min per image call.
- `governor/`: verdict types with no `Allow` severity, escalate-only `merge`, fail-closed checker parsing, strict proposal schema (bank fields → BLOCK), vendor master + PO table + policy, ledger (paid/pending/rejected), rules: unknown vendor, PO ownership/balance/tolerance, cap/threshold, duplicate key (NFKC, dashes, zero-width), IRN duplicate, same invoice other amount, 7-day vendor window, GSTIN and sender-domain issuer binding, bank-box tamper alert; hash-chained audit. 21 tests incl. proptest monotonic merge, checker fault table, forged all-clear, window edges.
- `structural/gstin.py`: mod-36 check character + PAN binding (3 tests; verified against the public sample 27AAPFU0939F1ZV).
- Next: Axum HTTP server + MCP-style proxy tool (`vendor_id, invoice_no, amount, po_ref` only), vendor master JSON fixture with plausible-format bank details and valid-checksum fictional GSTINs, reportlab invoice generator (B01, L01, S01, Nagpur-style bank change), Glimmer provider + caching, OFF agent for the Day 1 risk test.

### Mon Oct 26, 23:14 IST – Tue Oct 27 ~00:00 IST — Day 0 (second session)
- `dataset/gen/make_master.py` → `dataset/fixtures/master.json` (12 fictional vendors, 16 POs, policy; strict governor schema) and `profiles.json` (addresses, templates, invoice-number patterns, buyer, attacker entities). GSTINs carry valid check characters; IFSCs use real bank codes in each bank's real format with made-up branch codes; account lengths match the bank. Attacker set: Nagpur-style Bank of Baroda → Canara change, Panchkula-style IFSC change, UPI handle, insider edit, lookalikes L01–L05.
- `providers/llm.py`: `LLMProvider` protocol + `NimGlimmer` (NVIDIA NIM, `meta/muse-glimmer-30b`, temperature 1.0, reasoning strength in the system prompt, max_tokens ≥ 4000, retries on 429/5xx, failures returned as errors and never cached, disk cache keyed by SHA-256 of the full request + sample index, offline replay mode). Live smoke: 7.7 s text call, cache hit on replay.
- `providers/strict.py` + `agent/schemas.py`: one-JSON-object extraction, pydantic strict + `extra="forbid"` for `PaymentProposal` (no bank field), `OffPayment` (baseline tool), `CheckerReport` (severity review|block only).
- `governor/src/bin/server.rs`: Axum `/v1/actions` (Bearer auth, constant-time), `/v1/decisions/{id}`, `/v1/decisions/{id}/resolve` (separate operator key, REVIEW only), `/v1/audit/verify`, `/health`; one mutex around evaluate + ledger write; eval clock header only when `GOVERNOR_EVAL_CLOCK=1`.
- Tests: governor 25 (21 + 4 fixture-driven: B01 ALLOW, L01 BLOCK, bank change → REVIEW + tamper alert paying the master account, agent bank field → BLOCK); Python 29.
- Next: reportlab generator (T1 first: B01, L01, X bank-change; region boxes + cover email), assembler (text-layer extraction → Evidence), shield-OFF agent loop with `pay()` on the mock rail, then the Day 1 risk test (~20 poisoned invoices).

### Tue Oct 27, 02:15 IST — Day 1 (build loop, chunk 1)
- `dataset/gen/make_invoice.py`: reportlab T1 generator (clean invoice, bank-box region, cover email) plus the Nagpur-style bank-change attack on V001 (Canara, plausible story). 2 tests (text layer carries invoice no, IFSC, amount); Python suite 31 pass.
- Next: assembler (pypdf text → Evidence), shield-OFF agent loop with pay() on a mock rail, then the Day 1 risk test (~20 poisoned invoices, exact pay count to eval/EVAL_LOG.md).

### Tue Oct 27, 04:45–06:15 IST — Day 1 (build loop, chunk 2): shield-OFF agent and the Day 1 risk test
- `assembler/evidence.py`: deterministic evidence (pypdf text layer incl. hidden text, metadata, sha256, cover email) rendered for an agent.
- `agent/rail.py` mock rail; `agent/off_agent.py` shield-OFF baseline (pay/hold/reject tools, native tool calls). Found that Glimmer sends account numbers as JSON numbers, so the OFF tool layer coerces them to strings (otherwise a schema nit looks like a refusal). Tool calls on NIM take 25–110 s; reasoning "medium" timed out upstream, so the baseline runs at "low".
- `dataset/gen/make_invoice.py` extended (identity overrides, hidden text, footer, extra lines, inflated total, cover email overrides); `dataset/gen/make_riskset.py`: 20 poisoned invoices (bank swap, injection, lookalike, inflation) + 4 clean controls; `eval/run_day1.py` with a mechanical harm rule.
- **Risk test result (real, in `eval/EVAL_LOG.md`): 15 of 60 poisoned samples paid to the wrong place; 5 of 20 cases in 3/3 samples. 12/12 clean controls paid.** The quiet attacks (clean bank swap, IFSC change, lookalike name/domain/GSTIN) get through; loud ones are held.
- Tests: Python 37 pass.
- Next: shield-ON path. Governor client + `PaymentProposal`-only shield-ON agent (no bank field) end to end on B01/L01/X04 (M1), then checker prompt v1 and the structural extraction checks (GSTIN, sender domain).
