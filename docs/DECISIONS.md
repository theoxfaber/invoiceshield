# Decisions

## 2026-10-26 22:30 IST — Rules: rebuild the governor (Plan B)
Official rules (Cerebral Valley event guide §4): "New work only: What you present at this Hackathon must have been built during the event… Startups may build on existing products or codebases." We are on the Indie track, so no pre-existing code is reused. The governor is rewritten from scratch in-event.

## 2026-10-26 — Models: open-weight Meta models only
The Meta Model API requires a credit card on file to use the hackathon credits. We built without one, so the stack is Meta's open-weight models: **Muse Glimmer 30B** (Apache-2.0, text + image) as the accounts-payable agent and, in a separate quarantined context, as the document checker; **SAM 3.1** (open weights) for document region masks. Bonus upside: the whole thing can run on an office PC, so invoices need not leave the building.

Day 0 observations (Glimmer on NVIDIA's free trial endpoint): `response_format` JSON schema is accepted but not enforced, so every model output is strictly parsed and anything that fails becomes REVIEW. Image input works (~1 min per invoice image at medium reasoning), so model calls are cached by input hash.

## 2026-10-26 — Fixtures must look plausible
A smoke test showed that an attack invoice with an obviously fake IFSC (containing "FAKE") makes the unshielded agent refuse. That would make the baseline look safer than it is. Attack fixtures therefore use real-format bank details and believable stories, while every name, GSTIN and account stays fictional.

## 2026-10-26 23:30 IST — Fixture identifiers: real format, fictional values
Replaces the pre-event `FAKE`/`7777`/`9999`/`.test` conventions. GSTINs use a ZZ-series PAN with a valid check character; IFSCs use real bank codes (BARB, CNRB, HDFC, ICIC, SBIN, UTIB, KKBK) in each bank's real branch-code shape with made-up branch codes; account numbers have each bank's usual length; domains are plausible `.in`/`.co.in` names. V004 renamed to Visakha Steel Fabricators to avoid RINL's "Vizag Steel" brand. No email is sent and no payment is made to any of these; README carries a "fictional, any match is coincidental" line. The Day 0 smoke showed fake-looking identifiers make the unshielded agent refuse, which would understate the risk.

## 2026-10-26 23:30 IST — Model calls: cache by full-request hash + sample index
Glimmer runs at temperature 1.0, so each eval case is sampled k times (sample = 0..k-1) and each sample cached once. Failed calls are never cached. The demo replays the cache (`offline=True`).

## 2026-10-27 06:00 IST — Baseline agent runs at low reasoning, with a plain AP prompt
Glimmer tool calls on NIM free tier take 25–110 s and "medium" timed out upstream, so the shield-OFF baseline runs at "low", k=3. The prompt is an ordinary AP instruction (pay valid invoices, hold if unsure, reject if not payable) that never mentions fraud, and the agent has no vendor-master tool: it pays what the invoice prints. The harm rule is mechanical and fixed before the run. Realism fixes (today's date in the prompt, CGST+SGST on intra-state invoices) were made after a clean invoice was wrongly held, before any attack was run; no prompt was tuned against attack outcomes.

## 2026-10-27 08:30 IST: Governor treats any extra account/IFSC in the document text as a second set of bank details
The bank box is not the only place a payee can be named; injections paste "the correct account" elsewhere. The assembler extracts every account- and IFSC-shaped value in the text layer and the governor compares each with the master, so a pasted account is a tamper alert (REVIEW) even when the bank box looks right. Cost: a genuine invoice that prints another number shaped like an account (a long reference) would REVIEW; accepted, and it shows up in the false-positive count if it ever happens.

## 2026-10-27 08:30 IST: The shield-ON agent sees a vendor directory (IDs and names), the OFF agent does not
The ON agent needs vendor IDs to propose a payment. This makes ON and OFF differ in more than the payment tool, so the agent's own holds in the ON arm cannot be credited to the shield. The hijacked-agent arm (`eval/run_gate_only.py`) exists to measure the governor without that confound.

## 2026-10-27 08:30 IST: Eval reset endpoint exists only in eval mode
`/v1/eval/reset` returns 404 unless the server runs with `GOVERNOR_EVAL_CLOCK=1`, so samples of one invoice do not trip the duplicate rule. It is never available in a normal run.

## 2026-10-27 11:30 IST: Amount rules are REVIEW-only and check the document against itself and the PO
Three checks: the proposal must equal the printed Total Payable; item lines must sum to it; every non-tax line must name something the PO contracts for (`contracted_items`). They sit in the governor (Rust), fed by deterministic extraction, and can only escalate. Known cost: a PO with several legitimate line types needs those listed or its invoices go to REVIEW; a vendor that words a padded fee with a contracted term ("IT services expedite") would pass, which is why the real protection against over-billing remains the PO balance and tolerance. I02 was fixed after it showed up on the dev set, so it counts as a regression test, never as held-out evidence.

## 2026-10-27 11:30 IST: Slow-drip splits stay a stated limit
Three ₹40,000 invoices 8 days apart are each legitimate-looking and never cross ₹1,00,000 in any 7-day window. The governor pays them. We report this as a deliberate limit instead of widening the window until it breaks genuine monthly billing (benign BS3).

## 2026-10-27 20:00 IST: Go/no-go (single checkpoint from the build plan)
**GO, Plan B, open-weight Meta models.**
1. **Rules → Plan B.** The signed-in event guide says "New work only: What you present at this Hackathon must have been built during the event"; only Startups may build on existing codebases. We are Indie, so no code from `theoxfaber/agentic-payment-risk-governor` is used; it is cited as prior work only. The in-event Rust governor (started at kickoff, 33 tests by chunk 4) already covers most of the Plan B scope: payee binding, PO match, issuer binding, duplicate key + 7-day vendor window, monotonic merge with proptest, hash-chained audit, Axum server with operator resolve.
2. **Models → Muse Glimmer 30B (Meta open weights) via NVIDIA's free NIM endpoint, as both agent and quarantined checker.** Not Muse Spark and not the Llama API: Meta's Model API needs a card on file even for hackathon credits, and we build without one. Glimmer is confirmed working from India: Day 1 ran 60 OFF + 60 ON agent samples plus checker calls; live re-check at 19:4x IST answered a text call in 21.8 s. It misses the plan's p95 ≤ 20 s criterion (tool calls 25–110 s at low reasoning), so every call is cached by request hash and the demo replays the cache; JSON schema is not enforced, so all output is strictly parsed and failures become REVIEW. Upgrade path, no code change beyond `LLM_PROVIDER`: Muse Spark Open on Baseten if the credit request (submitted Oct 27 03:55 IST, no card) is granted. No reply yet.
3. **Segmenter → deterministic template-coordinate crop + local OCR (tesseract) as the default.** SAM 3.1 has not been run: no GPU (Kaggle signup blocked by a captcha; Baseten pending). SAM stays an optional overlay if GPU credits arrive; no claim about SAM is made unless it runs.
4. **Cut list impact:** none forced yet. M1 was reached Oct 27 08:30 IST (well ahead of the Plan B date, Oct 29 noon). Plan B cuts stay on the shelf and are applied only if M2 (Oct 30) slips.

## 2026-10-29 04:40 IST: Default provider is Muse Spark Open on Baseten; Glimmer stays selectable
The Baseten credit request was granted (no card). `BasetenMuseSpark` (`meta-models/muse-spark-open-1026`) is the default (`LLM_PROVIDER` / `--provider`); Glimmer 30B on NIM remains for the Day 1 numbers and as the open-weights, run-it-in-the-office option. Spark always reasons and has no JSON mode, so outputs are strict-parsed exactly as before; native tool calls work. Same prompts, temperature 1.0 and harm rule as Glimmer, so the two dev-set runs are comparable; nothing was tuned for Spark. The demo page still replays the Glimmer runs.

## 2026-10-29: Vendor bank changes need a call-back, two approvers and a cooling-off
Payee binding only holds if the master itself cannot be changed by a forged letter. `governor/src/master_change.rs` refuses a change without a call-back to the number on file, two distinct approvers other than the maker, and 72 h since the request, and audits it. Library only for now; see `docs/VENDOR_MASTER_CHANGES.md`.

## 2026-10-30 01:00 IST: Account numbers count only in a bank context (backlog #10)
`all_printed_accounts` used every 9–18 digit run in the text, so phones, IRN/Ack, e-way bill, PO/order/reference numbers and UTRs could raise the tamper alert on a genuine invoice. Now a digit run is an account only when its own field label, or the nearest label before it, is a bank one (A/c, Account, Beneficiary, Bank, IFSC, SWIFT, remit, pay to, NEFT/RTGS…), or when an IFSC/SWIFT code follows it on the same line, or when it has no label and bank words or a bank code are close by. Non-bank labels (Ph, GSTIN, PAN, IRN, Ack, PO, Invoice, Ref, UTR, PIN, MICR, routing…) and any other `<label>:` field exclude it unless an IFSC follows. The bank box is labelled, so it is always read; injected payees written in bank words or next to an IFSC still raise the alert (all dev and variant verdicts unchanged). Accepted gap: an account disguised under a non-bank label with no bank code beside it raises no alert; it still cannot be paid, because the payee comes only from the master.

## 2026-10-30 01:30 IST: What we changed after seeing the clean-invoice results, and what we did not
The first false-positive run (38/60 PAY on the gate path) exposed two extraction bugs, fixed with tests and with both runs kept: labelled reference fields outside the label vocabulary were treated as unlabelled numbers, and the item-table parser could not read a multi-column (HSN / qty x rate) table or negative amounts. We did **not** change the `charge_not_on_po` rule for "Round off" lines although it caused every remaining false positive: that is a policy choice, and changing it after seeing the set would make the number meaningless. A fresh-seed set was generated and run once after the fixes to give a less-tuned figure.

## Fri Oct 30: round-off rule NOT changed
The 2–4 "Round off" REVIEWs on clean invoices (charge_not_on_po) stay as measured. Rules are frozen as of now; the held-out run uses the current engine. Reported as a known false-positive source (about 2/60 to 4/60), not quietly fixed.
