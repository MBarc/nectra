#!/usr/bin/env bash
# Final gate step: run gate_compare.py with the FROZEN rule (eval/laya/gate/GATE.md) once all grades exist, and write
# $REPO/eval/laya/gate/GATE-RESULT.md + GATE-RESULT-<variant>.json. The decision is the `panel` variant; drop-self and swap-fable
# are the sensitivity check (reported; a different verdict there is flagged). Called at the end of gate-stage2.sh; safe to rerun.
# Inputs staged in $CAL_ROOT/.orca-task/gate-inputs: laya-cheap.toml, models.laya-cheap.toml (from MBarc/nectra-routing-accuracy
# 46e60a4) and lmodel/ (verified Laya alpha.2 model dir). Routing with Laya is slow (one model load per prompt, ~30 min/policy),
# so the policies run in parallel and picks are shared between variants that have the same model set (panel, swap-fable).
set -uo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root=${CAL_ROOT:-$HOME/nectra-calibration}
gi="$root/.orca-task/gate-inputs"
repo=${REPO:-$HOME/ac-work/nectra/laya-gate-prep}
out="$repo/eval/laya/gate"; mkdir -p "$out"
work=$(mktemp -d "${TMPDIR:-/tmp}/gate-final.XXXXXX")
trap 'pkill -TERM -P $$ 2>/dev/null; wait 2>/dev/null; rm -rf "$work"' EXIT INT TERM
gc=(nice -n 15 python3 -I "$here/gate_compare.py" --repo "$repo" --baselines sonnet,opus,fable,haiku)
pol=(--policy keywords=backend:keywords --policy "laya-shipped=laya:-:-:$gi/lmodel"
     --policy "laya-cheap=laya:$gi/laya-cheap.toml:$gi/models.laya-cheap.toml:$gi/lmodel")
for v in panel drop-self swap-fable; do
  # picks are cached per model set: panel and swap-fable share one file
  case $v in drop-self) pk=$work/picks-drop.json ;; *) pk=$work/picks-all.json ;; esac
  load=(); [ -f "$pk" ] && load=(--load-picks "$pk")
  "${gc[@]}" --variant $v "${pol[@]}" "${load[@]}" --save-picks "$pk" --json-out "$out/GATE-RESULT-$v.json" > "$work/$v.md" 2>&1 \
    || { echo "gate_compare failed for $v:"; tail -n 5 "$work/$v.md"; }
done
{
  echo "# Gate result (generated $(date -u +%FT%TZ))"
  echo; echo "Rule: eval/laya/gate/GATE.md (frozen 2026-11-11). Decision variant: **panel** (Fable + Astra mean). drop-self and swap-fable are the sensitivity check."
  echo; for v in panel drop-self swap-fable; do grep -m1 'GATE:' "$work/$v.md" | sed "s/^/- $v: /"; done
  for v in panel drop-self swap-fable; do echo; echo "---"; echo; cat "$work/$v.md"; done
} > "$out/GATE-RESULT.md"
grep -h '"verdict"' "$out"/GATE-RESULT-*.json | sort | uniq -c | awk '{print}' | head -3
echo "wrote $out/GATE-RESULT.md"
