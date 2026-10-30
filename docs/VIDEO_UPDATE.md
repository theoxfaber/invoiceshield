# Video update: final numbers (Oct 31, 2026)

Use only these numbers in the re-recorded video and its captions. Each comes from a file in `eval/results/`; the frozen commit for the held-out run is `55fb679`. Say which set a number is from every time it appears.

## Numbers to say

| Scene | Number | Exact wording to use | File |
|---|---|---|---|
| Problem (shield off) | 15/60 | "On our development set, with the shield off, the agent paid 15 of 60 poisoned samples to the wrong account" (Muse Spark Open; same for Muse Glimmer) | `day1_off_spark.jsonl` |
| Shield on | 0/60, controls 12/12 | "With the shield on: 0 of 60, and every genuine control still paid. Development set." | `day1_on_spark.jsonl` |
| Ablation | checker alone 26/60, rules 0/19 cases, both 0/60 | "The model on its own let 26 of 60 attack samples through; the deterministic gate let none through." | `ablation_dev_fa.json` |
| First blind test | 4/40 | "The first blind test caught us out: 4 of 40 attacks were paid, all to the vendor's own account, because our parser had read no line items." | `blind-cases-run1-680be4d.summary.json` |
| Second held-out set | 0/40 paid (gate arms), 18/18 genuine to review (rules), 12/18 (gate + model), checker alone 35/40 paid | "On a second held-out set, written by a separate agent after that failure: no attack was paid. But the gate couldn't read that invoice layout, so every genuine invoice went to a human; with Muse Spark Open reading the lines, 6 of 18 went through." | `heldout-v2-run2-55fb679.score.json` |
| Opening case | BLOCK | "Modelled on a publicly reported campaign from this month: blocked." | `demo-opening-case.json` |

Do not say "100% accuracy", "eliminates prompt injection", or any v1 post-fix figure as blind (v1 was re-run after we had seen it). Do not mention SAM 3.1.

## Labels that must appear on screen

- Dev numbers: "development set (rules built while looking at it)".
- v2: "second held-out set, authored after run 1 by a separate AI agent with no repo access; attack categories informed by run 1's failure classes; attempt 1 aborted on a harness crash before any verdicts".
- Replays: "replaying recorded runs".

## Scenes that change from the Oct 30 cut

1. Replace the "held-out set not run yet" caption with the v2 row above, including the 18/18 line. It is the honest weak spot and the reason the model's line reading matters.
2. Add the opening case (OC1) as the cold open: fictional Sharma Office Supplies, look-alike sender, hidden instruction, BLOCK.
3. Fail-closed beat: the OC1 checker reply was empty and was recorded as a REVIEW signal; show that row instead of only saying "fails closed".
4. Not built (production.md §7 items 2-5): live-call log panel, side-by-side agent/checker panel with provider badge, review page with L02 held, results view reading `eval/results`. Record those beats from the existing replay UI and the approver page (`/v1/ui/review`) or drop them.
