# Internal red team: attacks on the running governor

**This is an internal exercise, not an external security review.** The same team that built InvoiceShield wrote and ran these probes, with full knowledge of the code. It finds what we thought to try. An independent review would go further. One has not happened, and nothing here should be read as if it had.

- Harness: `tools/redteam.py`. It starts the real Rust server over HTTP, uses the real assembler, and generates attack PDFs with `dataset/gen/make_invoice.py`.
- Raw results:
  - `eval/results/redteam-internal-pre-phase1.json`: pre-Phase-1 governor binary, built 2026-10-29; engine source unchanged through `473cb60`.
  - `eval/results/redteam-internal-post-phase1.json`: governor built from `main` at `e636306`, with Phase 1 currency, PO lines, the `ifsc()` normaliser and audit auth.
  - `eval/results/redteam-internal-post-phase3.json`: governor built from `main` at `a07989b`, adding the Phase 3 SQLite ledger. The restart probe now runs in production mode with an on-disk `GOVERNOR_DB`.
  - `eval/results/redteam-internal-post-phase2.json`: governor rebuilt from `main` at `30a3678` (Phases 1, 2, 3 and 6) plus this branch, which does not touch `governor/` or `extractor/`. It has Phase 2 signed evidence, and the binary refuses to start without `GOVERNOR_EXTRACTOR_PUBKEY`. The run uses the real extraction service (`extractor/`) and adds 11 trust-boundary probes and a sandbox size-cap probe. An earlier file with this name was built against a stale pre-Phase-2 binary. It was discarded and never committed.
  - `eval/results/redteam-internal-post-phase4.json`: the same harness against a governor rebuilt from `main` at `e28727f` (adds Phase 4 security), about 21:40 IST. Every probe has the same outcome as post-Phase-2; only the 401 response body text differs. The harness has no probes yet for Phase 4's own features (roles, maker-checker, rate limits, which are off in eval mode, and TLS).
- Rerun: `python3 -m tools.redteam --tag <name>`. It needs `governor/target/release/governor-server`.
- Runs 2026-10-30, about 19:00–19:20 IST; post-Phase-2 run about 21:10 IST. Frozen-rule policy: **nothing here changed a gate rule.** Findings are recorded below and assigned to the phase that owns the fix.

Outcome key:
- **held**: the system did the safe thing.
- **finding**: it did not, or only partly.
- **info**: recorded, no pass/fail.

| run | held | finding | info |
|---|---|---|---|
| pre-Phase-1 governor | 27 | 9 | 4 |
| post-Phase-1 governor (`main` e636306) | 34 | 7 | 4 |
| post-Phase-3 governor (`main` a07989b) | 35 | 6 | 4 |
| post-Phase-2 governor (`main` 30a3678, signed evidence) | 47 | 2 | 10 |
| post-Phase-4 governor (`main` e28727f), same probes | 47 | 2 | 10 |

The post-Phase-1 run has more probes, because it adds 5 currency probes.

The post-Phase-2 run has 59 probes:
- New: 11 trust-boundary probes and an 11 MB size-cap probe.
- The three forged-evidence probes now run on the DEV/TEST unsigned path and are recorded as info. Production refuses caller evidence, and the trust probes test that.
- The in-process `assemble()` timing probes are info, because production extraction runs in the sandbox.

## Findings (post-Phase-2 unless stated)

Open after Phase 2: **R4** (suffix re-submit) and **R6** (Cyrillic IFSC). Phase 2 closed R1–R3 and R7; Phase 3 closed R5.

| # | attack | what happened | why it matters | fix / owner |
|---|---|---|---|---|
| R1 | **Forged evidence: bank-swap PDF, the caller strips the bank fields from the evidence object** | pre-Phase-2: honest evidence → REVIEW + tamper; forged → **ALLOW**. Post-Phase-2: forged → REVIEW `evidence_unsigned` | The governor trusts the evidence it is sent. Money still goes only to the master account, because the payee never comes from evidence. But the tamper alert and REVIEW vanish. | **Fixed by Phase 2**: the production governor reads only evidence signed by the extraction service. Caller evidence → REVIEW `evidence_unsigned`. An envelope edited after signing → BLOCK `evidence_signature_invalid`. Through the signed path, the bank-swap PDF → REVIEW + tamper. |
| R2 | **Forged evidence: ₹23,600 invoice proposed at ₹70,000 with a forged `printed_total` and no item lines** | pre-Phase-2: **ALLOW**. Post-Phase-2: forged → REVIEW `evidence_unsigned`; honest signed evidence → REVIEW `amount_differs_from_printed_total` | Overpayment to the right payee, inside the PO balance and threshold. This was the harmful one. | **Fixed by Phase 2** |
| R3 | **Forged evidence: a scan relabelled `text_source=text_layer`** | pre-Phase-2: as OCR → REVIEW; relabelled → **ALLOW**. Post-Phase-2: production reads `text_source` only from the signed envelope | The scan-needs-a-person rule keyed off a caller-supplied label. | **Fixed by Phase 2**: an edited envelope fails its signature (BLOCK). The relabel still ALLOWs on the DEV/TEST unsigned path only (info). |
| R4 | Same invoice re-sent with a suffix on the number (`…/0900-A`) | **ALLOW** (still open post-Phase-2) | The duplicate key is vendor + normalised number. A new suffix is a new invoice, and the amount is under the window threshold. No PDF-hash or (vendor, amount, date) near-duplicate check exists. | Proposal: REVIEW on same vendor + same amount + same PDF hash within N days (rules change, after the held-out run) |
| R5 | **Same invoice after a governor restart** | pre-Phase-3: **ALLOW** (ledger forgotten); post-Phase-3 (production mode, on-disk DB): BLOCK `duplicate_invoice` | The in-memory ledger meant a restart re-opened every paid invoice to duplicate payment. | **Fixed by Phase 3** (SQLite WAL ledger). Eval mode still uses `:memory:` by design. |
| R6 | IFSC with a Cyrillic `В` (extractor path) | not extracted, no tamper alert (still open post-Phase-2) | Homoglyph IFSCs are invisible to `IFSC_RE`. The payee still can't change, and the same document already REVIEWs on its GSTIN. | Candidate: NFKC + confusable fold, and flag any non-ASCII inside an IFSC- or GSTIN-shaped token |
| R7 | 1,500-page PDF (0.7 MB); 11 MB file | pre-Phase-2: assembled in about 1–2 s, no cap. Post-Phase-2, through the extraction service: 1,500 pages → signed `failed` (`too_many_pages:1500>50`) in 0.3 s; 11 MB → signed `failed` (`file_too_large`) in 0.2 s; a 3-page image-only PDF → ok in about 16 s | In-process `assemble()` still has no cap, but production extraction runs in the sandbox. Under the caps, OCR cost is still linear in pages (about 5 s a page here), bounded by CPU 30 s and wall 60 s. | **Fixed by Phase 2** (`extractor/sandbox.py` Limits: 10 MB, 50 pages, CPU 30 s, wall 60 s, memory and file limits; failed → REVIEW) |
| (pre-P1) | USD invoice proposed with no currency (cents read as paise) | **ALLOW** before Phase 1 | Cents were compared with INR-paise balances and thresholds. | **Fixed by Phase 1**: now REVIEW (`currency_mismatch_invoice`, `currency_mismatch_po`) |
| (pre-P1) | `/v1/audit/verify` without a token | HTTP 200 before Phase 1 | Unauthenticated information about the audit log. | **Fixed by Phase 1**: now 401 |

## What held (post-Phase-2)

- **Trust boundary** (Phase 2; production-mode and strict eval governors with the real extraction service):
  - Genuine invoice through the signed path: ALLOW.
  - Caller-supplied evidence, including the R1 and R2 forgeries: REVIEW `evidence_unsigned`.
  - Signed envelope edited after signing: BLOCK `evidence_signature_invalid`.
  - The same envelope twice, with the ledger reset in between: BLOCK `evidence_replayed`.
  - An envelope bound to another document's hash: BLOCK `document_hash_mismatch`.
  - Signed by an unknown extractor key, or a zeroed signature: BLOCK `evidence_signature_invalid`.
  - Over-payment with honest signed evidence: REVIEW.
  - Bank-swap PDF through the signed path: REVIEW + tamper.
- **Restart** (Phase 3; production mode, on-disk DB, signed evidence): ALLOW, restart, BLOCK `duplicate_invoice`.

- **Replay and duplicates.** These were all BLOCKed:
  - An identical request sent twice.
  - The same invoice number with a zero-width space, fullwidth digits, an en dash and lower case, or spaces.
  - An IRN reused under a new invoice number.
  - The `x-eval-now` clock header in production mode, which is ignored, so it can't be used to dodge the 7-day window. The request was a duplicate BLOCK.
- **Splits.** 2 × ₹60,000 on the same day across two POs: the second is REVIEW (vendor window).
- **Currency** (Phase 1). These were REVIEW:
  - A USD invoice proposed as INR.
  - An INR invoice proposed as USD on an INR PO.
  - Evidence naming two currencies.
  - A currency with no configured limits (EUR).
  - A USD invoice with the currency stripped from forged evidence. The PO currency still catches it.

  The correct USD proposal on the USD PO was ALLOWed.
- **Unicode.** These were caught:
  - A Cyrillic-A GSTIN: not read by the extractor (REVIEW, unreadable), and BLOCKed when sent straight to the governor.
  - An IDN-homograph sender domain: BLOCK.
  - A bank key spelled with a Cyrillic letter in the proposal: REVIEW, because the schema is closed.
- **Schema abuse.** These were stopped:
  - `ACCOUNT_NUMBER` in the proposal: BLOCK.
  - A negative amount: BLOCK.
  - `i64::MAX`: BLOCK.
  - A float or string amount: REVIEW.
  - An unknown evidence field (`payee_override`): REVIEW.
- **Checker.** Malformed or missing checker output fails closed (REVIEW). Checker output that claims an ALLOW verdict is ignored, because signals only escalate.
- **HTTP surface.** These all behaved:
  - No bearer token: 401.
  - Resolving a REVIEW without the operator key: 401. An unset operator key is rejected, not treated as a match for an empty one.
  - A 3.6 MB body: 413 in 0.01 s, and the server stays up.
  - `/v1/eval/reset` in production mode: 404.
- **Hostile PDF bytes.** The sandbox refuses an 11 MB file before parsing (signed `failed` → REVIEW). Random bytes behind a PDF header raise a clean exception in-process. The pipeline and `tools/shadow.py` turn that into no payment.

Info only:
- **Forged evidence on the DEV/TEST path.** R1–R3 replayed with `GOVERNOR_DEV_UNSIGNED_EVIDENCE=1` still ALLOW there, by design. Production never enables that path.
- **PDF timing.** In-process `assemble()` of 1,500 pages: about 1 s. 3-page OCR: about 14 s in-process, about 16 s in the sandbox.
- **Slow drip.** 3 × ₹25,000, eight days apart, are each ALLOWed. This is the known limit S05; the PO balance still caps the total.
- **Fullwidth digits in an evidence account.** REVIEW + tamper, because they don't silently match.

## Not covered (be honest about scope)

- Network-level attacks: TLS, MITM, DoS beyond one oversized body. These are Phase 4 (TLS option, rate limits).
- Concurrency races on the HTTP path. The ledger mutex and the Phase 3 tests cover these, and this harness doesn't.
- Model-side prompt injection. That is covered by the dev-set ON/OFF arms in `EVAL_LOG.md`, not here.
- Supply chain (`cargo audit`, `pip-audit`). That is Phase 4.
- Insider misuse of the operator key, and master-data changes. Those are Phase 4 (maker-checker, two approvers).
- Adaptive attackers who have never seen the repo. That is the blind K set (Phase 5, a separate stream).

## Next steps

1. An **external** review by someone outside the team, against `tools/redteam.py`'s list plus their own ideas.
2. Done: rerun after Phase 2 merged (`redteam-internal-post-phase2.json`). R1–R3 and R7 now hold. R5 was re-checked after Phase 3 and holds. Re-run on the Phase 4 governor: same outcomes (`redteam-internal-post-phase4.json`). Still to do: add probes for Phase 4 itself (role separation, maker-checker bypass, rate limits in production mode, TLS).
3. Decide R4 and the "no readable total → REVIEW" rule (see `docs/FINDINGS_REAL_INVOICES.md` F3) together, as one rules change after the frozen held-out run.
