#!/usr/bin/env bash
# Regression suite run before every merge (v3-prep, 2026-10-31), in steps so each fits a short
# session: dev (gate-only, variants when rendered, sequences, benign), ablation (dev arms, offline),
# clean (gate + gate&model, offline), v1 and v2 re-runs (all arms; NOT blind, both sets have been
# seen; resumable: repeat the step until it prints "complete"), summary.
# Outputs: eval/results/regress/<tag>/. The dev scripts' fixed-name outputs are copied there and the
# committed copies restored.
#   GOVERNOR_BIN=... tools/regress.sh <tag> <dev|ablation|clean|v1|v2|summary> [commit]
set -euo pipefail
TAG=${1:?tag}; STEP=${2:?step}; COMMIT=${3:-$(git rev-parse --short HEAD)}
PY=${PY:-python3}
OUT=eval/results/regress/$TAG
mkdir -p "$OUT"
restore() { git checkout -- eval/results/day1_gate_only.jsonl eval/results/sequences_gate_only.jsonl eval/results/benign_gate_only.jsonl eval/results/variants_gate_only.jsonl 2>/dev/null || true; }
case $STEP in
dev)
  $PY -m eval.run_gate_only >"$OUT/dev_gate_only.log" 2>&1; cp eval/results/day1_gate_only.jsonl "$OUT/dev_gate_only.jsonl"
  if [ -f eval/variants/manifest.json ]; then $PY -m eval.run_gate_only variants >"$OUT/variants.log" 2>&1; cp eval/results/variants_gate_only.jsonl "$OUT/variants_gate_only.jsonl"; fi
  $PY -m eval.run_sequences >"$OUT/sequences.log" 2>&1; cp eval/results/sequences_gate_only.jsonl "$OUT/sequences_gate_only.jsonl"
  $PY -m eval.run_benign >"$OUT/benign.log" 2>&1; cp eval/results/benign_gate_only.jsonl "$OUT/benign_gate_only.jsonl"
  restore ;;
ablation)
  $PY -m eval.run_ablation_dev --tag "rg_$TAG" --offline >"$OUT/ablation.log" 2>&1
  for f in eval/results/*_rg_"$TAG".json*; do mv "$f" "$OUT/$(basename "$f" | sed "s/_rg_$TAG//")"; done
  restore ;;
clean)
  $PY -m eval.run_clean --both --offline --out "regress/$TAG/clean_arms.jsonl" >"$OUT/clean.log" 2>&1 ;;
v1|v2)
  set +e
  $PY -m tools.heldout_eval --set "$STEP" --run --resume --budget 95 --out "regress/$TAG/$STEP" --commit "$COMMIT" >>"$OUT/$STEP.log" 2>&1
  rc=$?; set -e
  if [ $rc -eq 3 ]; then echo "partial"; exit 0; fi
  [ $rc -eq 0 ] || exit $rc
  $PY -m tools.heldout_eval --set "$STEP" --score "regress/$TAG/$STEP" >>"$OUT/$STEP.log" 2>&1
  echo "complete" ;;
summary)
  $PY -m tools.regress_summary "$OUT" >"$OUT/summary.json"; cat "$OUT/summary.json" ;;
*) echo "unknown step $STEP"; exit 2 ;;
esac
