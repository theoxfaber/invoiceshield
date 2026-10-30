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

### Thu Oct 29, 04:30–05:30 IST: review fixes (outside review 7.5/10), Muse Spark Open provider
- Repo hygiene: manifests now store PDF paths relative to the repo root (`repo_relative` in `dataset/gen/make_invoice.py`); no absolute sandbox path remains in the tree. The generator uses reportlab `invariant=1`, so regenerated PDFs are byte-identical (regenerating `eval/riskset/` changed no extracted text and no case field). `.gitignore` now commits `eval/riskset/` and `eval/cache/llm/` (model responses only, no keys) so the recorded numbers replay without an API key; `eval/variants/` stays ignored (13 MB, regenerable). Root-only planning copies (`/BUILD_LOG.md` etc.) are anchored so they no longer shadow `docs/`.
- Verified from a clean copy (no `.git`, `target`, caches or variants; fresh venv from `requirements.txt`): `cargo test` 37 passed (33 + 4 new); `pytest` 58 passed (57 in 25 s + `test_variants.py` in 201 s); offline replay with no `NVIDIA_API_KEY` reproduces Glimmer OFF 15/60 harmful, ON 0/60, controls 12/12. One extra provider test added afterwards (59 Python tests).
- `governor/src/master_change.rs`: vendor bank-change rule (call-back to the number on file, two distinct approvers other than the maker, 72 h cooling-off, audit entry), 4 tests. Library only, not on the HTTP server. `docs/VENDOR_MASTER_CHANGES.md` says what is implemented vs designed.
- `providers/llm.py`: `BasetenMuseSpark` (Muse Spark Open, `meta-models/muse-spark-open-1026`, Baseten OpenAI-compatible endpoint; native tool calls work; no Glimmer reasoning line; same temperature 1.0, cache and fail-closed handling) and `get_provider()` (`LLM_PROVIDER` / `--provider`, default `spark`). Spark result files get a `_spark` suffix; Glimmer files keep their names.
- **Dev set on Muse Spark Open (live, Oct 29 ~04:40 IST, k=3, no prompt changes):** OFF 15/60 samples paid to the wrong place (X04, X_ifsc, L01, L02, L03, each 3/3, the same five cases as Glimmer), 7 no-decision (model answered in text without a tool call), controls 12/12 paid. ON 0/60 wrong-place pays, governor BLOCK 18 / REVIEW 10, agent held 29, agent no-decision 3, controls 12/12 paid; checker returned empty output twice (→ REVIEW, fail closed). Numbers in `eval/EVAL_LOG.md`.
- Frozen 77-case held-out set: not run. Only the spec exists; cases are not generated and the K set must be written blind by a separate author, so it was not generated in this session.
- Next: frozen set generation (non-K rows) + blind K author brief, persistent ledger, 60-second demo recording.

### Thu Oct 29, 05:40 IST: Priority 2 (build loop, chunk 16): screen-capture recorder
- `ui/record_demo.py`: records a real Chromium render of `ui/demo.html` through five cases (X04, L02, L03, X_ifsc, B01), 1600x900 webm, ~70 s, 9 MB (not committed). Needs `python3 -m playwright install ffmpeg`. Raw capture only: no captions or voice-over yet; replays recorded runs, labelled dev set.
- Next: caption overlays per demo-script.md, trim to 3:00 with narration, add I04 hidden-text case.

### Thu Oct 29, 08:30 IST: Priority 2 (build loop, chunk 17): captions in the capture
- `ui/record_demo.py` now overlays a one-line caption per case in the real recording. Still no voice-over; I04 case and 3:00 trim pending.
- Next: narration/timing to 3:00 per demo-script.md, I04 hidden-text case.

### Thu Oct 29, 11:20 IST: checkpoint (build loop, chunk 18)
- Checked state: no recorded OFF/ON results exist for I04 (hidden text), so it can't be added to the replay UI without a new cached run; recorder still captions five cases at 14 s each (~70 s). No code change this chunk.
- Next: run I04 OFF/ON (cached), add it to ui/build_demo.py CASES, then narration timing to 3:00.

### Thu Oct 29, 14:30 IST: Priority 2 (build loop, chunk 19): recorder reaches ~3:00
- `ui/record_demo.py`: title card (12 s), five cases at 20 s each, results card (real dev-set counts: Glimmer and Spark OFF 15/60, ON 0/60, controls 12/12, labelled dev set / small n), limits card (20 s). Total ~2:32 of capture, leaving room for voice-over pauses. Syntax checked; not re-recorded this chunk. Repo URL on the card is a placeholder until the repo is public.
- Next: re-record, then I04 run, voice-over.

### Thu Oct 29, 17:30 IST: checkpoint (build loop, chunk 20)
- Tried to re-record the capture: sandbox reset removed /usr/bin/chromium (and playwright). Reinstalled playwright + its ffmpeg; no Chromium binary available. No code change.
- Next: `python3 -m playwright install chromium` (or apt chromium), point record_demo.py at it, re-record, then I04 run and voice-over.

### Thu Oct 29, 20:30 IST: checkpoint (build loop, chunk 21)
- `playwright install chromium` fails in this sandbox (download error) a second time; no browser binary, so no re-record. No code change. Needs a browser-agent capture or an apt chromium route.
- Next: try `apt-get install chromium` / browser agent to screen-record ui/demo.html; then I04 run, voice-over.

### Thu Oct 29, 23:20 IST: checkpoint (build loop, chunk 22)
- `apt-get install chromium` not available (package not found); third stall on a local browser, so logging and moving on. README already covers judge packaging. No code change.
- Next: use the browser agent to screen-record ui/demo.html (or ship the replay UI plus captions), then I04 run, voice-over.

### Fri Oct 30, 00:15 IST: browser unblocked
- Sandbox is arm64 with no root. Fix: `pip install playwright==1.47.0 && python3 -m playwright install chromium` (chromium-1134 downloads), then extract missing libs from Debian arm64 debs (libatk1.0-0t64, libatk-bridge2.0-0t64, libatspi2.0-0t64, libxcomposite1, libxdamage1, libxres1) with `dpkg-deb -x` into ~/xl and `export LD_LIBRARY_PATH=$HOME/xl/usr/lib/aarch64-linux-gnu`. demo-v3.html renders.

### Fri Oct 30, 00:40 IST: 3-minute submission video (real screen capture)
- New `ui/video/`: `segs.json` + `tts.py` (edge-tts, en-US-AndrewMultilingualNeural, +5%), `build_audio.py` (timeline, two-pass loudnorm to -16 LUFS, captions.srt, script.txt), `record_submission.py` (Playwright 1.47 chromium, real mouse clicks and key presses on `ui/demo-v3.html`, CDP screencast at 1920x1080 with the page zoomed 1.25x), `encode.py` (1080p30 H.264 + AAC + soft subs, duration check).
- `ui/demo-v3.html`: recorder hook `window.__sched(stepTimes, runMs)` so replay steps land on narration lines; the default pace is unchanged. (The v3 build script lives outside the repo in brag-output-v5/work and does not have this hook yet.)
- `ui/record_demo.py`: dropped the hard-coded `/usr/bin/chromium` and now uses Playwright's own chromium.
- Output (outside the repo): `hackathon/invoiceshield/submission-video/final.mp4`, 2:57, plus final.srt, poster.jpg, script.txt, README.md. Story: L02 look-alike domain, ₹59,000; shield OFF pays the wrong account, shield ON blocks; X04/X_ifsc held; B01 genuine paid; Rule of Two; vendor record; dev-set results (15/60 → 0/60, 12/12, Glimmer same, held-out not run); limits.
- Next: run the frozen held-out set, then update the results view and the narration line; review the video before submitting.

### Fri Oct 30, 01:45 IST: ACCT_RE fix + clean-invoice false-positive run (backlog #4, #10)
- `assembler/extract.py`: account numbers are now context-aware (`classify_number` / `printed_accounts`): a 9–18 digit run counts only under a bank label (A/c, Account, Beneficiary, Bank, IFSC, SWIFT, remit, pay to, NEFT/RTGS…), next to an IFSC/SWIFT code, or unlabelled near bank words; phone, GSTIN/PAN, IRN/Ack, e-way, PO/invoice/order, ref/UTR, PIN, MICR/routing and any other `<label>:` field are excluded unless an IFSC follows. Digit runs inside hex (IRN) or Indian-grouped amounts no longer match; a number followed by "," or "." now does (old regex missed it). Bank box under "Account No./Number" with grouped digits is read. `item_lines` now reads multi-column tables (amount column closes the row) and negative amounts.
- Tests: `tests/test_account_extraction.py` (67: true positives incl. injections, fake tool call, OCR-garbled labels; false positives incl. phones, IRN/Ack, PO, UTR, pincode, routing, amounts) and 2 new item-table tests. Full suite: **pytest 128 passed** (59 existing + 69 new, incl. test_variants); **`cargo test --release` 37 passed** (12 fixture + 21 gate + 4 master_change; toolchain reinstalled via rustup with a zig-cc linker shim because the sandbox has no `cc`; governor Rust source unchanged today).
- New `dataset/gen/make_clean.py` + `dataset/clean/` (60 genuine invoices, seed 20261030, deterministic) and `eval/run_clean.py`.
- Clean FP run (eval/EVAL_LOG.md): first run 38 PAY / 17 REVIEW / 5 BLOCK (gate) exposed two extraction bugs (labelled refs outside the vocabulary; multi-column item table). Fixed, both runs kept. Final: gate **56/4/0**, Muse Spark model path **56/4/0**; all 4 non-PAY are "Round off" lines (`charge_not_on_po`), left unfixed on purpose. Fresh seed 20261101, run once after the fixes: 58/2/0, again round-off only. A generator bug (4 invoices dated after the agent's "today") made Spark hold 3; fixed in the generator, raw run kept.
- Dev numbers unchanged: gate-only, benign 24/24, sequences, variants, and cached ON replays (Glimmer and Spark 0/60 wrong-place pays, controls 12/12); OFF 15/60 does not touch the extractor. Video/README numbers stand.
- Next: decide the round-off rule before the held-out run; currency (#6).

### Fri Oct 30, 05:20 IST: checkpoint (build loop)
- Round-off rule drafted (ignore a "round" line under ₹1 in `charge_not_on_po`, engine.rs ~l.294) but not committed: the Rust toolchain/zig-cc linker shim is gone after a sandbox reset (`cc` not found), so it could not be tested. Reverted.
- Next: restore the linker shim, apply the round-off change with a test, rerun clean-FP, then freeze rules and run the held-out set once.

### Fri Oct 30, 08:20 IST: rules frozen (build loop)
- Rust toolchain is gone again (no cargo/cc), so the round-off tweak can't be tested. Decision: leave the engine unchanged and report round-off holds (2–4 of 60 clean) as a known false-positive source. See DECISIONS.md.
- Next: generate the non-K rows of the frozen held-out set and run once (Fri night/Sat); no rule edits after.

### Fri Oct 30, 17:20 IST: checkpoint (build loop)
- Reviewed state: rules frozen, clean-FP results saved in eval/results/. Held-out generator (63 non-K rows from eval-plan §3 + threat model) not yet started; no code change this chunk.
- Next: write dataset/gen/make_heldout.py for the non-K rows, commit the manifest hash before running once.
