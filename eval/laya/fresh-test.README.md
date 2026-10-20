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
