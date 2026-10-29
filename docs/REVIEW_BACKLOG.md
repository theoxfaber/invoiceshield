# External review backlog (Oct 27, 2026, 22:29 IST)

Two independent AI reviews of the repo zip (commit 9d5386d) scored it 8/10 and 7.5/10. The goal is to clear every point below before submission. Ordered by how much it moves the judging score.

## P0: judges see these
1. Demo UI: one screen showing attack invoice -> OFF agent pays -> ON governor blocks -> audit trail, on cached replay. The 3:00 video is built from it.
2. README top: the headline result (15/60 -> 0/60 harmful pays, 12/12 clean paid), a one-paragraph threat model and a diagram. Less defensive wording, with the caveats kept lower down.
3. Frozen 77-case held-out set, run once and reported raw. Add a second and third invoice template, plus real (non-synthetic) clean invoices.
4. False-positive rate: run a larger clean-invoice set (50+) and measure REVIEW/BLOCK on genuine invoices, especially ACCT_RE long reference numbers. **Done Oct 30:** 60 genuine invoices (`dataset/clean/`), gate 56/4/0 and Spark model path 56/4/0 (PAY/REVIEW/BLOCK) after two extraction fixes (first run 38/17/5); all 4 remaining REVIEWs are round-off lines (still open, see EVAL_LOG). Fresh seed: 58/2/0.
5. Meta-model depth: a real SAM 3.1 run (Kaggle free GPU) on photos/scans, and the Glimmer vision path for scans. Show where the model adds value beyond the deterministic rules, e.g. the checker catching semantic padding that the rules miss.

## P1: correctness gaps reviewers found
6. Currency field in the governor: the USD vendor's amounts are compared against INR PO balances and thresholds. Add currency to the proposal/PO/evidence, REVIEW on mismatch, and per-currency thresholds. Not yet flagged in the docs; fix and document it.
7. Persistent ledger and audit log (SQLite, WAL), so duplicate/split detection and the hash chain survive a restart. Add a restart test.
8. Padded fee worded with a contracted_items term passes charge_not_on_po. Tighten it (quantity x unit price vs PO lines), or route it to the checker and document it.
9. Slow-drip split (S05): keep it as a stated limit, or add a 30-day per-vendor velocity rule.
10. ACCT_RE false positives: require proximity to bank words (A/c, IFSC, account) before flagging a tamper. **Done Oct 30:** context-aware `classify_number` in `assembler/extract.py`, 67 tests; dev/variant verdicts unchanged.
15. (new, Oct 30) Round-off lines (|amount| < ₹1) trip `charge_not_on_po` on genuine invoices: 4/60 + 2/60 in the clean sets. Decide the rule before the held-out run, not after.

## P2: hygiene
11. requirements.txt and LICENSE (done Oct 27). Still to do: pyproject or Makefile, Dockerfile, a GitHub Actions CI running cargo test and pytest, and one-command `make demo`.
12. Rename normalize::gstin() reuse for IFSC to normalize::upper_alnum() or a dedicated ifsc().
13. Ship a small committed LLM-cache sample (no keys), so the shield OFF/ON numbers replay without NVIDIA_API_KEY.
14. A public GitHub repo with real commit history near submission (proves in-event work; the zip has no history).
