# tools/laya: retraining the local classifier

`tools/laya/retrain` rebuilds the model behind `nectra classifier laya` in one
command. Run it when the task or role criteria change, the model registry's
roles change, or a provider CLI or model change calls for it. It never
publishes anything. A model reaches `--out` only if it beats the bar.

```sh
cargo build --release
python tools/laya/retrain --host michael@192.168.5.31 --ort /path/to/onnxruntime.dll --out ~/laya-model
```

Training needs Python, the generator CLI and ssh. No GPU is required.

## What it does

1. **Data.** Reads `eval/laya/criteria.json` (the ten task labels and the six
   routing roles, which a Rust test keeps identical to `src/jev.rs`,
   `src/policy.rs` and `src/laya.rs`) and `eval/laya/RUBRIC.md`.
   - **Writing:** the generator writes 40 messages for each of 100 (label,
     style) batches.
   - **Filtering:** duplicates and near-duplicates of the test sets are dropped
     (token Jaccard >= 0.8).
   - **Relabelling:** a second, blind generator call labels every message
     (task, difficulty 0-2, role).
   - **Disagreements:** messages whose blind label differs from the label they
     were written for are dropped as ambiguous.

   The result is cached in `<work>/data-<hash>.json`, keyed on criteria,
   rubric and styles. Unchanged criteria reuse the data; `--regenerate`
   forces new data.
2. **Training records.** Builds the exact questions `src/laya.rs` asks (bare
   labels, short instructions). Each question is one encoder pass in
   training, so on CPU the item count is the budget.
   - **Task** (the gated question) is asked on every example; **difficulty**
     and **role** on every other one.
   - **Role options:** a third of the role records offer only the roles that
     models in `--registry` claim (`prefers`), because that is the list Nectra
     offers.
   - **Follow-ups:** one in eight examples adds two records: a follow-up
     (`new_task` false, same task) and an unrelated next message (`new_task`
     true).

   About 9.9k items per epoch from ~4k examples.
3. **Train.** Runs `laya-train` (full fine-tune, RLCD loss) from the
   `multilingual` (mmBERT-base) Laya checkpoint. With `--host`, it runs on
   that Linux machine over ssh in a tmux session `laya-<run>`. Retrain polls
   it, so a dropped connection doesn't kill training. On first use the host
   gets `~/nectra-laya` with a laya checkout at the commit `Cargo.toml` pins,
   plus a CPU torch venv. Without `--host` it trains locally with `--python`;
   use `--adamw8bit` on a 6 GB GPU.

   **Interruptions.** Training goes through `resumable_train.py`, which
   writes `ckpt/resume.pt` (weights, AdamW and scheduler state, RNG) after
   every epoch and continues from it when started again.
   - **Host reboot:** if the tmux session disappears without finishing, retrain
     relaunches it, up to `--relaunches` times, and the relaunch resumes.
   - **Retrain itself died:** run `retrain --resume <run>` with the same
     arguments.
   - **What can be lost:** at most the epoch in progress.
   - **Older runs:** a `checkpoint_latest` with an `epochs_done` file but no
     `resume.pt` resumes from its weights, with fresh AdamW moments and the LR
     schedule fast-forwarded.
4. **Export.** Uses laya's `export_onnx.py` (fp32, external weights). Then
   `int8_embeddings.py` stores the token-embedding table as int8 with
   per-row scales (1.29 GB -> 0.70 GB). Matrix multiplies stay fp32.
   `--fp32` skips this step.
5. **Evaluate through Nectra.** Writes a throwaway config with
   `backend = "laya"`, `model_dir` set to the export,
   `allow_unpinned_model = true` (a new model is not the published one) and
   no per-message deadline. It checks with
   `nectra route` that the model, not the keyword fallback, answered. Then it
   runs `nectra eval` on `eval/laya/fresh-test-2.json` (the bar). It also
   runs `dev.json`, `fresh-test.json` and `old60-rubric.json`, reported only. Latency is wall time per
   message with process start and model load subtracted.
6. **Emit or refuse.** It copies the model to `--out` only if fresh-set
   accuracy is above `--bar` and latency is at most `--max-ms`. The copy goes
   to `<out>.partial`. Two renames swap it in (`out` -> `out.old`, then
   `.partial` -> `out`). A swap that was killed partway is finished or undone
   by the next run.

## Interface (stable; other tools call this)

| Argument | Default | Meaning |
|---|---|---|
| `--out DIR` | `<work>/model` | Where a passing model is written. It replaces the directory's contents and is a valid `classifier.model_dir`. |
| `--work DIR` | `tools/laya/work` (git-ignored) | Data cache and `runs/<id>/` (train.jsonl, report.json). |
| `--registry PATH` | `config/models.toml` | Registry whose claimed roles shape the role options. |
| `--generator CMD` | `claude -p --model sonnet` | Any command that reads a prompt on stdin and prints the answer on stdout. Writes and labels the data. |
| `--regenerate` | off | Rebuild data even if the criteria are unchanged. |
| `--per-batch N`, `--parallel N` | 40, 4 | Data size (100 batches) and concurrent generator calls. |
| `--stakes N` | 0 | Add N prompts per (`STAKES_STYLES` style, task) batch: long task specs and short requests, high-stakes (untrusted input, auth, side effects that must not repeat, data loss, races) against routine work that only mentions security. Cached apart from the main data as `stakes-<hash>.json`. These ask difficulty and role on every example. |
| `--replay N` | all | Train on a random N of the main examples, to fine-tune an earlier run's `ckpt` (`--base`) on new data without paying for the whole set. |
| `--host USER@HOST` | local | Train over ssh on a Linux host. |
| `--threads N` | 8 | `OMP_NUM_THREADS` on the training host. On builder (16 vCPU Opteron 6272, 8 FPUs) 8 threads ran 16 s/step, 16 threads 36 s, 4 threads 22 s. |
| `--python`, `--laya-src` | this python, none | Local training: python with laya installed, and a laya checkout for the exporter. |
| `--base` | multilingual | Starting checkpoint: a laya name, or a checkpoint path on the training host. A path continues from an earlier run's `ckpt`. From Git Bash, set `MSYS_NO_PATHCONV=1`, or Git Bash rewrites `/home/...` into a Windows path. |
| `--epochs`, `--micro-batch`, `--grad-accum`, `--device`, `--seed`, `--adamw8bit` | 3, 8, 8, auto, 0, off | Training knobs. |
| `--fp32` | off | Keep the embedding table fp32 (the default int8 table halves the size). |
| `--resume RUN` | none | Continue `runs/RUN` from its last finished epoch instead of starting a new run. |
| `--session NAME` | `laya-<run>` | tmux session name on `--host`. |
| `--relaunches N` | 3 | Relaunch a `--host` session that died without finishing (for example, a reboot). |
| `--model DIR` | none | Skip data and training. Evaluate an already-exported model and emit it if it passes. |
| `--nectra PATH` | `target/release/nectra[.exe]` | The Nectra binary that runs the evaluation. |
| `--ort PATH` | `$ORT_DYLIB_PATH` | ONNX Runtime 1.31 library, copied next to the model for evaluation. |
| `--bar F` | 0.95 | Fresh-set accuracy the model must exceed. |
| `--max-ms F` | 300 | Slowest acceptable ms per message. |
| `--latency-runs N` | 3 | Time the fresh set N times and keep the fastest (background load only adds time). |
| `--data-only` | off | Stop after building the training records. |

**Exit codes:** `0` model emitted (or `--data-only` done), `2` trained but
below the bar (nothing emitted), `1` error.

**`report.json`** goes in `runs/<id>/` and, when emitted, also in `--out`. It
holds:
- `run`, `laya_rev`, `bar`, `max_ms`, `examples`, `train_items`,
  `claimed_roles`;
- `eval` by corpus, each with `passed`, `total`, `accuracy`,
  `ms_per_message`, a `confusion` map (`"expected -> actual": count`) and
  `failures`;
- `emitted`: the path, set only after the model is in place at `--out`, or
  null; or `error`.

**Installing an emitted model** is up to the caller. Point
`[classifier] model_dir` at `--out` (with the ONNX Runtime library inside)
and set `allow_unpinned_model = true` (Nectra warns on every load),
or publish it as a new release, update the hashes in `src/laya.rs`, and let
`nectra classifier laya` download it. Publishing is Michael's call.

## The test set is fixed

`eval/laya/fresh-test-2.json` gates every run, and nothing is trained on it,
tuned against it or selected by it. The generator never sees it, and
near-duplicates of it (and of `dev.json`, `fresh-test-3.json` and the older
sets) are filtered out of generated prompts and their follow-ups.
`fresh-test-3.json` is never run by `retrain`; it was evaluated once, by hand. `long-spec-tuning.json` (long specs and their short forms, gold `tier` labels) is **not** held out: `STAKES_STYLES` were written from its prompts, and their paraphrases pass the near-duplicate filter, so its tier and demanding numbers are optimistic. `long-spec-heldout.json` (40 specs and requests, half needing the demanding model, written after training from sources the styles never saw) is the held-out demanding-routing set. `nectra eval` reports tier accuracy, demanding TP/FP/FN for both the classifier's tier and the model the router picks, and how many prompts fell back to keywords; `--min-tier-accuracy` makes tier misses fail the run. `dev.json` is the model-selection set: training prints its
task accuracy and confusion after every epoch. `fresh-test.json`, the first
bar, is reported only, because its failures have been seen. Don't edit its labels to make a model pass. See
`eval/laya/fresh-test.README.md`.

## Publishing (CI)

`.github/workflows/laya-release.yml` turns an emitted model into a release and
a pin-update PR. Retraining stays on your machine (it needs the generator CLI
and the training host), so the model reaches CI as a **staging release**:

```sh
# --out is the retrain output; ORT_DIR holds the three runtimes under their release names.
gh release create laya-staging-N --prerelease --title "Laya staging N" \
  OUT/model.onnx OUT/model.onnx.data OUT/rl_agent_config.json OUT/ONNXRUNTIME-LICENSE \
  OUT/tokenizer/tokenizer.json OUT/tokenizer/tokenizer_config.json \
  ORT_DIR/onnxruntime-win-x64.dll ORT_DIR/libonnxruntime-linux-x64.so ORT_DIR/libonnxruntime-macos-arm64.dylib
```

Then run the workflow (Actions > laya-release > Run workflow) with `model_tag`
`laya-model-N`. It runs only on demand: there is no tag-push trigger, because a
pushed tag would run the workflow file as it is at that commit, with write
permissions. It:

1. downloads the staged files and hashes them;
2. rewrites `RELEASE` and the model pins in `src/laya.rs`
   (`scripts/update_laya_pins.py`). Only the model files (`model*`,
   `tokenizer/*`, `rl_agent_config.json`) are repinned; the staged ONNX Runtime
   binaries and `ONNXRUNTIME-LICENSE` must match their existing pins, or the run
   stops here. Upgrading the runtime is a reviewed source change, not a retrain;
3. builds Nectra and evaluates the model on a held-out set
   (`scripts/laya_gate.py`, default `fresh-test-3.json`). It fails if accuracy is
   below the current model's (`fresh-test-3-result.json`, 97.7%) or below
   `min_accuracy`;
4. pushes `laya-pins-N` and opens a PR with the eval numbers;
5. creates the `laya-model-N` release with `scripts/release-laya.sh`, which
   re-checks every file against the new pins and adds `SHA256SUMS`. Merge the
   PR only after this step succeeded: the pins point at this release.

Each gate run on `fresh-test-3` counts as seeing it; pass another `eval_set`
(a new held-out set with a `-result.json` next to it) once it has been
used for selection. Nothing is released until you run the workflow.

**If a run fails.** Before step 5 nothing is public except the `laya-pins-N`
branch and its PR; fix the cause and re-run, which force-pushes the branch and
refreshes the PR. If step 5 itself fails, `gh release create` may have left a
partial `laya-model-N` release, and a re-run stops at "already released". Check
it with `gh release view laya-model-N`; if it is incomplete, delete it with
`gh release delete laya-model-N --cleanup-tag --yes` and re-run. If the run
is abandoned, close the PR and delete `laya-pins-N`.
