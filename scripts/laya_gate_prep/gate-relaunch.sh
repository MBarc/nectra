#!/usr/bin/env bash
# Laya ship gate: one command to resume the Astra half at the Codex reset (2026-11-12 14:55Z).
#
#   gate-relaunch.sh              preflight, then start the real run now as a user systemd unit (nice 15, 1 job)
#   gate-relaunch.sh --at-reset ["YYYY-MM-DD HH:MM:SS"]  same, but the units sleep until then (UTC; default 2026-11-12 14:55:00) first (safe to arm early)
#   gate-relaunch.sh --dry-run    preflight + every calibrate.py step in --dry-run against the real tree (only re-materializes the idempotent task manifests)
#   gate-relaunch.sh --stub       full path on a scratch copy of 3 tasks with a stub codex (no network, no Codex call)
#
# Real run = astra-half.sh (retry wrapper) -> astra-half-body.sh. The wrapper's try counter / "gave up" state from the
# previous attempt is archived first, so it gets a fresh 200 tries. Watch: journalctl --user -u laya-gate -f
# Stop: systemctl --user stop laya-gate. Resumable: finished grades are skipped.
set -euo pipefail
root=${CAL_ROOT:-$HOME/nectra-calibration}
t="$root/.orca-task"
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
nectra=${NECTRA_BIN:-$HOME/nectra-eval/target/release/nectra}
export PATH=$HOME/.local/bin:$PATH
mode=${1:-run}
at=${2:-2026-11-12 14:55:00}   # UTC start for --at-reset; armed 5 min late (15:00:00) so the first try is not rate-limited
fail() { echo "PREFLIGHT FAIL: $*" >&2; exit 1; }

preflight() {
  command -v codex >/dev/null || fail "codex not on PATH"
  codex --version | grep -q 'codex-cli 0\.166' || echo "warning: codex version is not 0.166.x: $(codex --version)"
  [ -x "$nectra" ] || fail "nectra binary missing: $nectra"
  [ -x "$root/.orca-task/astra-half-body.sh" ] || fail "body script missing"
  [ -f "$root/config/models.calibration-full.toml" ] || fail "calibration registry missing"
  [ -f "$root/new-heldout-prompts.json" ] && [ -f "$root/new-heldout-prompts-2.json" ] || fail "held-out manifests missing"
  local use; use=$(df --output=pcent / | tail -1 | tr -dc 0-9)
  [ "$use" -lt 83 ] || fail "disk at ${use}% (guard 83%)"
  python3 --version | grep -q 'Python 3\.\(1[1-9]\|[2-9][0-9]\)' || fail "need python >= 3.11"
  echo "preflight ok (codex $(codex --version | awk '{print $2}'), disk ${use}%, nectra $($nectra --version 2>&1 | head -1))"
}

case "$mode" in
  --dry-run)
    preflight
    cd "$root"
    export NECTRA_CALIBRATION_REGISTRY="$root/config/models.calibration-full.toml"
    export NECTRA_ROUTING_HELDOUT_MANIFEST="$root/new-heldout-prompts.json,$root/new-heldout-prompts-2.json"
    held=$root/eval/calibration/results/routing-new-heldout
    python3 -W ignore scripts/materialize-routing.py          # idempotent; only rewrites the task manifests
    python3 scripts/calibrate.py run --out "$held" --models 'gpt-5.6-sol,gpt-5.6-luna,gpt-6-astra' --tasks 'rh-*' --jobs 1 --deadline 600 --max-cost 0 --nectra "$nectra" --dry-run | tail -8
    python3 scripts/calibrate.py grade --out "$held" --grader panel:gpt-6-astra --tasks all --max-cost 0 --nectra "$nectra" --dry-run | grep -c 'would grade' | sed 's/^/tasks that would be Astra-graded: /'
    echo "dry run ok"
    ;;
  --stub)
    preflight
    scratch=$(mktemp -d "${TMPDIR:-/tmp}/laya-gate-stub.XXXXXX")
    trap 'rm -rf "$scratch"' EXIT
    src=$root/eval/calibration/results/routing-new-heldout
    for m in "$src"/runs/*/; do
      mkdir -p "$scratch/runs/$(basename "$m")"
      cp "$m"rh-code-02.* "$m"rh-debug-02.* "$m"rh-plan-02.* "$scratch/runs/$(basename "$m")/"
    done
    mkdir -p "$scratch/bin"; cp "$here/stub_codex.py" "$scratch/bin/codex"
    cd "$root"
    export NECTRA_CALIBRATION_REGISTRY="$root/config/models.calibration-full.toml"
    PATH="$scratch/bin:$PATH" python3 scripts/calibrate.py grade --out "$scratch" --grader panel:gpt-6-astra --tasks 'rh-code-02,rh-debug-02,rh-plan-02' --max-cost 0 --nectra "$nectra"
    n=$(ls "$scratch"/grades/panel/gpt-6-astra/*.json | wc -l)
    [ "$n" -eq 3 ] || fail "stub run wrote $n grade files, expected 3"
    grep -q 'STUB GRADE' "$scratch"/grades/panel/gpt-6-astra/rh-code-02.json || fail "stub grade not recorded"
    # resumability: a second pass must skip all three
    PATH="$scratch/bin:$PATH" python3 scripts/calibrate.py grade --out "$scratch" --grader panel:gpt-6-astra --tasks 'rh-code-02,rh-debug-02,rh-plan-02' --max-cost 0 --nectra "$nectra" | grep -c 'already graded' | grep -qx 3 || fail "resume did not skip"
    echo "stub run ok (3 graded through nectra->stub codex, resume skips; real tree untouched)"
    ;;
  run|--at-reset)
    preflight
    for f in astra-half.done astra-half.gate-ready; do [ -f "$t/$f" ] && mv "$t/$f" "$t/$f.old-$(date +%Y%m%d%H%M%S)"; done
    [ -f "$t/astra-half.tries" ] && mv "$t/astra-half.tries" "$t/astra-half.tries.old-$(date +%Y%m%d%H%M%S)"
    timer=(); [ "$mode" = "--at-reset" ] && timer=(--on-calendar="$at UTC")   # a real systemd timer, not a sleeping service
    systemctl --user reset-failed laya-gate laya-gate-stage2 2>/dev/null || true
    systemd-run --user --unit=laya-gate "${timer[@]}" --description="Laya gate: Astra half" -p Nice=15 -p CPUQuota=100% \
      /bin/bash -c "exec bash $t/astra-half.sh"
    systemd-run --user --unit=laya-gate-stage2 "${timer[@]}" --description="Laya gate: stage 2 Fable regrade (guarded)" -p Nice=15 -p CPUQuota=100% \
      /bin/bash -c "exec bash $here/gate-stage2.sh"
    [ "$mode" = "--at-reset" ] && systemctl --user list-timers --all --no-pager | grep -E "NEXT|laya-gate"
    echo "started: journalctl --user -u laya-gate -f ; progress: tail -f $t/astra-half.log"
    ;;
  *) sed -n 2,11p "$0"; exit 2 ;;
esac
