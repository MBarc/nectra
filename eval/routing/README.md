# Routing accuracy

`scripts/routing_accuracy.py` is the reproducible routing measurement.

It consumes calibration artifacts from the sibling `nectra-calibration`
worktree by default:

```powershell
python scripts/routing_accuracy.py build
python scripts/routing_accuracy.py fit
python scripts/routing_accuracy.py eval --split heldout --registry config/models.toml --binary target/release/nectra.exe --out eval/routing/hand-set
python scripts/routing_accuracy.py eval --split heldout --registry config/models.calibrated.toml --binary target/release/nectra.exe --out eval/routing/fitted
```

`build` writes `oracle.json`. For each task, the oracle is the least-cost
measured model whose quality is within `--margin` (default 0.05) of that
task's best quality. The JSON retains every measured quality and the stable
70/30 train/held-out split.

`fit` writes `config/models.calibrated.toml` and `fit.json`. Scores are
quality-weighted capability means plus the calibration harness's log-scaled
speed/cost scores, using only the training split. Roles are inferred from the
best measured task family. The default registry is never modified.

`eval` runs the actual router with all configured providers and once more with
each provider removed, recording exact-match, acceptable-set, under-routing,
over-routing, provider, task-type, and fallback results. Run it once with the
hand-set registry and once with the fitted registry; the two `report.json`
files are the held-out comparison. Use `jq` or PowerShell's
`ConvertFrom-Json` to inspect `in_acceptable_set_rate` and the `rows` list for
the largest misroutes.

The calibration root, task root, results root, binary, oracle margin, and
output directory are all overrideable with command-line options so later
Codex/Gemini grades can be incorporated without changing the harness.

## Side-by-side report (use this for every routing report)

```bash
python3 scripts/routing_accuracy.py compare --reports current=<dir> proposed=<dir> --out <dir>
```

Writes `comparison.md` and `comparison.json`: in-acceptable-set accuracy with a
Wilson 95% CI, over-routing, exact-match and mean API-priced cost per task for
each registry plus always-sonnet and always-fable, and a verdict per pair on
whether routing beat the fixed model or matched it at lower cost. Fable has no
API price, so its cost and over-routing show n/a. `selftest` checks the maths.

## 200-prompt held-out set

`new-heldout-prompts.json` (48) plus `new-heldout-prompts-2.json` (152) make
200 prompts: tiers 70/76/54 (35/38/27%), nine task types. Written only, not
measured; the calibration worker measures them in the Nov 5 pass (materialise
both files as `rh-*` tasks). `scripts/dedup_heldout.py` checks the 152 against
Laya training states, the 63 non-`rh-` calibration tasks, the first 48 and each
other (exact, and word-3-shingle Jaccard >= 0.5); the result is
`dedup-heldout.json`.

## Fresh holdout expansion

`new-heldout-prompts.json` contains 48 newly authored prompts (17 tier-1,
18 tier-2, 13 tier-3) across nine task types. They remain out of
`oracle.json` until the calibration worker has run and graded every enabled
model on them. The worker should materialize these manifest entries as normal
calibration tasks, then rerun `build` so measured-only coverage and the 70/30
split include them. The current 15-task report is not the final headline after
this expansion.

## laya-cheap candidate (third candidate for the Nov 5 comparison)

`config/models.laya-cheap.toml` + `config/laya-cheap.toml` (router settings,
passed with `eval --settings`). Same models and scores as `models.toml`; only
`prefers` moves: haiku takes simple, general and demanding, sonnet takes
planning, opus and fable claim nothing. Thresholds and `stakes_weight` stay at
the shipped 0.2 / 0.6 / 0.30.

Why the role map and not thresholds: with Laya, the model is chosen by Laya's
role answer, not its difficulty tier. On the 63 training tasks no
`simple_below` / `demanding_above` / `stakes_weight` setting changed cost without
losing accuracy. Laya answered demanding for 25 of the 63 (fable) and 32 of the
interim 48, while the oracle picks haiku for 55 of 63 and 41 of 48. Haiku's
eight training misses fall under every Laya role, so no map that keeps Laya's
judgment beats near-always-haiku on this data.

Reproduce (on builder; `scripts/laya_cheap_fit.py`, train only; `assert_clean`
fails if any `rh-` id or held-out prompt text reaches the oracle, probe or fit):

```bash
python3 scripts/laya_cheap_fit.py oracle          # coverage-*, codex-v1*, default, hard-pilot, crossgrade*; Claude models
python3 scripts/laya_cheap_fit.py probe train --binary <master nectra>   # Laya pinned files in $XDG_DATA_HOME/nectra/laya
python3 scripts/laya_cheap_fit.py fit             # -> laya-cheap/fit.txt
python3 scripts/routing_accuracy.py eval --registry config/models.laya-cheap.toml --settings config/laya-cheap.toml \
  --binary <master nectra> --providers anthropic --out eval/routing/laya-cheap/laya-cheap   # with interim oracle.json there
```

Interim held-out 48 (Claude models, Fable-graded), real `nectra route` built
from master 158938a with the pinned Laya model (`laya-cheap/comparison`):

| policy | accuracy | 95% CI | over-routing | exact | $/task |
|---|---:|---|---:|---:|---:|
| current (keywords) | 89.6% | 77.8–95.5% | 79.2% | 10.4% | $0.1012 |
| proposed (keywords) | 87.5% | 75.3–94.1% | 29.2% | 58.3% | $0.0622 |
| laya-shipped | 87.5% | 75.3–94.1% | 81.2% | 6.2% | $0.7807 |
| laya-cheap | 87.5% | 75.3–94.1% | 18.8% | 68.8% | $0.0193 |
| always-sonnet | 89.6% | 77.8–95.5% | 83.3% | 6.2% | $0.0771 |
| always-fable | 91.7% | 80.5–96.7% | 87.5% | 4.2% | $0.9233 |

`current` and `proposed` route on keywords (the eval's default settings);
`laya-shipped` is what nectra-bin runs. A Laya eval now fails instead of
scoring a keyword fallback after a Laya timeout.
