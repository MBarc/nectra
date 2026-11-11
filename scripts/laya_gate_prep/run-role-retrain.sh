#!/usr/bin/env bash
# STAGED, NOT RUN. Role-head retrain on calibration oracle labels, after the gate data lands.
#   1. regenerate the oracle:  (routing_accuracy.py build, or gate_compare's oracle) -> ORACLE=/path/oracle.json
#   2. ORACLE=... bash run-role-retrain.sh [--yes]        (without --yes: build + leak check + estimate only)
# Trains from the last good checkpoint (continues, role question only + 800 replay rows), 2 epochs, CPU, nice 15, 4 threads.
# Resumable (resume.pt each epoch): rerun the same command. Deploy only via tools/laya/retrain's eval bar (fresh-test-2).
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/../.." && pwd)"
: "${ORACLE:?set ORACLE=/path/to/oracle.json (needs split/prompt/acceptable_set/cost_usd_median)}"
base_run=${BASE_RUN:-$HOME/nectra-laya/runs/20261030-164330}
out=${OUT:-$HOME/nectra-laya/runs/role-$(date -u +%Y%m%d)}
nice -n 15 python3 -I "$here/role_retrain.py" build --oracle "$ORACLE" --out "$out" --replay-from "$base_run/train.jsonl" --replay 800
items=$(python3 -I -c "import json;print(json.load(open('$out/manifest.json'))['question_items_per_epoch'])")
python3 -I "$here/role_retrain.py" estimate --records "$items" --epochs 2 --threads 4
[ "${1:-}" = "--yes" ] || { echo "dry: data built in $out; pass --yes to train"; exit 0; }
df --output=pcent / | tail -1 | tr -dc 0-9 | awk '{exit !($1<83)}' || { echo "disk guard"; exit 3; }
export OMP_NUM_THREADS=4 MKL_NUM_THREADS=4
cd "$HOME/nectra-laya"
exec nice -n 15 venv/bin/python "$repo/tools/laya/resumable_train.py" --dev "$out/dev.jsonl" \
  --data "$out/train.jsonl" --base "$base_run/ckpt" --out "$out/ckpt" --epochs 2 --micro-batch 8 --grad-accum 8 --device cpu --seed 1
