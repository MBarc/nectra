# Laya gate compare (INTERIM)

Generated 2026-11-11T13:43:11Z by scripts/laya_gate_prep/gate_compare.py. Grades: Fable only (167 of 200 held-out tasks, Claude models only: opus, sonnet, haiku, fable); Astra and the codex models are not in this oracle. NOT the gate number: the final run needs the panel / drop-self / swap-fable variants after the Astra half and the stage-2 Fable regrade.

Policies: keywords = master registry, classifier local; laya-shipped = master registry + shipped Laya alpha.2 (default thresholds); laya-cheap = config/models.laya-cheap.toml + config/laya-cheap.toml from MBarc/nectra-routing-accuracy (46e60a4). Models other than the four Claude ones are disabled so every pick is gradable. The Laya model was staged from ~/laya-stage-alpha2 (hash-verified by nectra classifier laya --verify-only).

## Gate compare INTERIM (Fable-graded, Claude models) — variant `fable`, oracle margin 0.05, 167 of 200 tasks usable (tier counts: T1=59, T2=62, T3=46)

| policy | all acc (95% Wilson CI) | tier1 acc (95% Wilson CI) | tier2 acc (95% Wilson CI) | tier3 acc (95% Wilson CI) | mean $/task |
|---|---|---|---|---|---|
| keywords | 62.3% (104/167; 55–69%) | 57.6% (34/59; 45–69%) | 72.6% (45/62; 60–82%) | 54.3% (25/46; 40–68%) | $0.0997 |
| laya-shipped | 71.9% (120/167; 65–78%) | 69.5% (41/59; 57–80%) | 72.6% (45/62; 60–82%) | 73.9% (34/46; 60–84%) | $0.1434 |
| laya-cheap | 58.1% (97/167; 51–65%) | 52.5% (31/59; 40–65%) | 61.3% (38/62; 49–72%) | 60.9% (28/46; 46–74%) | $0.0213 |
| always-sonnet | 61.1% (102/167; 54–68%) | 55.9% (33/59; 43–68%) | 71.0% (44/62; 59–81%) | 54.3% (25/46; 40–68%) | $0.0754 |
| always-opus | 73.1% (122/167; 66–79%) | 74.6% (44/59; 62–84%) | 74.2% (46/62; 62–83%) | 69.6% (32/46; 55–81%) | $0.2279 |
| always-fable | 70.1% (117/167; 63–76%) | 76.3% (45/59; 64–85%) | 67.7% (42/62; 55–78%) | 65.2% (30/46; 51–77%) | n/a |
| always-haiku | 50.9% (85/167; 43–58%) | 49.2% (29/59; 37–62%) | 48.4% (30/62; 36–61%) | 56.5% (26/46; 42–70%) | $0.0075 |

Hard-tier (tier 3) check, candidate vs baseline. REGRESSION = CIs separate (candidate upper < baseline lower) OR exact McNemar on paired tasks significant at 95% with the candidate worse; b/c = tasks only the candidate / only the baseline got right:

* keywords vs always-sonnet: 54.3% vs 54.3% (+0.0 pts, n=46) b=3 c=3 p=1.000 -> no regression
* keywords vs always-opus: 54.3% vs 69.6% (-15.2 pts, n=46) b=3 c=10 p=0.092 -> point-lower
* keywords vs always-fable: 54.3% vs 65.2% (-10.9 pts, n=46) b=10 c=15 p=0.424 -> point-lower
* keywords vs always-haiku: 54.3% vs 56.5% (-2.2 pts, n=46) b=9 c=10 p=1.000 -> point-lower
* keywords vs laya-shipped: 54.3% vs 73.9% (-19.6 pts, n=46) b=7 c=16 p=0.093 -> point-lower
* keywords vs laya-cheap: 54.3% vs 60.9% (-6.5 pts, n=46) b=7 c=10 p=0.629 -> point-lower
* laya-shipped vs always-sonnet: 73.9% vs 54.3% (+19.6 pts, n=46) b=16 c=7 p=0.093 -> no regression
* laya-shipped vs always-opus: 73.9% vs 69.6% (+4.3 pts, n=46) b=10 c=8 p=0.815 -> no regression
* laya-shipped vs always-fable: 73.9% vs 65.2% (+8.7 pts, n=46) b=4 c=0 p=0.125 -> no regression
* laya-shipped vs always-haiku: 73.9% vs 56.5% (+17.4 pts, n=46) b=13 c=5 p=0.096 -> no regression
* laya-shipped vs keywords: 73.9% vs 54.3% (+19.6 pts, n=46) b=16 c=7 p=0.093 -> no regression
* laya-shipped vs laya-cheap: 73.9% vs 60.9% (+13.0 pts, n=46) b=12 c=6 p=0.238 -> no regression
* laya-cheap vs always-sonnet: 60.9% vs 54.3% (+6.5 pts, n=46) b=11 c=8 p=0.648 -> no regression
* laya-cheap vs always-opus: 60.9% vs 69.6% (-8.7 pts, n=46) b=6 c=10 p=0.454 -> point-lower
* laya-cheap vs always-fable: 60.9% vs 65.2% (-4.3 pts, n=46) b=9 c=11 p=0.824 -> point-lower
* laya-cheap vs always-haiku: 60.9% vs 56.5% (+4.3 pts, n=46) b=3 c=1 p=0.625 -> no regression
* laya-cheap vs keywords: 60.9% vs 54.3% (+6.5 pts, n=46) b=10 c=7 p=0.629 -> no regression
* laya-cheap vs laya-shipped: 60.9% vs 73.9% (-13.0 pts, n=46) b=6 c=12 p=0.238 -> point-lower

Reading: laya-cheap at 58.1% overall is below keywords (62.3%) and laya-shipped (71.9%) on this oracle but at $0.021/task against $0.100 and $0.143. Hard tier: laya-cheap 60.9% (46–74%) vs always-sonnet 54.3% and keywords 54.3%: no hard-tier regression by CI or paired test. It is below always-opus (69.6%) and laya-shipped (73.9%) on tier 3 and in tier 2 (61.3% vs 72.6%) without reaching significance (n=46/62). Caveat: the oracle margin is 0.05 and the 33 tool/writing tasks without Fable grades are excluded, so tool_use and writing are under-represented.
