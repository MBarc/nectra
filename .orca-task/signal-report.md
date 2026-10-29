# Laya stakes-signal report

Model: laya-model-1-alpha.2, builder run 20261023-204433/int8.
The implementation commit is 9e57377113fc6ffd085fc247c65ec4aeb5de36ad.

## Held-out result

The baseline is the already-recorded result supplied with the task and its
existing eval commit; the held-out corpus was not rerun for a baseline. The
after column is the single permitted run of
eval/laya/long-spec-heldout.json.

| metric | before | after |
|---|---:|---:|
| tier accuracy | 22/40 (0.55) | 26/40 (0.65) |
| classifier demanding TP / FP / FN | 9 / 2 / 11 | 13 / 3 / 7 |
| router demanding TP / FP / FN | 9 / 1 / 11 | 13 / 3 / 7 |
| keyword fallbacks | 0 | 0 |

This improves demanding recall by four, but **fails the held-out no-new-FP
objective**: router FP increased from 1 to 3 (and classifier FP from 2 to 3).
I did not retune on the judge or run it a second time.

## Tuning result and selected blend

eval/laya/long-spec-tuning.json:

| metric | weight 0.0 | chosen weight 0.30 |
|---|---:|---:|
| task accuracy | 24/30 (0.8) | 24/30 (0.8) |
| tier accuracy | 30/57 (0.5263157894736842) | 41/57 (0.7192982456140351) |
| classifier demanding TP / FP / FN | 9 / 2 / 13 | 16 / 2 / 6 |
| router demanding TP / FP / FN | 9 / 2 / 13 | 16 / 2 / 6 |
| keyword fallbacks | 0 | 0 |

tools/laya/sweep-stakes swept weights 0.0 through 0.6 in 0.05 increments
and selected the smallest weight that maximised router TP without exceeding
the baseline router FP of 2. Weights 0.30, 0.35, and 0.40 all produced
16 / 2 / 6; 0.45 was the first weight to increase FP (16 / 3 / 6).

The stakes score is capped at 1.0; prompts with no constraint phrase are
capped at 0.20, and the blended difficulty is capped at 1.0. The final blend
is laya_difficulty + 0.30 * stakes_score.

Option 2 (a logistic head) was not attempted: option 1 met the stated
tuning-set selection rule. Its held-out generalisation failure was learned
only in the final one-shot judge run, after which model selection was closed.

## Fresh-test-3 gate

| metric | before | after |
|---|---:|---:|
| task accuracy | 1011/1031 (0.9806013579049466) | 1011/1031 (0.9806013579049466) |
| keyword fallbacks | 0 | 0 |

The required floor of 1010/1031 is met before and after.

## Builder checks

- cargo fmt --check: passed.
- cargo clippy --all-targets -- -D warnings: passed.
- cargo test: all library and preceding integration suites passed
  (172 library tests, then 2 and 31 integration tests). The known master
  failure remained:
  native_terminal::cancelling_native_work_does_not_save_a_fake_answer
  (terminal did not exit within deadline); that suite printed
  5 passed; 1 failed.
- The alpha.2 directory differs from the published pins only at
  ONNXRUNTIME-LICENSE, so evals used
  classifier.allow_unpinned_model = true. Every reported eval had zero
  keyword fallbacks.
