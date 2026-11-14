# Label-sheet protocol: the 120 real prompts

The label sheet (120 real prompts with human labels for kind and difficulty) is a **frozen held-out audit**.
It is not training data and not a tuning set.

1. **Freeze first.** Before anyone opens the labels, record Laya's predictions for the 120 prompts, the
   classifier build (hashes) and every threshold (`simple_below`, `demanding_above`, hedge settings), in a file
   committed with its date. Opening the labels before that commit voids the audit.
2. **No tuning on it.** If a threshold has to change, tune on separate calibration examples (grouped nested
   cross-validation), call that a development estimate, and collect a new untouched set for the next audit.
3. **Group the prompts** by project or task family so related prompts cannot leak across a split.
4. **Label mapping** (declared before scoring): `simple` -> simple, `standard` -> normal,
   `demanding` -> demanding, `hard` -> demanding. Report `hard` separately as well; Nectra has three tiers.
5. **Score** kind accuracy, ordinal tier error, harmful under-routing (a demanding or hard prompt sent to a
   simple tier), and the resulting model choices, not only exact tier agreement.
6. **Report uncertainty.** With 120 prompts the worst-case 95% interval is about +-9 points, and zero harmful
   errors still leaves a one-sided 95% upper bound of about 2.5%. Per-kind or hard-task claims need the sample
   size within that subgroup.
