| policy | n measured | accuracy | 95% CI | over-routing | exact | mean cost/task |
|---|---:|---:|---|---:|---:|---:|
| current | 48/48 | 89.6% | 77.8–95.5% | 79.2% | 10.4% | $0.1012 |
| proposed | 48/48 | 87.5% | 75.3–94.1% | 29.2% | 58.3% | $0.0622 |
| laya-shipped | 48/48 | 87.5% | 75.3–94.1% | 81.2% | 6.2% | $0.7807 |
| laya-cheap | 48/48 | 87.5% | 75.3–94.1% | 18.8% | 68.8% | $0.0193 |
| always-sonnet | 48/48 | 89.6% | 77.8–95.5% | 83.3% | 6.2% | $0.0771 |
| always-fable | 48/48 | 91.7% | 80.5–96.7% | 87.5% | 4.2% | $0.9233 |

| policy | vs baseline | verdict |
|---|---|---|
| current | always-sonnet | no accuracy gain (+0.0 pts, CIs overlap) and not cheaper |
| current | always-fable | matches accuracy (-2.1 pts, CIs overlap) at lower cost |
| proposed | always-sonnet | matches accuracy (-2.1 pts, CIs overlap) at lower cost |
| proposed | always-fable | matches accuracy (-4.2 pts, CIs overlap) at lower cost |
| laya-shipped | always-sonnet | no accuracy gain (-2.1 pts, CIs overlap) and not cheaper |
| laya-shipped | always-fable | matches accuracy (-4.2 pts, CIs overlap) at lower cost |
| laya-cheap | always-sonnet | matches accuracy (-2.1 pts, CIs overlap) at lower cost |
| laya-cheap | always-fable | matches accuracy (-4.2 pts, CIs overlap) at lower cost |

Per tier (accuracy = in-acceptable-set, Wilson 95% CI):

| tier | policy | n measured | accuracy | 95% CI | over-routing | under-routing |
|---|---|---:|---:|---|---:|---:|
| 1 | current | 17/17 | 100.0% | 81.6–100.0% | 94.1% | 0.0% |
| 1 | proposed | 17/17 | 94.1% | 73.0–99.0% | 23.5% | 5.9% |
| 1 | laya-shipped | 17/17 | 70.6% | 46.9–86.7% | 70.6% | 29.4% |
| 1 | laya-cheap | 17/17 | 100.0% | 81.6–100.0% | 23.5% | 0.0% |
| 1 | always-sonnet | 17/17 | 100.0% | 81.6–100.0% | 94.1% | 0.0% |
| 1 | always-fable | 17/17 | 82.3% | 59.0–93.8% | 82.3% | 17.6% |
| 2 | current | 18/18 | 88.9% | 67.2–96.9% | 83.3% | 11.1% |
| 2 | proposed | 18/18 | 88.9% | 67.2–96.9% | 38.9% | 11.1% |
| 2 | laya-shipped | 18/18 | 100.0% | 82.4–100.0% | 94.4% | 0.0% |
| 2 | laya-cheap | 18/18 | 83.3% | 60.8–94.2% | 22.2% | 16.7% |
| 2 | always-sonnet | 18/18 | 88.9% | 67.2–96.9% | 83.3% | 11.1% |
| 2 | always-fable | 18/18 | 100.0% | 82.4–100.0% | 94.4% | 0.0% |
| 3 | current | 13/13 | 76.9% | 49.7–91.8% | 53.8% | 23.1% |
| 3 | proposed | 13/13 | 76.9% | 49.7–91.8% | 23.1% | 23.1% |
| 3 | laya-shipped | 13/13 | 92.3% | 66.7–98.6% | 76.9% | 7.7% |
| 3 | laya-cheap | 13/13 | 76.9% | 49.7–91.8% | 7.7% | 23.1% |
| 3 | always-sonnet | 13/13 | 76.9% | 49.7–91.8% | 69.2% | 23.1% |
| 3 | always-fable | 13/13 | 92.3% | 66.7–98.6% | 84.6% | 7.7% |

Hard tier verdicts. Regression on the hard tier (tier 3) = the candidate's accuracy CI upper bound is below the baseline's point estimate; baselines are always-sonnet and current.

| policy | vs baseline | hard-tier verdict |
|---|---|---|
| current | always-sonnet | no regression: 76.9% (CI 49.7–91.8%) vs 76.9% |
| proposed | always-sonnet | no regression: 76.9% (CI 49.7–91.8%) vs 76.9% |
| proposed | current | no regression: 76.9% (CI 49.7–91.8%) vs 76.9% |
| laya-shipped | always-sonnet | no regression: 92.3% (CI 66.7–98.6%) vs 76.9% |
| laya-shipped | current | no regression: 92.3% (CI 66.7–98.6%) vs 76.9% |
| laya-cheap | always-sonnet | no regression: 76.9% (CI 49.7–91.8%) vs 76.9% |
| laya-cheap | current | no regression: 76.9% (CI 49.7–91.8%) vs 76.9% |

Accuracy = in-acceptable-set over tasks where the pick was measured; CI is Wilson 95%. Cost = mean API-equivalent list-price cost of the picked model per task, over priced picks only.
