| policy | n measured | accuracy | 95% CI | over-routing | exact | mean cost/task |
|---|---:|---:|---|---:|---:|---:|
| current | 48/48 | 89.6% | 77.8–95.5% | 79.2% | 10.4% | $0.1012 |
| proposed | 48/48 | 87.5% | 75.3–94.1% | 29.2% | 58.3% | $0.0622 |
| always-sonnet | 48/48 | 89.6% | 77.8–95.5% | 83.3% | 6.2% | $0.0771 |
| always-fable | 48/48 | 91.7% | 80.5–96.7% | 87.5% | 4.2% | $0.9233 |

| policy | vs baseline | verdict |
|---|---|---|
| current | always-sonnet | no accuracy gain (+0.0 pts, CIs overlap) and not cheaper |
| current | always-fable | matches accuracy (-2.1 pts, CIs overlap) at lower cost |
| proposed | always-sonnet | matches accuracy (-2.1 pts, CIs overlap) at lower cost |
| proposed | always-fable | matches accuracy (-4.2 pts, CIs overlap) at lower cost |

Accuracy = in-acceptable-set over tasks where the pick was measured; CI is Wilson 95%. Cost = mean API-equivalent list-price cost of the picked model per task, over priced picks only.
