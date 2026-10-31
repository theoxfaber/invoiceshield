# Regression run: before-cf9c591

Produced by `tools/regress.sh` (steps dev, ablation, clean, v1, v2, summary) at commit cf9c591 (before the v3-prep fixes).

**v1 and v2 here are re-runs after fixes, on sets we have already seen. They are NOT blind results.**
The blind numbers are the first runs only (`eval/results/blind-cases-run1-680be4d.*`, `eval/results/heldout-v2-run1-355783b.*`).
The `note` field copied into `v2.meta.json` ("Run once") describes the v2 set's history, not this run.

Consistency check (2026-10-31): `v1.score.json` and `v2.score.json` were re-scored from `v1.jsonl` / `v2.jsonl`
with `python -m tools.heldout_eval --score`, and `summary.json` regenerated with `python -m tools.regress_summary`;
all four score files and the summary were byte-for-byte identical in content to the saved ones. 60 rows per set, 60 unique case ids, 0 harness errors.
