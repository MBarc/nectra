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

P1 = laya + table as on master (certified, opus-only); P2 = laya tier-start (simple luna / normal haiku / demanding sonnet, predicted); P3 = tier-start with authored tier (upper bound for that rule); P4 = haiku-first cascade; P5 = luna-first cascade; P6 = luna-first, capped at 2 escalations (luna>haiku>sonnet); P7 = sonnet-first cascade; P8 = always-opus; P9 = oracle (cheapest model that passed)

| kind | n | P1 | P2 | P3 | P4 | P5 | P6 | P7 | P8 | P9 |
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

## Free failure detection: what nectra's real signal sees

What triggers an escalation in nectra today (`src/chat.rs`, `src/headless.rs`): a **failed turn** (the provider run errored: deadline, rate limit, tool error) steps the task up the ladder; the user can ask for a stronger model (`/model`, a demanding follow-up); and a headless caller passes `--escalate-from` (optionally with `--prior-verified`, a verifier verdict). **Nothing in nectra judges whether a completed answer is right** (`Completed` is not success). In these runs the only recorded signal of that kind is `status == failed` (`error_class`). The objective check is a grader the real system does not have.

Confusion matrix per rung: 'nectra would escalate' = the run's turn failed; truth = the objective check failed (the run did not pass).

| rung | runs | escalate & check failed | escalate & check passed | no escalate & check failed (missed) | no escalate & check passed | recall of check failures |
|---|---:|---:|---:|---:|---:|---:|
| luna | 194 | 0 | 0 | 98 | 96 | 0.0% |
| haiku | 194 | 2 | 0 | 42 | 150 | 4.5% |
| sonnet | 194 | 1 | 0 | 42 | 151 | 2.3% |
| opus | 194 | 1 | 0 | 46 | 147 | 2.1% |

So on these runs the real signal fires on a handful of deadline failures and misses essentially every wrong answer. Cascades re-run with it (a failed turn always escalates; otherwise a wrong answer is accepted with probability 1-d, where d is the share of wrong answers some detector would catch; false alarms assumed zero; expected values are exact, not sampled). d=1 is the oracle check used above; 'real' is d=0 plus the failed-turn trigger.

| cascade | detection of wrong answers | pass (expected) | pass 95% CI (task / family) | $/task | $/task 95% CI (task / family) |
|---|---|---:|---|---:|---|
| luna-first | oracle check (d=1) | 174.0/194 (89.7%) | 0.851-0.938 / 0.812-0.964 | 0.0591 | 0.0419-0.0782 / 0.0265-0.1022 |
| luna-first | d=0.75 | 151.2/194 (77.9%) | 0.734-0.821 / 0.705-0.850 | 0.0327 | 0.0240-0.0425 / 0.0165-0.0536 |
| luna-first | d=0.5 | 130.9/194 (67.5%) | 0.625-0.723 / 0.593-0.754 | 0.0169 | 0.0130-0.0214 / 0.0096-0.0254 |
| luna-first | d=0.25 | 112.6/194 (58.1%) | 0.521-0.639 / 0.486-0.679 | 0.0088 | 0.0072-0.0107 / 0.0058-0.0125 |
| luna-first | **real: failed turns only** | 96.0/194 (49.5%) | 0.423-0.567 / 0.374-0.605 | 0.0055 | 0.0048-0.0061 / 0.0039-0.0074 |
| haiku-first | oracle check (d=1) | 170.0/194 (87.6%) | 0.830-0.923 / 0.784-0.954 | 0.0649 | 0.0472-0.0856 / 0.0313-0.1105 |
| haiku-first | d=0.75 | 164.3/194 (84.7%) | 0.802-0.894 / 0.744-0.932 | 0.0448 | 0.0330-0.0572 / 0.0227-0.0742 |
| haiku-first | d=0.5 | 159.2/194 (82.1%) | 0.771-0.870 / 0.713-0.912 | 0.0291 | 0.0221-0.0367 / 0.0161-0.0447 |
| haiku-first | d=0.25 | 154.8/194 (79.8%) | 0.742-0.851 / 0.694-0.891 | 0.0177 | 0.0134-0.0232 / 0.0108-0.0254 |
| haiku-first | **real: failed turns only** | 151.0/194 (77.8%) | 0.716-0.835 / 0.670-0.878 | 0.0105 | 0.0072-0.0155 / 0.0062-0.0158 |

With the real signal a cascade is just its first rung (luna alone 96/194, haiku alone 150/194): the cascade's whole pass-rate advantage comes from a detector the system does not have. Any real deployment needs a verifier (tests, a grader model, the user), and its recall d sets the result between the d=0 and d=1 rows.

**What is missing, and the minimal re-run.** Not recorded: any automated verdict on a completed answer (a verifier result, a test run by nectra, a user reaction). Recorded and usable: `status`, `error_class`, `answer_chars`, the answer text (`*.answer.md`) and the objective check. To measure a real verifier's recall d and false-alarm rate, no task or model needs re-running: grade the saved answers. 776 answers, about 0.84M input and 0.12M output tokens (chars/4, 400 tokens of rubric and 150 of verdict per answer). At the registry's list prices the whole set costs, by grader: haiku $0.14; sonnet $2.84; opus $5.69. Comparing each verdict with the objective check gives d and the false-alarm rate to plug into the table above.

## Stop rule: capping escalations

Oracle check as the trigger. 'Nobody-passed' = the 20 tasks no model passes; they climb the whole ladder under no cap. Pass lost = tasks the uncapped cascade passes and the capped one does not.

| cascade | cap (max escalations) | pass | pass lost vs no cap | $/task | $/task saved | spent on nobody-passed $/task | saved there |
|---|---:|---:|---:|---:|---:|---:|---:|
| luna-first | 0 | 96/194 | 78 | 0.0055 | 0.0537 | 0.0010 | 0.0379 |
| luna-first | 1 | 159/194 | 15 | 0.0100 | 0.0491 | 0.0019 | 0.0369 |
| luna-first | 2 | 169/194 | 5 | 0.0272 | 0.0319 | 0.0124 | 0.0265 |
| luna-first | 3 (= no cap) | 174/194 | 0 | 0.0591 | 0.0000 | 0.0389 | 0.0000 |
| haiku-first | 0 | 150/194 | 20 | 0.0077 | 0.0572 | 0.0010 | 0.0369 |
| haiku-first | 1 | 165/194 | 5 | 0.0291 | 0.0358 | 0.0114 | 0.0265 |
| haiku-first | 2 (= no cap) | 170/194 | 0 | 0.0649 | 0.0000 | 0.0379 | 0.0000 |

## Conclusion

- **No classifier row beats the plain cascade.** Luna-first (no classifier) is cheaper than every Laya row and passes at least as many: $0.0591/task, 174/194, against Laya tier-start $0.0799, 170/194 and Laya + the table as on master $0.1668, 155/194. Haiku-first (170/194, $0.0649) also beats both on cost at equal or better pass. With authored labels the tier-start rule is no better (172/194, $0.0832), so label error is not the limit.
- **Laya as it stands on master is the worst of the routed policies**: the certified table is opus-only, and opus alone passes 147/194; the table costs 2.6x haiku-first and passes 15 fewer tasks. That is the cost of requiring certification at ~22 tasks per kind, not a classifier error.
- **The signal is not there.** Laya's predicted difficulty has AUC 0.55 / 0.49 / 0.43 for luna / haiku / sonnet failing (0.5 = nothing); the authored tier is no better. Skipping haiku needs at least 91% precision to pay (haiku is so cheap that a skip costs 11x when wrong); skipping luna needs only 29% but can save at most $0.0035/task. The most any skip rule could save over luna-first is its failed-attempt cost ($0.0054/task) plus overshoot ($0.0001).
- **The money left on the table is elsewhere**: luna-first spends $0.0389/task climbing the full ladder on the 20 tasks no model passes. Capping at two escalations costs 5 passes and $0.0319/task saved (169/194, $0.0272). That is a stop rule, not a classifier.
- **Free failure detection is the dominant caveat, and it is large.** nectra's own signal (a failed turn) catches 0-5% of wrong answers on these runs, so a cascade driven by it is just its first rung: luna-first 96/194 at $0.0055, haiku-first 150/194 at $0.0105 (against 174 and 170 with the objective check). Every cascade advantage above needs a verifier with high recall; at d=0.5 luna-first drops to about 131/194 and haiku-first to about 159/194. A verifier (tests, a grader model, the user) matters more than any classifier. The saved answers allow measuring a grader's d with no new model runs (cost in the section above).
- **Where a classifier could still win**: (1) a *stop/give-up* predictor for tasks that will fail at every rung (largest unpaid cost above); (2) a *difficulty* signal with AUC well above 0.5 for luna/haiku failure (none measured here); (3) kinds or workloads where the cheap rung fails often and a mid rung is cheap relative to opus (the p* column). Caveats: one run per model and task, 194 tasks, free failure detection, no project identifier (clusters are id families), noise data empty, and costs are notional API list prices.

## Reproduce

`python3 eval/calibration/classifier_vs_cascade.py` (reads `rhv2-snapshot.json`); add `--data ~/nectra-calibration/eval/calibration` to rebuild the snapshot from the run files.
