#!/usr/bin/env bash
# Route the held-out prompts for the Laya policies (slow: one model load per prompt) in ONE foreground process group.
# Ctrl-C / kill / exit takes every child with it (trap), so nothing is left detached.  usage: run-interim-picks.sh OUTDIR
set -u
out=${1:?outdir}; mkdir -p "$out"
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
trap 'pkill -TERM -P $$ 2>/dev/null; wait 2>/dev/null' EXIT INT TERM
: "${LAYA_MODEL_DIR:?dir with the verified alpha.2 model}" "${LAYA_CHEAP_SETTINGS:?config/laya-cheap.toml}" "${LAYA_CHEAP_REGISTRY:?config/models.laya-cheap.toml}"
nice -n 15 python3 -I "$here/gate_compare.py" --variant fable --baselines "" --policy "laya-shipped=laya:-:-:$LAYA_MODEL_DIR" --save-picks "$out/picks-laya-shipped.json" > "$out/shipped.log" 2>&1 &
nice -n 15 python3 -I "$here/gate_compare.py" --variant fable --baselines "" --policy "laya-cheap=laya:$LAYA_CHEAP_SETTINGS:$LAYA_CHEAP_REGISTRY:$LAYA_MODEL_DIR" --save-picks "$out/picks-laya-cheap.json" > "$out/cheap.log" 2>&1 &
wait
