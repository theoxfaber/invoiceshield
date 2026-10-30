# Findings: the extractor and the gate on public, real invoices

Phase 5 of `docs/HARDENING.md`. Written 2026-10-30 (IST). Runner: `eval/run_real.py`; raw results in
`eval/results/real-*.json`; log entry in `eval/EVAL_LOG.md` (2026-10-30, ~19:15 IST).

## What was tested, and what it is not

| Set | What it is | Licence | Used |
|---|---|---|---|
| **MIDD** (Zenodo 5113009, Baviskar, Ahirrao, Kotecha 2021) | **Real Indian GST tax invoices**, OCR'd and released as IOB token files with tags for supplier/buyer GSTIN, invoice no, date, grand total. 708 files, 684 distinct (24 are the same invoice under two file names). | CC BY 4.0 | all 684 |
| **Sparrow** (Katana ML, HF `katanaml-org/invoices-donut-data-v1`, built on Mendeley doi:10.17632/tnj49gpmtz.2) | Rendered invoice **images** with labels (invoice no, IBAN, items, totals). The content is generated (fake names, US-style `$` amounts with decimal commas). It's a test of real-image OCR, not real business data. | MIT | test + validation, 76 |

Not used: two Hugging Face sets with 22 real Indian invoices (`vanshbadjate07/invoice`, `AjitRawat/invoice`) state no licence, which means all rights are reserved. Two Kaggle sets need the user's Kaggle login, so they were skipped as instructed. FATURA/FATURA2 are template-generated and non-Indian, with no bank fields. SROIE and CORD are receipts.

**Provenance and storage.** Raw files are downloaded to `data/third_party/raw/` (gitignored) and pinned by sha256 in `dataset/real/sources.py` and `dataset/real/manifest.json`. Only anonymised labels are committed: `dataset/real/labels_midd.json` and `labels_sparrow.json`. In those files, identifiers (GSTINs, account numbers, IFSCs, invoice numbers) are salted sha256 hashes; amounts and currency are kept. `run_real.py` re-derives the labels from the raw data and warns if they drift from the committed hashes. The results files keep masked values only (`27…ZP(15)`).

**Labels.**
- MIDD's own tags are used for supplier and buyer GSTIN, invoice number and grand total. The "true" GSTIN repairs the tag's OCR slips. The repair is gated by the GSTIN checksum (`structural.gstin.repair_ocr`). Both the true value and the printed token are scored.
- **Bank account and IFSC are hand labels.** MIDD doesn't tag them. I read the token streams of the 4 main suppliers and wrote down each supplier's canonical account and IFSC. I fixed OCR slips by comparing many copies: one IFSC prints as `IDIBOOONO12` in 144 copies, and its true form is `IDIB000N012`. A label is set only on invoices whose text actually prints that value, allowing ≤2 or ≤3 OCR slips. I spot-checked 60 random invoices by eye against their bank-box text, and that check changed the presence rule (see below).
- One supplier's account is unrecoverable: the dataset stores it as the spreadsheet artefact `5.09044E+11`. It is excluded from account scoring.
- The plaintext hand labels sit in the gitignored raw dir. Their hashes are committed.

**Limits on what these numbers mean.**
1. **MIDD has no page images and no line breaks.** The extractor sees the OCR token stream joined with spaces. Layout-dependent logic (item tables, "label on the previous line") is not tested on MIDD.
2. **MIDD is heavily clustered.** It has 4 main suppliers (196 / 461 / 31 / 15 files) plus 3 singletons, and all invoices are billed to one buyer group in Pune. The invoices aren't independent draws, so **the Wilson intervals below understate the real uncertainty**. Per-supplier rows are in the results file.
3. **The gate false-positive run uses a synthetic master.** Each invoice gets a vendor whose GSTIN and bank match the labels, a PO whose balance equals the labelled total, a sender domain on file and a clear checker. The approval threshold and cap are raised out of the way. 151 of the 578 MIDD invoices are over ₹1,00,000 and would REVIEW under the default policy. That is a policy decision, not an extraction error.
4. These are **text-level** results. Every MIDD invoice is an OCR'd scan, so as the gate really treats them (`ocr_as_is`), **all 578 are REVIEW by design** (`evidence_read_by_ocr`). The informative number is the counterfactual `text_layer_cf`: the same text treated as a digital PDF's text layer. It measures how often extraction alone would stop a genuine invoice.
5. The same team wrote the extractor, the hand labels and this harness. The extractor was then changed after seeing the results (before/after below). The "after" numbers are therefore tuned on this data and are **not held-out**.

## Headline numbers

Extraction, MIDD, current branch (`eval/results/real-after.json`). Each row gives correct/labelled, the rate, and the Wilson 95% interval:

| field | correct / labelled | rate (Wilson 95%) | missing | wrong | note |
|---|---|---|---|---|---|
| supplier GSTIN (true value) | 654 / 663 | 98.6% (97.4–99.3) | 8 | 1 | 91.4% equal the printed token; the rest are OCR slips repaired by checksum |
| bank account | 522 / 537 | 97.2% (95.4–98.3) | 11 | 4 | the 4 wrong are OCR misreads (`6711312022`) |
| IFSC | 339 / 555 | 61.1% (57.0–65.1) | 216 | 0 | 0/192 for the supplier whose IFSC OCR prints as `IDIBOOONO12` |
| total | 561 / 579 | 96.9% (95.1–98.0) | 17 | 1 | read from amount-in-words + matching figure (new) |
| invoice no | 528 / 678 | 77.9% (74.6–80.8) | 146 | 4 | new candidate reader; column-header layouts miss |
| currency, as the governor reads it | 674 / 684 | 98.5% (97.3–99.2) | 0 | 10 | 280 print no marker → governor assumes INR (right here); 10 read USD/EUR from OCR noise |
| line items | not labelled in MIDD | | | | |
| spurious accounts (any non-supplier number extracted as an account) | 0 / 684 docs | 0% (0–0.6) | | | was 9 (phones near "Bank of …" in an address, VAT TIN) |

Extraction, Sparrow (OCR of real images, generated content): invoice no 74/74, currency 71/71, **total 0/74 and line items 0/74**, spurious account 4/76. All four spurious accounts are an IBAN that OCR split with a space.

**Gate false positives on genuine invoices** (counterfactual text layer, current `main` governor with Phase 1 currency, ledger reset per invoice):

| | MIDD (578) | of which BLOCK | MIDD excl. currency-only REVIEWs | Sparrow (74) |
|---|---|---|---|---|
| extractor on `main` (before) | **153 = 26.5%** (23.0–30.2) | **61** | 132 = 22.8% (19.6–26.4) | 7 = 9.5% (4.7–18.3) |
| this branch (after) | **50 = 8.7%** (6.6–11.2) | **1** | **28 = 4.8%** (3.4–6.9) | 7 = 9.5% (4.7–18.3) |

Before Phase 1 merged, the same two extractors on the old governor gave 132 (22.8%, 61 BLOCK) and 28 (4.8%, 1 BLOCK) on MIDD. Those raw files are `eval/results/real-prephase1-{before,after}.json`.

Remaining after-FPs on MIDD:
- 24 `currency_mismatch_invoice` and 19 `currency_ambiguous`. These come from OCR noise (finding F6); 22 invoices have only currency reasons.
- 22 `issuer_gstin_unreadable`. On these, the supplier GSTIN isn't in the OCR text at all, or is too damaged to repair.
- 4 `bank_details_differ_from_master`, from OCR-misread account digits.
- 1 `issuer_gstin_mismatch`. This is the BLOCK: an unrepairable OCR'd GSTIN.
- 1 `amount_differs_from_printed_total`.

## Findings

**F1. A genuine invoice was BLOCKed because OCR changed one character of the *buyer's* GSTIN.** (Fixed in the extractor.) The extractor drops the buyer's own GSTIN by exact match. OCR printed the buyer `…C1ZY` as `…CIZY`, which still matches the GSTIN regex. The buyer was then taken for the issuer, `issuer_gstin_mismatch` fired, and the invoice was **BLOCKed**. This hit 49 of 193 invoices from one supplier, and 52 of the 61 BLOCKs overall. Fix: `issuer_gstins()` recognises the buyer after a checksum-gated OCR repair.

**F2. OCR slips made real supplier GSTINs unreadable or wrong.** (Fixed in the extractor.) The slips: a letter in a digit slot (`27BEMPLO248G…`), a digit in a letter slot (`27AAFF14780E…` for `…FFI4780E…`), `$` for `S`, `I` for `1`, and a space after the state code (`27 AARFA…`). Before the fix there were 69 missing and 52 wrong out of 663. The new loose candidate is accepted only when a class-aware repair passes the mod-36 checksum. A strict-format GSTIN that no repair fixes is still reported as printed, so a forged GSTIN still differs from the master and still BLOCKs (tested). After the fix there are 8 missing and 1 wrong.

**F3. The amount rules were silently off on real invoices.** (Partly fixed in the extractor.) `printed_total_paise` only understood "Total Payable". No real invoice in either set says that: they use "Grand Total", "Bill Amount", "Total Invoice Amount" or Tally's "Amount Chargeable". Total extraction was **0/579**, and with no printed total, `amount_differs_from_printed_total` and `invoice_does_not_add_up` don't run. **This is fail-open:** an agent's amount went unchecked against the document on every real invoice. The fallback is now the largest amount the document writes **in words** that it also prints **in figures**. Both must agree, and "… and N paise" tax lines are skipped. MIDD total recall is now 96.9% with 1 wrong. It doesn't help Sparrow (`$ 978,12`, decimal comma, no words). **Rule-level recommendation (not applied, rules are frozen):** an invoice with no readable total should REVIEW, not skip the amount checks.

**F4. Line items are not read from real layouts at all.** (Open.) `item_lines` needs a line starting "Description" and a "Total Payable" line. Recall was 0/74 on Sparrow, and on MIDD there is no layout to read. So `charge_not_on_po` and `invoice_does_not_add_up` are effectively **untested on real invoices**. The padded-fee protection demonstrated on the dev set depends on our own generator's layout. Needed: a table reader that uses positions (pdfplumber words with x/y), or a signed extraction service, before claiming line-level checks on real documents.

**F5. Phone numbers and tax IDs were read as bank accounts.** (Fixed in the extractor.) Two patterns caused this:
- An address such as "Near Bank of Maharashtra … Pune.70306xxxxx" made a mobile number an "account".
- A "Company's VAT TIN : 27110029618" line under the bank box was read as an account.

Each one raises a tamper alert and REVIEW on a genuine invoice: 9 documents in all, 13 REVIEWs with OCR slips. The fixes:
- `tin`, `vat` and `cst` are now non-bank labels.
- An Indian-mobile-shaped number (10 digits, first digit 6–9) needs a real account label or an IFSC. A bare "bank" word nearby is no longer enough. An injected account under "pay to" / "A/c" / "Beneficiary" or next to an IFSC is still caught (tests).

**F6. Currency markers are noisy in OCR text.** (Open; finding for Phase 1's `doc_currency`.) OCR turns stamps, signatures and `%` signs into stray `$`/`€`. That made 43 genuine MIDD invoices (7.4%) REVIEW on `currency_mismatch_invoice` or `currency_ambiguous`, and 10 were read as USD/EUR outright. It's harmless today because OCR evidence is REVIEW anyway. It must be solved before OCR evidence can clear on its own. **It was not "fixed" by loosening the marker count.** A rule like "≥2 markers" (used by the candidate reader `assembler/fields.printed_currency`) would let a USD invoice that prints `$` once be read as INR, which is exactly the attack Phase 1 closes. The candidate reader is measured, not wired in.

**F7. IFSC OCR slips (`O` for `0`) make the IFSC invisible.** (Open, deliberately.) The strict `IFSC_RE` misses `IDIBOOONO12`, so recall is 61%. Folding `O`→`0` in slots 6–11 would recover it. But branch codes can legitimately contain letters, and a folded IFSC that differs from the master would raise a tamper REVIEW on genuine invoices. Since the payee never comes from the document, a missed IFSC only weakens the tamper signal. Left strict; the governor's Phase 1 `ifsc()` normaliser is the right place for any fold.

**F8. Misread account digits on genuine invoices raise tamper alerts.** (Open, by design.) 4 genuine invoices print the supplier's account with one OCR-wrong digit. The governor can't tell an OCR slip from a changed account, so it raises REVIEW plus a tamper alert. That is the safe behaviour. It's listed because it is a real false-positive source on scans.

**F9. IBANs are not modelled.** (Open.) `ACCT_RE` reads digit runs, so an IBAN split by OCR (`GB10YCPS61 791374226282`) yields a digit fragment that counts as an account. The master's IBAN never matches it, so the invoice gets REVIEW plus a tamper alert (4/76 on Sparrow). Foreign vendors need IBAN-aware extraction and matching.

**F10. Synthetic-only claims to retire.** The 56/60 and 58/60 clean-set false-positive numbers in `EVAL_LOG.md` hold for our own generator's layouts. On real token streams the comparable figures are:
- **26.5% before the fixes** on `main` (61 BLOCKs).
- **8.7% / 4.8% after**, counting with / without currency-only REVIEWs.
- **100% REVIEW as the scans really are**, because scans need a person by policy.

Any README or video line that implies the clean-set rate is the real-world rate should say "on our generator's layouts".

## Extractor changes made here (all evidence-identical on the dev material)

`assembler/extract.py`:
- `issuer_gstins()` (OCR-tolerant, checksum-gated, buyer recognised after repair).
- `printed_total_paise()` fallback `words_confirmed_total_paise()`.
- `tin`/`vat`/`cst` non-bank labels.
- The mobile-shaped-number rule.

`structural/gstin.py`: `repair_ocr()`.

`assembler/fields.py` (new, not used by the governor): `printed_invoice_no()`, `printed_currency()`.

**Regression check.** The `governor_evidence()` output of `main`'s extractor and this branch's is byte-identical on all 216 dev/clean documents:
- 24 risk-set PDFs
- 72 T2/scan/photo variants, OCR included
- 60 clean invoices, seed 20261030
- 60 clean invoices, seed 20261101

No dev, clean or variant verdict can move. Re-run the check with the gate-only arms (`run_gate_only`, `run_benign`, `run_sequences`): their verdicts and reasons are unchanged. The clean gate is 56/4/0.

Tests: `tests/test_real_extraction.py`, with fictional ZZ-PAN GSTINs only.

## Next steps (not done here)

- Signed or governor-owned extraction (Phase 2), so the evidence above can't be replaced by the caller.
- A position-aware table reader, then re-measure line items on Sparrow.
- An IBAN model.
- An OCR-aware currency rule.
- REVIEW when no total is readable (F3; needs a rules change after the held-out run).
- A real pilot via `tools/shadow.py` on an AP team's own invoices. That is the only way to get a real-world rate. These public sets are a proxy.
