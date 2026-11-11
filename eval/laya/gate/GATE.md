# Laya-cheap ship gate: FROZEN RULE

**Frozen 2026-11-11 about 14:35Z**, before any Nov 12 grade (Astra, or the stage-2 Fable regrade) exists.

**This rule is stricter than the one it replaces, and it was set after seeing the interim Fable-only numbers** (`compare.md`). The old rule (hard tier shows no regression against keywords and always-sonnet) is *replaced*, not kept as an alternative; laya-cheap passed it on the interim data and would have shipped.

laya-cheap ships only if **both** hold on the all-200 run:

1. **Non-inferiority to laya-shipped on the hard tier (tier 3).** The lower bound of the 95% confidence interval of the *paired* accuracy difference (laya-cheap minus laya-shipped, same tasks) is **greater than -5 percentage points**. The interval is a percentile bootstrap over tasks (20,000 resamples, seed 20261111). Newcombe's hybrid-score interval and an exact McNemar test are printed for information only (Newcombe is conservative at small n: identical picks at n=46 give about +-5.5 pts, which is why it is not the gate interval).
2. **Overall accuracy of laya-cheap >= keywords'** (point estimate; both CIs are reported).

Cost is reported and never traded against accuracy. Accuracy = the pick is in the oracle's acceptable set (within 0.05 of the best graded quality); a pick of a model with no run on that task counts as a miss.

`scripts/laya_gate_prep/gate_compare.py` prints a single `GATE: PASS|FAIL|ERROR` with the reasons (policies must be named `laya-cheap`, `laya-shipped`, `keywords`). `--selftest` includes a case that passes the old rule and fails this one, and a passing case. The run must be repeated for the final variants: `panel`, `drop-self`, `swap-fable` (the sensitivity check); the gate must hold in the panel variant, and the others are reported.

## Interim result (Fable grades only, 167 of 200 tasks, Claude models only): FAIL

Generated 2026-11-11 by `gate_compare.py --variant fable` (Fable grades only: 167 of 200 held-out tasks, opus/sonnet/haiku/fable; Astra not in this oracle).

| policy | all acc (95% Wilson CI) | tier1 acc (95% Wilson CI) | tier2 acc (95% Wilson CI) | tier3 acc (95% Wilson CI) | mean $/task |
|---|---|---|---|---|---|
| keywords | 62.3% (104/167; 55–69%) | 57.6% (34/59; 45–69%) | 72.6% (45/62; 60–82%) | 54.3% (25/46; 40–68%) | $0.0997 |
| laya-shipped | 71.9% (120/167; 65–78%) | 69.5% (41/59; 57–80%) | 72.6% (45/62; 60–82%) | 73.9% (34/46; 60–84%) | $0.1434 |
| laya-cheap | 58.1% (97/167; 51–65%) | 52.5% (31/59; 40–65%) | 61.3% (38/62; 49–72%) | 60.9% (28/46; 46–74%) | $0.0213 |
| always-sonnet | 61.1% (102/167; 54–68%) | 55.9% (33/59; 43–68%) | 71.0% (44/62; 59–81%) | 54.3% (25/46; 40–68%) | $0.0754 |
| always-opus | 73.1% (122/167; 66–79%) | 74.6% (44/59; 62–84%) | 74.2% (46/62; 62–83%) | 69.6% (32/46; 55–81%) | $0.2279 |
| always-fable | 70.1% (117/167; 63–76%) | 76.3% (45/59; 64–85%) | 67.7% (42/62; 55–78%) | 65.2% (30/46; 51–77%) | n/a |
| always-haiku | 50.9% (85/167; 43–58%) | 49.2% (29/59; 37–62%) | 48.4% (30/62; 36–61%) | 56.5% (26/46; 42–70%) | $0.0075 |


Hard tier (n=46): laya-cheap 28/46 vs laya-shipped 34/46; paired b=6 (only cheap right), c=12 (only shipped right).

**GATE: FAIL** (rule frozen 2026-11-11, variant `fable`, INTERIM)

* FAIL hard tier (n=46): laya-cheap - laya-shipped = -13.0 pts, 95% paired bootstrap CI [-30.4, +4.3], lower bound must be > -5.0 (info: Newcombe [-30.2, +5.2], McNemar b=6 c=12 p=0.238)
* FAIL overall (n=167): laya-cheap 58.1% (CI 50.5-65.3%) must be >= keywords 62.3% (CI 54.7-69.3%)
* info mean cost/task: laya-cheap $0.0213, keywords $0.0997, laya-shipped $0.1434 (reported, not traded against accuracy)

Under the **old** rule this data *passed* (laya-cheap hard tier 60.9% vs keywords 54.3% and always-sonnet 54.3%, no regression by CI or McNemar). It fails the frozen rule on both criteria. The interim is not the gate: the final verdict is the `panel` variant on all 200 tasks (`GATE-RESULT.md`, written automatically by `gate-final.sh` after the stage-2 regrade).

