# Classifier vs cascade (offline, rh-v2, no model calls)

Script: `classifier_vs_cascade.py`; data: `rhv2-snapshot.json` (per-task pass/cost for four models, Laya's predicted kind/tier/difficulty from `nectra route`, the imported table). 194 tasks, one run per model (rep 0).

## Method and assumptions

- Escalation trigger: the recorded objective check failed -> next rung. A cascade's outcome is exact (every task has one recorded run per model, so no independence assumption); a cascade passes if any attempted rung passed.
- Cost = sum of the recorded `cost_usd` of **every attempted rung**, failed ones included. `cost_usd` is nectra's API-equivalent (usage x list prices, `eval/chat-savings/prices.json`, the registry's price table). Subscription capacity and retry latency are not in it. Mean cost per run: luna $0.0055, haiku $0.0077, sonnet $0.0862, opus $0.2169. Cascades climb in that cost order.
- **Detection is free here**: the cascade sees the objective check result. In real use `Completed` is not success, so the real escalation signal is weaker and every cascade row is optimistic by that amount (the classifier-start rows less so, since they escalate less).
- 'laya + table as on master' uses the imported table (`rh-v2-table.nectra.json`): non-inferiority certified, which at ~22 tasks per kind leaves **opus only** for every measured kind; unmeasured kinds (architecture, data_analysis, general) and the 27 prompts Laya did not classify fall back to haiku-first. The tier-start rule is a fixed, unfitted rule (no in-sample table), so it is not tuned to these runs.
- Oracle = the cheapest model whose run passed (cost = that single run); on tasks nobody passed it counts a failure at the cheapest run's cost. It is a lower bound no policy can reach.
- **Clustering.** The rh-v2 data carries no project identifier (tasks are standalone fixtures). The only grouping available is the task-id family (rh-code, rh-debug, ... = the authored kind, 9 clusters of 21-23). CIs below are 95% bootstrap percentile intervals, 4000 resamples, seed fixed, shown both **task-level** and **clustered by id family**. Nine clusters is few, so the clustered intervals are rough and the task-level ones are probably too tight; read the pair as a range.
- **Noise data.** `noise-repeat-20261112` holds no complete tasks (every model 0/0), so no flip-rate correction is applied. At n=194 a 1-2 task difference is noise regardless.

## Policies

| policy | pass | pass 95% CI (task / family) | $/task | $/task 95% CI (task / family) | escalation rate |
|---|---:|---|---:|---|---:|
| laya + table as on master (certified, opus-only) | 155/194 (79.9%) | 0.742-0.856 / 0.694-0.896 | 0.1668 | 0.1469-0.1873 / 0.1150-0.2181 | 7% |
| laya tier-start (simple luna / normal haiku / demanding sonnet, predicted) | 170/194 (87.6%) | 0.830-0.918 / 0.782-0.954 | 0.0799 | 0.0632-0.0994 / 0.0466-0.1248 | 27% |
| tier-start with authored tier (upper bound for that rule) | 172/194 (88.7%) | 0.840-0.928 / 0.796-0.959 | 0.0832 | 0.0655-0.1022 / 0.0499-0.1264 | 28% |
| haiku-first cascade | 170/194 (87.6%) | 0.830-0.918 / 0.776-0.954 | 0.0649 | 0.0472-0.0842 / 0.0310-0.1108 | 23% |
| luna-first cascade | 174/194 (89.7%) | 0.851-0.938 / 0.818-0.964 | 0.0591 | 0.0414-0.0786 / 0.0280-0.1023 | 51% |
| luna-first, capped at 2 escalations (luna>haiku>sonnet) | 169/194 (87.1%) | 0.825-0.918 / 0.778-0.949 | 0.0272 | 0.0209-0.0336 / 0.0155-0.0412 | 51% |
| sonnet-first cascade | 163/194 (84.0%) | 0.789-0.892 / 0.745-0.929 | 0.1388 | 0.1216-0.1572 / 0.1021-0.1793 | 22% |
| always-opus | 147/194 (75.8%) | 0.696-0.814 / 0.649-0.865 | 0.2169 | 0.2019-0.2333 / 0.1820-0.2598 | 0% |
| oracle (cheapest model that passed) | 174/194 (89.7%) | 0.851-0.938 / 0.812-0.964 | 0.0156 | 0.0105-0.0215 / 0.0113-0.0201 | 0% |

## Where the money and the failures go

Under-routing = tasks the policy failed although some model passed them (cost: lost quality). Nobody-passed tasks are failures no routing could fix. Over-routing waste = $/task spent above the cheapest passing model, split into **failed attempts** (cascade steps that did not pass) and **overshoot** (the final passing model dearer than the cheapest passing one). A third cost is **spent on tasks no model passes**: a cascade climbs the whole ladder on them. No routing can fix those tasks, but a cap or a stop rule can stop paying for them.

| policy | under-routing fails | nobody passed | failed-attempt cost $/task | overshoot $/task | spent on nobody-passed tasks $/task |
|---|---:|---:|---:|---:|---:|
| laya + table as on master (certified, opus-only) | 19 | 20 | 0.0016 | 0.1003 | 0.0280 |
| laya tier-start (simple luna / normal haiku / demanding sonnet, predicted) | 4 | 20 | 0.0032 | 0.0188 | 0.0374 |
| tier-start with authored tier (upper bound for that rule) | 2 | 20 | 0.0044 | 0.0229 | 0.0377 |
| haiku-first cascade | 4 | 20 | 0.0031 | 0.0034 | 0.0379 |
| luna-first cascade | 0 | 20 | 0.0054 | 0.0001 | 0.0389 |
| luna-first, capped at 2 escalations (luna>haiku>sonnet) | 5 | 20 | 0.0029 | 0.0001 | 0.0124 |
| sonnet-first cascade | 11 | 20 | 0.0056 | 0.0635 | 0.0369 |
| always-opus | 27 | 20 | 0.0000 | 0.1443 | 0.0265 |
| oracle (cheapest model that passed) | 0 | 20 | 0.0000 | 0.0000 | 0.0008 |

## Per authored kind (pass / $ per task; n<20 flagged)

| kind | n | P1 | P2 | P3 | P4 | P5 | P6 | P7 | P8 | P9 |

P1 = laya + table as on master (certified, opus-only); P2 = laya tier-start (simple luna / normal haiku / demanding sonnet, predicted); P3 = tier-start with authored tier (upper bound for that rule); P4 = haiku-first cascade; P5 = luna-first cascade; P6 = luna-first, capped at 2 escalations (luna>haiku>sonnet); P7 = sonnet-first cascade; P8 = always-opus; P9 = oracle (cheapest model that passed)

|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| code_generation | 21 | 11/21 $0.295 | 12/21 $0.239 | 12/21 $0.238 | 12/21 $0.218 | 13/21 $0.205 | 12/21 $0.072 | 12/21 $0.254 | 11/21 $0.291 | 13/21 $0.027 |
| code_review | 22 | 21/22 $0.154 | 22/22 $0.020 | 22/22 $0.038 | 22/22 $0.010 | 22/22 $0.009 | 22/22 $0.009 | 22/22 $0.087 | 21/22 $0.185 | 22/22 $0.007 |
| debugging | 22 | 20/22 $0.096 | 22/22 $0.028 | 22/22 $0.043 | 22/22 $0.024 | 22/22 $0.023 | 21/22 $0.014 | 21/22 $0.102 | 18/22 $0.192 | 22/22 $0.019 |
| long_context | 22 | 17/22 $0.105 | 18/22 $0.063 | 19/22 $0.080 | 18/22 $0.067 | 20/22 $0.032 | 20/22 $0.019 | 15/22 $0.165 | 14/22 $0.197 | 20/22 $0.009 |
| planning | 22 | 22/22 $0.181 | 22/22 $0.044 | 22/22 $0.030 | 22/22 $0.009 | 22/22 $0.010 | 22/22 $0.010 | 22/22 $0.068 | 22/22 $0.218 | 22/22 $0.007 |
| refactoring | 22 | 13/22 $0.295 | 20/22 $0.089 | 20/22 $0.087 | 20/22 $0.066 | 20/22 $0.071 | 20/22 $0.040 | 19/22 $0.209 | 12/22 $0.348 | 20/22 $0.018 |
| research | 22 | 20/22 $0.039 | 21/22 $0.056 | 21/22 $0.036 | 21/22 $0.022 | 21/22 $0.024 | 20/22 $0.011 | 20/22 $0.076 | 19/22 $0.144 | 21/22 $0.011 |
| tool_use | 20 | 14/20 $0.157 | 15/20 $0.094 | 16/20 $0.093 | 15/20 $0.094 | 16/20 $0.083 | 15/20 $0.035 | 14/20 $0.155 | 13/20 $0.177 | 16/20 $0.019 |
| writing | 21 | 17/21 $0.184 | 18/21 $0.096 | 18/21 $0.112 | 18/21 $0.084 | 18/21 $0.086 | 17/21 $0.038 | 18/21 $0.141 | 17/21 $0.199 | 18/21 $0.023 |

## Where a classifier could beat the cascade

Skipping a cheap rung pays only on tasks that rung fails (saves that rung's cost) and loses on tasks it would have passed (pays the next rung's price). A classifier that flags 'will fail on rung R' with precision p breaks even at p* = (c_next - c_R) / c_next, using mean costs.

| rung R | passes | fails | cost of its failed attempts, $/task (the most skipping can save) | c_R | c_next | break-even precision p* | actual fail share |
|---|---:|---:|---:|---:|---:|---:|---:|
| luna | 96 | 98 | 0.0035 | 0.0055 | 0.0077 | 29% | 51% |
| haiku | 150 | 44 | 0.0021 | 0.0077 | 0.0862 | 91% | 23% |
| sonnet | 151 | 43 | 0.0218 | 0.0862 | 0.2169 | 60% | 22% |

Does Laya's signal predict where a rung fails? AUC (0.5 = no information) of the signal for 'the rung's run failed':

| signal | luna fails | haiku fails | sonnet fails | n |
|---|---:|---:|---:|---:|
| Laya predicted difficulty | 0.55 | 0.49 | 0.43 | 167 |
| authored tier (human label) | 0.57 | 0.46 | 0.47 | 194 |

Laya vs authored labels: kind 118/194, tier 84/194; 27 prompts unclassified (keyword fallback).

## Conclusion

- **No classifier row beats the plain cascade.** Luna-first (no classifier) is cheaper than every Laya row and passes at least as many: $0.0591/task, 174/194, against Laya tier-start $0.0799, 170/194 and Laya + the table as on master $0.1668, 155/194. Haiku-first (170/194, $0.0649) also beats both on cost at equal or better pass. With authored labels the tier-start rule is no better (172/194, $0.0832), so label error is not the limit.
- **Laya as it stands on master is the worst of the routed policies**: the certified table is opus-only, and opus alone passes 147/194; the table costs 2.6x haiku-first and passes 15 fewer tasks. That is the cost of requiring certification at ~22 tasks per kind, not a classifier error.
- **The signal is not there.** Laya's predicted difficulty has AUC 0.55 / 0.49 / 0.43 for luna / haiku / sonnet failing (0.5 = nothing); the authored tier is no better. Skipping haiku needs at least 91% precision to pay (haiku is so cheap that a skip costs 11x when wrong); skipping luna needs only 29% but can save at most $0.0035/task. The most any skip rule could save over luna-first is its failed-attempt cost ($0.0054/task) plus overshoot ($0.0001).
- **The money left on the table is elsewhere**: luna-first spends $0.0389/task climbing the full ladder on the 20 tasks no model passes. Capping at two escalations costs 5 passes and $0.0319/task saved (169/194, $0.0272). That is a stop rule, not a classifier.
- **Where a classifier could still win**: (1) a *stop/give-up* predictor for tasks that will fail at every rung (largest unpaid cost above); (2) a *difficulty* signal with AUC well above 0.5 for luna/haiku failure (none measured here); (3) kinds or workloads where the cheap rung fails often and a mid rung is cheap relative to opus (the p* column). Caveats: one run per model and task, 194 tasks, free failure detection, no project identifier (clusters are id families), noise data empty, and costs are notional API list prices.

## Reproduce

`python3 eval/calibration/classifier_vs_cascade.py` (reads `rhv2-snapshot.json`); add `--data ~/nectra-calibration/eval/calibration` to rebuild the snapshot from the run files.
