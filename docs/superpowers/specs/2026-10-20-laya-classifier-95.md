# Laya classifier above 95%, with a repeatable retrain

Date: 2026-10-20. Branch from `laya` (43b02ca).

## Goal

Get the local Laya classifier (`nectra classifier laya`, `src/laya.rs`) above
95% task accuracy on a fresh, correctly labelled test set, measured through
`nectra eval`, at no more than 300 ms per message on CPU. Make retraining a
single command another tool can call.

## Decisions

- **Measure on a new set, not the old 60.** The old 60 were used for tuning and
  carry Jev's labels. `eval/laya/fresh-test.json` (323 prompts) was written by
  three independent writers, labelled blind by two labelers against
  `eval/laya/RUBRIC.md`, and adjudicated. It is the gate and is never trained
  on. The old 60, relabelled by the rubric (one label changes), are reported
  only.
- **Fix the labels, not the model size.** The best prior model (mmBERT-base,
  54/60) scores 86.1% on the fresh set. Its training labels were Jev's, which
  bound it to Jev's mistakes. New training data is generated per (label,
  style) and kept only when a blind second labelling agrees. Same encoder:
  ModernBERT-large was 1.9 s per message on CPU, and mmBERT-base is the only
  other pretrained Laya base.
- **One source of truth for criteria.** Task descriptions move to
  `jev::TASK_CRITERIA` (the same text Jev receives). `eval/laya/criteria.json`
  is the training copy, and a unit test fails if it drifts from
  `TASK_CRITERIA`, `policy::ROLES` or the question wording in `src/laya.rs`.
  Routing semantics are unchanged: Jev gets the same request, and the Laya
  questions are the same strings.
- **Training host.** builder (CPU-only, coordinator's choice; not the desktop
  GPU, not lumina). Retrain runs training over ssh in tmux and polls, so a
  dropped connection doesn't kill hours of work.
- **The gate is in the pipeline.** `tools/laya/retrain` exits 2 and emits
  nothing below the bar. Its interface is documented in `tools/laya/README.md`
  for the CLI auto-adapt worker.

## Cross-product contract

`tools/laya/retrain` (arguments, exit codes 0/1/2, `report.json` shape) is the
contract with the Nectra CLI auto-adapt worker. The model directory layout is
unchanged (`model.onnx`, `model.onnx.data`, `rl_agent_config.json`,
`tokenizer/`, plus the ONNX Runtime library), so `src/laya.rs` loads a
retrained model without code changes.

## Out of scope

Publishing weights and filling in the release hashes in `src/laya.rs` (needs
Michael). Model-registry discovery and `scripts/compat/**` (other workers).
