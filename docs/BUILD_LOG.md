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

### Tue Oct 27, 08:29 IST: Day 1 (build loop, chunk 3): shield-ON path, quarantined checker, M1
- `assembler/extract.py`: deterministic evidence for the governor (seller GSTIN with the buyer's own skipped, sender domain, the bank box, and **every** account-/IFSC-shaped value anywhere in the text, plus the printed "Total Payable"). Rust `Evidence` gained `all_printed_accounts` / `all_printed_ifscs`: a second account pasted outside the bank box (the injection pattern) now counts as bank details differing from the master, so it is a tamper alert → REVIEW. Governor tests 25 → 28 (pasted-account tamper, matching values stay ALLOW, eval-only `reset_state`).
- `agent/on_agent.py`: same AP agent with `propose_payment(vendor_id, invoice_no, amount_paise, po_ref)`; no bank field; whatever the model sends (minus free-text reason) goes to the governor unfiltered, so an extra bank key is a BLOCK decided by Rust, not by Python.
- `checker/checker.py` (prompt v1): separate Glimmer call, no tools, does not see the proposal; strict-parsed; output converted to `{"signals":[…]}` or, if it fails the schema, the raw text goes to the governor's own fail-closed parser (→ REVIEW). Tests cover garbage, "allow" severity, empty output.
- `shield/`: governor process + HTTP client (eval clock and `/v1/eval/reset` only under `GOVERNOR_EVAL_CLOCK=1`; sandbox proxy env vars broke localhost calls, fixed with `trust_env=False`), pipeline agent + checker → governor → rail with beneficiary-only payout. `eval/run_day1_on.py` and `eval/run_gate_only.py` (hijacked agent + blind checker, no model calls).
- **M1 reached:** B01 → ALLOW and paid to the master account; X04 (Nagpur-style bank swap) → REVIEW with tamper alert, nothing sent; L01 → BLOCK on GSTIN + sender domain when the agent is hijacked (the ON agent itself held it).
- **Day 1 shield-ON result (real, `eval/EVAL_LOG.md`):** 0 of 60 poisoned samples paid wrongly (OFF: 15/60); 12/12 clean controls paid. Governor BLOCK 12, REVIEW 6, agent held 42. Hijacked-agent arm: 0/19 wrong-account pays, **one miss: I02 (fee padding inside the PO balance) is ALLOWed**. Dev set, k=3, one run, no prompt tuning.
- Tests: Python 45, Rust 28.
- Next: close I02 (PO expected-amount / line-item rule), the Rust-side `printed_total` vs proposal check, then vision path for scans (page render → Glimmer image view), T2/T3 templates, duplicate/split-payment cases through the ledger, and the frozen 77-case set generator.

### Tue Oct 27, 11:14 IST: Day 1 (build loop, chunk 4): close I02, amount rules, sequence cases
- Governor: three EXTRACTION rules, each REVIEW-only. `amount_differs_from_printed_total` (proposal ≠ the invoice's "Total Payable"), `invoice_does_not_add_up` (item lines ≠ printed total), `charge_not_on_po` (a non-tax line that names nothing the PO contracts for). `PurchaseOrder.contracted_items` added to the master (generator fills it from PO and vendor category). `assembler/extract.py` now returns `printed_total_paise` and `item_lines` (tax rows flagged). Governor tests 28 → 33; Python 51.
- I02 (fee padding inside the PO balance) was the one hijacked-agent miss; it is now REVIEW. It was found on the dev set and then fixed, so it is logged as a regression, not a held-out result. Hijacked-agent arm: 0/19 harmful ALLOWs, controls 4/4 ALLOW. ON arm replayed from cache: unchanged (nothing paid wrongly, controls paid).
- `dataset/gen/make_sequences.py` + `eval/run_sequences.py`: 16 attack sequences (6 duplicates, 5 splits, 5 wrong-vendor) and 3 benign, real PDFs through the real governor with a simulated clock. Headline 15/15 contained; the slow-drip split (S05) is paid in full by design and reported as a stated limit; benign 3/3 allowed.
- Known gap: the printed-total rule assumes INR; the USD vendor (V003, T3 template) needs a currency field before that template is generated.
- Next: vision path for scans/photos (page render → Glimmer image view, text-less GSTIN → REVIEW is the fallback), T2/T3 templates (incl. the USD invoice), then the frozen 77-case generator and the single-run adaptive set.

### Tue Oct 27, 14:15 IST: Day 1 (build loop, chunk 5): USD vendor template
- `make_t1` takes a currency (defaults from the vendor profile): V003 now renders as a USD invoice (no GSTIN, no GST row, SWIFT instead of IFSC, "Amount (USD)"). `assembler/extract.py` reads USD/$ prefixes on Total Payable and item rows, closing the INR-only gap noted in chunk 4. Amounts stay in minor units (cents), so the Rust amount rules apply unchanged. Test added; Python 52 pass, Rust green.
- Next: vision path for scans/photos (page render → Glimmer image view; text-less GSTIN → REVIEW fallback), scan/photo variants of T1, then the frozen 77-case generator and the single-run adaptive set.

### Tue Oct 27, 17:30 IST: Day 1 (build loop, chunk 6): scan path (local OCR)
- `assembler/scan.py`: `make_scan` rasterises a generated invoice to an image-only PDF (grayscale, slight blur); `ocr_text` runs pdftoppm + tesseract locally. `assemble()` falls back to OCR when the text layer is empty and records `text_source: ocr` in metadata. Clean T1 scan round-trips: GSTIN, bank box and printed total all extract identically. Tests 54 pass.
- Open point: OCR'd evidence still reports `has_text_layer: true` to the governor; decide whether OCR-sourced GSTIN should force REVIEW (design says text-less GSTIN → REVIEW). Vision (Glimmer image view) not started; OCR is the deterministic baseline.
- Next: pass `text_source` to the governor (OCR + noisy photo → REVIEW rule), photo variants (rotation/noise), then frozen 77-case generator and single-run adaptive set.

### Tue Oct 27, 20:00 IST: Go/no-go
- Decision recorded in `docs/DECISIONS.md`: GO on Plan B with Muse Glimmer 30B (NIM, cached replay) and the template-crop + OCR segmenter; SAM 3.1 only if GPU credits arrive. Live Glimmer text call re-checked at 19:4x IST (21.8 s).
- Environment note: this sandbox session had no `cargo` on PATH and was missing Python deps (httpx was reinstalled; reportlab/pypdf were not). Reinstall before running tests.
- Next (unchanged): `text_source` to the governor (OCR-sourced GSTIN → REVIEW), photo variants, frozen 77-case generator, blind K set by Oct 29 evening.

### Tue Oct 27, 20:40 IST: Day 1 (build loop, chunk 7): OCR source reaches the governor
- `Evidence.text_source` (Rust) fed from the assembler; `"ocr"` → REVIEW reason `evidence_read_by_ocr` (EXTRACTION), so a scan can never be auto-paid on OCR-read fields alone. Python 55 tests pass (the governor release binary must be rebuilt after Rust changes: `cargo build --release`, shield client uses it).
- Environment note: cc/mold also missing after reset; reinstall build-essential.
- Next: photo variants (rotation/noise) of scans, frozen 77-case generator, blind K set by Oct 29 evening.

### Tue Oct 27, 23:30 IST: Day 1 (build loop, chunk 8): photo variant
- `assembler/scan.py::make_photo`: rotation, uneven lighting, noise, blur on top of the scan path (stand-in for a phone photo). Goes through OCR and so reaches the governor as `text_source: ocr` → REVIEW. Test added; Python 56 pass.
- Next: frozen 77-case generator (mix of T1/T2/T3, scan, photo), blind K set by Oct 29 evening.

### Wed Oct 28, 02:40 IST: Day 2 (build loop, chunk 9): benign dev set
- `dataset/gen/make_benign.py` + `eval/run_benign.py`: 24 legitimate invoices (16 text PDFs over all 12 vendors/PO pairs incl. USD V003, 4 scans, 4 photos) through the real governor with a clear checker. Result: 24/24 as expected (16 ALLOW, 8 scan/photo REVIEW via `evidence_read_by_ocr`), 0 false BLOCK. Dev set, not the frozen held-out set. Python 56 tests still pass.
- Friction note: every scan/photo goes to REVIEW by design, so friction on image invoices is 8/8; report it openly.
- Next: frozen 77-case generator + manifest hash, blind K set by Oct 29 evening, T2 template.

### Wed Oct 28, 05:20 IST: Day 2 (build loop, chunk 10): T2 layout
- `make_t1(..., layout="t2")`: bank block moves to a top-right "Pay To" panel (different region box), same labels. Extraction, printed total and bank fields still read identically (test added; Python 57 pass). Gives the frozen set a second layout so extraction isn't tuned to one template.
- Next: frozen 77-case generator + manifest hash (mix T1/T2/USD/scan/photo), blind K set by Oct 29 evening.

### Wed Oct 28, 08:45 IST: Day 2 (build loop, chunk 11): attack variants
- `dataset/gen/make_variants.py`: every risk-set attack re-rendered as T2 layout and as a T1 scan (24 base → 48 variant cases), ground truth inherited, SHA-256 manifest hash for freezing. Test added; Python 58 pass. Not yet run through OFF/ON (dev material).
- Next: run variants through gate-only + shield ON, log counts in EVAL_LOG; then assemble the 77-case frozen set (X/K/R rows) and blind K set by Oct 29 evening.

### Wed Oct 28, 11:30 IST: Day 2 (build loop, chunk 12): variants through the gate
- `eval.run_gate_only variants` runs the 48 variants through the real governor (hijacked agent, blind checker): 0 harmful ALLOWs; controls 4 ALLOW / 4 REVIEW (scans). Numbers in EVAL_LOG.
- Next: shield-ON run on variants with Glimmer (cached), then assemble the 77-case frozen set and blind K set by Oct 29 evening.

### Wed Oct 28, 14:30 IST: Day 2 (build loop, chunk 13): photo variant of attacks
- `make_variants` now also renders each attack as a phone-photo variant (24 base → 72 variant cases, T2 / scan / photo). Test updated. Not yet run through the gate (dev material).
- Next: gate-only + shield-ON runs on the photo variants, then assemble the 77-case frozen set and blind K set by Oct 29 evening.

### Wed Oct 28, 23:10 IST: Priority 1 (build loop, chunk 14): demo UI v1
- `ui/build_demo.py` + `ui/demo.template.html` → self-contained `ui/demo.html` (`python3 -m ui.build_demo`). Replays recorded Glimmer sample-0 runs from eval/results (no model calls, never waits): five cases (X04, L02, L03, X_ifsc, genuine B01), invoice image left, shield-OFF run centre (pay call, red "PAID wrong account"), shield-ON run right (agent proposes, checker, governor reasons in plain words, REVIEW/BLOCK). `?case=L02` deep-links; Replay button restarts the animation.
- Not yet visually checked: no browser in this sandbox; JS syntax checked only. Dev-set material, labelled as such on the page.
- Next: open it with the browser agent to screenshot/fix layout, then screen-record per demo-script.md (priority 2); add an I04/hidden-text case once OFF paid arm exists.

### Thu Oct 29, 02:30 IST: Priority 1 (build loop, chunk 15): demo UI visually checked
- Headless Chromium (playwright, /usr/bin/chromium) renders `ui/demo.html` for X04 and L02 with no page errors; layout reads cleanly at 1600x900 (invoice, OFF red "PAID wrong account", ON governor reason + REVIEW). `ui/screenshot_check.py` does this and can drive the screen capture.
- Next: record the real screen capture (playwright record_video) per demo-script.md, add narration captions; add I04/hidden-text case.
