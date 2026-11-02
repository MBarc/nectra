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
