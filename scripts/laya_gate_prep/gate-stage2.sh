#!/usr/bin/env bash
# Gate stage 2: Fable grades of ALL 7 models' outputs on all 200 rh-* tasks (needed by the panel / drop-self / swap-fable variants).
# Waits for the Astra half (astra-half.gate-ready, written when the role models are measured+Astra-graded; or astra-half.done),
# then runs `calibrate.py grade --grader panel:fable`. The grade key hashes every run of the task, so the codex outputs change
# the key and all 200 tasks are regraded (the 167 old 4-model grades are replaced; old files are archived first).
# Guard (reused from claude-152-guarded.sh): a watcher stops the run if the Claude weekly usage meter reaches LIMIT % (default 65)
# or the meter is unreadable 3 times in a row. Resumable: rerun to continue; finished grades are skipped.
# Foreground only (a backgrounded nectra run fails with "Ctrl-C signal handler already registered"); the systemd unit is foreground.
set -uo pipefail
root=${CAL_ROOT:-$HOME/nectra-calibration}
t="$root/.orca-task"
LIMIT=${LIMIT:-65}
nectra=${NECTRA_BIN:-$HOME/nectra-eval/target/release/nectra}
held="$root/eval/calibration/results/routing-new-heldout"
export PATH="$HOME/.local/bin:$PATH"
export NECTRA_CALIBRATION_REGISTRY="$root/config/models.calibration-full.toml"
exec > >(tee -a "$t/gate-stage2.log") 2>&1
echo "$(date -Is) stage2 waiting for the Astra half"
for _ in $(seq 1 1152); do   # 4 days
  [ -f "$t/astra-half.gate-ready" ] || [ -f "$t/astra-half.done" ] && break
  sleep 300
done
[ -f "$t/astra-half.gate-ready" ] || [ -f "$t/astra-half.done" ] || { echo "Astra half never finished; giving up"; exit 1; }
meter() { python3 "$t/meter.py" 2>/dev/null || echo ""; }
m0=$(meter); echo "$(date -Is) stage2 start, meter ${m0:-unreadable}%"
if [ -z "$m0" ] || python3 -c "import sys;sys.exit(0 if float('$m0')>=$LIMIT else 1)"; then
  echo "meter ${m0:-unreadable}% >= $LIMIT or unreadable: not starting. Rerun later: systemctl --user restart laya-gate-stage2"; exit 4; fi
mkdir -p "$held/grades/panel/fable.old-$(date +%Y%m%d)" && cp -n "$held"/grades/panel/fable/*.json "$held/grades/panel/fable.old-$(date +%Y%m%d)/" 2>/dev/null
cd "$root"
(
  fails=0
  while true; do
    sleep 300
    m=$(meter); echo "$(date -Is) stage2 meter ${m}%" >> "$t/guard.log"
    if [ -z "$m" ]; then fails=$((fails+1)); else fails=0; fi
    if [ "$fails" -ge 3 ] || { [ -n "$m" ] && python3 -c "import sys;sys.exit(0 if float('$m')>=$LIMIT else 1)"; }; then
      echo "STOPPED stage2 at meter ${m:-unreadable}% on $(date -Is) (limit $LIMIT). Rerun after the weekly reset: systemctl --user restart laya-gate-stage2" > "$t/gate-stage2.stopped"
      pkill -TERM -f "calibrate.py grade --out $held"; sleep 15; pkill -KILL -f "calibrate.py grade --out $held"; exit 0
    fi
  done
) &
watcher=$!
rm -f "$t/gate-stage2.stopped"
python3 scripts/calibrate.py grade --out "$held" --grader panel:fable --tasks all --max-cost 0 --nectra "$nectra"
rc=$?
kill "$watcher" 2>/dev/null
[ $rc -eq 0 ] && [ ! -f "$t/gate-stage2.stopped" ] && touch "$t/gate-stage2.done" && echo "DONE stage 2"
exit $rc
