# Fresh Laya test set

`fresh-test.json`: 323 prompts, labelled by `RUBRIC.md`. This is the bar for
the local classifier: `tools/laya/retrain` refuses to emit a model that scores
at or below 95% on it. **Never train on it, tune prompts against it, or pick a
model by it other than the final pass/fail gate.** If it ever has to change,
add new prompts and say so in the report; don't edit labels to make a model pass.

How it was made (2026-10-20):

- **Prompts.** Three writers, each told to write 110 messages across the ten
  labels, with different briefs: (a) developers in a terminal: terse,
  lowercase, typos, pasted errors and snippets; (b) non-specialist and mixed
  users: small business, students, hobbyists, ~15 in non-native English;
  (c) deliberately hard boundary cases with misleading keywords. The writers
  never saw the training data or the old eval sets. Exact and normalised
  duplicates of each other and of `eval/{holdout,generalization,tasks}.json`
  were removed (5), leaving 325.
- **Labels.** Two independent labelers (different models) labelled all 325
  blind (prompt only, no writer intent) against the rubric. They agreed on all
  325. 12 prompts were flagged (one labeler marked unsure, or the writer
  intended a different label). Every label was then reviewed by an
  adjudicator. Two prompts the rubric genuinely can't settle were dropped
  (f006, f206). Ids are the order prompts were shuffled in; `fresh-test.json` keeps that order without them. The decisions and reasons are in `fresh-test-adjudication.json`.
- **Caveat.** All three roles (writers, labelers, adjudicator) were language
  models applying a written rubric, not humans. The writers were asked for
  prompts with one clearly best label, so this set measures the classifier on
  decidable messages. Real chat traffic contains more truly ambiguous
  messages, where any classifier will score lower.

`old60-rubric.json` is the old 60 (`eval/holdout.json` +
`eval/generalization.json`) relabelled by the same rubric. Only one label
changes: "look into what caused the outage" goes from research to debugging
(rubric rule 2). The old labels were Jev's, and those sets were used during
earlier tuning, so they are reported but don't gate anything.

## fresh-test-2.json and dev.json (2026-10-20, second round)

After the first retrain scored 91.3% on `fresh-test.json`, its failures had
been seen. Steering more training at them would have turned that set into a
model-selection set. So `fresh-test.json` is now reported only, and
`fresh-test-2.json` is the bar.

- **Prompts.** Three new writers got the same three briefs as round one and
  could read only `RUBRIC.md`: no eval set, no training data. They wrote 660
  prompts. 12 near-duplicates (token Jaccard >= 0.8) of every earlier set and
  of the generated training data were dropped. The remaining 648 were split at
  random, stratified by writer and label, into `dev.json` (324) and
  `fresh-test-2.json` (324).
- **Dev first.** Two blind labelers (Opus, Sonnet) labelled dev. They agreed
  on every prompt, and with the writers. The 11 unsure notes were settled by
  the rubric (`dev-adjudication.json`). The coordinator's confusable pairs were
  then adjudicated on dev only, and their tie-breaks written into
  `RUBRIC.md`:
  - "review why X fails" (debugging vs code_review);
  - porting and writing tests (code_generation vs refactoring);
  - legal and "is X current" questions (research vs general).

  The rubric was committed (7fa16a7) 107 s before fresh-test-2 (64ef5bc),
  too little time to label 324 prompts twice, so git shows only that it was
  committed first, not that it was frozen before labelling began. What
  limits the damage: both labelers agreed with each other and with the
  writers on all 324 prompts, so no tie-break changed a fresh-test-2 label.
- **Fresh-test-2.** Two new blind labelers applied the frozen rubric. They
  agreed with each other and with the writers on all 324 prompts. The 13
  unsure notes are recorded in `fresh-test-2-adjudication.json`. Nothing was
  dropped. The set was committed before any model was evaluated on it.
- **Uses.** `dev.json` is for model selection: per-epoch confusion and
  generator styles. `fresh-test-2.json` is only the retrain gate.
- **Caveat.** Same as round one: writers, labelers and adjudicator were all
  language models.

## fresh-test-3.json (2026-10-21, third round)

fresh-test-2's 324 items give a 95% interval of 93.6-97.9% around 96.3%, too
wide to say the model is above 95%. fresh-test-3 is a larger held-out set
built to narrow it. It is evaluated once, by hand, and never used by
`retrain` or for training.

- **Built by** `tools/laya/heldout.py`, with Claude models only. Sonnet
  writers got the same three briefs and only `RUBRIC.md`, frozen since
  7fa16a7 and unchanged since.
- **Near-duplicates dropped:** 42 near-duplicates (token Jaccard >= 0.8) of
  the training prompts and follow-ups, of every earlier set, and of each
  other. A recheck against all 19,253 training and eval texts found none at
  >= 0.8 and 55 at >= 0.6.
- **Labelled** by two blind labelers (Opus, Sonnet). They agreed on
  1029/1031, and an Opus adjudicator settled the other 2 by the rubric
  (`fresh-test-3-adjudication.json`). The adjudication shows no model output.
- **Labels committed** in 5a66555 (15:56 local), before the only evaluation
  (16:34). Its output is `fresh-test-3-result.json`.
- **Do not train on, tune against, or re-select with this set.** A future
  model needs a new set.
