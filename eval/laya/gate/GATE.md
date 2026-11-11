# Laya-cheap ship gate: FROZEN RULE

**Frozen 2026-11-11 about 14:35Z**, before any Nov 12 grade (Astra, or the stage-2 Fable regrade) exists.

**This rule is stricter than the one it replaces, and it was set after seeing the interim Fable-only numbers** (`compare.md`). The old rule (hard tier shows no regression against keywords and always-sonnet) is *replaced*, not kept as an alternative; laya-cheap passed it on the interim data and would have shipped.

laya-cheap ships only if **both** hold on the all-200 run:

1. **Non-inferiority to laya-shipped on the hard tier (tier 3).** The lower bound of the 95% confidence interval of the *paired* accuracy difference (laya-cheap minus laya-shipped, same tasks) is **greater than -5 percentage points**. The interval is a percentile bootstrap over tasks (20,000 resamples, seed 20261111). Newcombe's hybrid-score interval and an exact McNemar test are printed for information only (Newcombe is conservative at small n: identical picks at n=46 give about +-5.5 pts, which is why it is not the gate interval).
2. **Overall accuracy of laya-cheap >= keywords'** (point estimate; both CIs are reported).

Cost is reported and never traded against accuracy. Accuracy = the pick is in the oracle's acceptable set (within 0.05 of the best graded quality); a pick of a model with no run on that task counts as a miss.

`scripts/laya_gate_prep/gate_compare.py` prints a single `GATE: PASS|FAIL|ERROR` with the reasons (policies must be named `laya-cheap`, `laya-shipped`, `keywords`). `--selftest` includes a case that passes the old rule and fails this one, and a passing case. The run must be repeated for the final variants: `panel`, `drop-self`, `swap-fable` (the sensitivity check); the gate must hold in the panel variant, and the others are reported.

## Interim result (Fable grades only, 167 of 200 tasks, Claude models only): FAIL

INTERIM_PLACEHOLDER
