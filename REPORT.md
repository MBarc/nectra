# Laya task classifier: report

**Result.** The run-4 model scores **312/324 (96.3%) on `fresh-test-2.json`**.
That set is the gate, and no model saw it before this evaluation. The bar is
over 95%.

On latency, the gate evaluation that emitted the model measured **202
ms/message**, under the 300 ms bar, on a loaded desktop.

- The same architecture measured **104 ms/message** on an idle desktop (run 3).
- The three run-4 evaluations measured 172, 418 and 202 ms. All ran on a shared
  desktop under load: Roblox Studio at 33–65% CPU, and under 4 GB free on C:.
- The accuracy figures are deterministic: identical in every evaluation.

The 418 ms evaluation was REFUSED by the script, and the 202 ms evaluation
passed and emitted `tools/laya/work/model`. The first evaluation (172 ms) also
passed, but its emit failed because the disk was full (see Emit below).

All numbers below are through Nectra: `nectra eval` with the int8-embedding
ONNX model, on CPU via ONNX Runtime.

## Accuracy

| set | role | run 4 (this) | run 3 (refused) | old model |
|---|---|---|---|---|
| `fresh-test-2.json` (324) | **gate**, unseen until now | **312 (96.3%)** | not run | not run |
| `dev.json` (324) | per-epoch dev, rubric tie-breaks | 308 (95.1%) | 296 (91.4%) | not run |
| `fresh-test.json` (323) | reported; its failures were seen | 310 (96.0%) | 295 (91.3%) | 278 (86.1%) |
| `old60-rubric.json` (60) | reported; used in old tuning | 56 (93.3%) | 55 (91.7%) | not run |

The keyword baseline scores 99/323 on `fresh-test.json`.

### Gate confusion (fresh-test-2, expected -> predicted, 12 misses)

| pair | n | example |
|---|---|---|
| refactoring -> code_review | 2 | "reviewer said my code is messy, clean up OrderService without changing what it does" |
| writing -> general | 2 | "Haiku about a failing unit test, please." |
| planning -> writing | 1 | "Rewriting our legacy PHP app: how should I approach it, which parts first…" |
| code_review -> general | 1 | "Grade my two-sum solution: is it idiomatic Python?…" |
| debugging -> data_analysis | 1 | "invoice totals off by a cent, sum() over float prices gives 0.30000000000000004" |
| data_analysis -> code_review | 1 | "tickets per label in this jira export + avg time to close" |
| debugging -> code_review | 1 | "Can you review this function? It throws KeyError…" |
| refactoring -> code_generation | 1 | "class components -> function components + hooks for Dashboard.jsx" |
| code_generation -> refactoring | 1 | "…Leave it exactly as it is, but add a binary_search(…)" |
| code_generation -> debugging | 1 | "fetchUser() works fine, i just want retry w/ exponential backoff added for 503s" |

No pair accounts for more than 2 misses. The residual errors are the usual
boundaries:

- a review word plus a failure, or a clean-up request phrased as a review;
- "works fine, but add X" (a denied failure read as debugging);
- short creative or editing requests read as general.

### Dev confusion per epoch (from `resumable_train.py --dev`, in training)

- **Epoch 1/2: 310/324 (95.7%).** code_generation->data_analysis 2,
  code_review->data_analysis 2, and ten pairs with 1 each.
- **Epoch 2/2: 309/324 (95.4%).** 15 pairs, 1 each. Nothing systematic left.

The exported int8 model scores 308/324 through Nectra. One prompt flips
between the fp32 weights in training and the int8-embedding ONNX model.

## Latency (CPU, desktop, ONNX Runtime via Nectra)

| measurement | fresh-test-2 | dev | fresh-test | old60 | conditions |
|---|---|---|---|---|---|
| run 3, 2026-10-20 21:55 | – | – | 104 ms | 86 ms | idle desktop |
| run 4, eval 1, 2026-10-21 13:47 local | 172 ms | 253 ms | 246 ms | 216 ms | disk filled up during this eval |
| run 4, eval 2, 14:03 | 418 ms | 494 ms | 369 ms | 312 ms | Roblox Studio running, CPU 37% before start |
| run 4, eval 3, 14:08 (**emitted**) | 202 ms | 216 ms | 220 ms | 216 ms | CPU 33–65% before start |

`ms_per_message` is the fastest of 3 timed passes over the gate set (the other
sets get one pass).

Runs 3 and 4 use the same graph: mmBERT-base, int8 embedding table, 718 MB on
disk. Only the weights differ, so per-message compute is identical. The spread
in the table is load on the shared desktop, not the model.

To re-measure the emitted model on an idle machine:

```
python tools/laya/retrain --resume 20261020-220908 \
  --model tools/laya/work/model --ort <onnxruntime.dll>
```

## Training history

All training ran on builder (michael@192.168.5.31, 16-vCPU Opteron, CPU only)
with laya pinned at d74e8be.

- **8 threads** beat 16 and 4 on builder. A micro-step (batch 8) takes about
  15–17.5 s.
- **Item count** is the compute budget.

| run | data | start | epochs | result |
|---|---|---|---|---|
| 3 | round 1: 3967 examples -> 4959 records (9519 items/epoch) | mmBERT-base (`multilingual`) | 3 | Killed by the 2026-10-20 power outage in epoch 2. Resumed weights-only from epoch 1. Scored 91.3% on fresh-test; REFUSED. |
| 4 | round 2: 5144 examples -> 6430 records (12,460 items/epoch, 390 updates) | run 3's checkpoint | 2 | dev 95.7% then 95.4%; **gate 96.3%** |

Run 4 started 2026-10-21 02:49 UTC. Epoch 1 finished at 09:25 UTC and epoch 2
at 17:27 UTC; export finished at 17:44 UTC.

**Round-2 data** came from 130 generator batches: 40 prompts per (label, style)
batch, `claude -p --model sonnet`.

- 5197 written, 5183 unique.
- 9 dropped as near-duplicates of test prompts.
- 39 dropped where the labeler disagreed with the writer.

Three hard-negative styles were added for round 2:

- a denied or set-aside failure next to the real task;
- borrowed verbs ("review", "analyze") on a different task;
- background about code, logs or an incident before the ask.

## Eval sets and labelling

Full process: `eval/laya/fresh-test.README.md`.

- **Writers.** Three writers per round, given briefs that differ on purpose.
  They could read only `RUBRIC.md`.
- **Labelers.** Two blind labelers per set. Every set got full agreement after
  adjudicating the prompts flagged unsure: 11 on dev, 13 on fresh-test-2.
- **Rubric freeze.** The rubric tie-breaks for the confusable pairs were
  decided on dev only. They were committed in 7fa16a7 before fresh-test-2 was
  labelled (64ef5bc), and before any model saw fresh-test-2.
- **Models used.** This iteration used only Claude models (Opus and Sonnet) for
  writing, labelling and adjudication.
- **Caveat.** Every writer, labeler and adjudicator was a language model
  applying a written rubric. Writers were asked for prompts with one clearly
  best label, so real traffic with genuinely ambiguous messages will score
  lower.

## `tools/laya/retrain`

The interface is documented in `tools/laya/README.md`. One command does all
of it:

1. regenerates data;
2. trains, locally or on `--host`;
3. exports ONNX and makes the int8 version;
4. evaluates every set through Nectra;
5. emits `tools/laya/work/model` **only** if the gate set beats `--bar` (0.95)
   within `--max-ms` (300).

Otherwise it exits 2 and writes `report.json`.

Changes in this iteration:

- **Resume.** `resumable_train.py` wraps laya's training loop. After every
  epoch it saves `resume.pt` (weights, AdamW, scheduler, RNG), and a rerun
  picks up from it. Older checkpoints with no `resume.pt` resume weights-only.
  - `--resume RUN` reuses a run directory.
  - The remote job runs in a named tmux session (`--session`).
  - The poller survives ssh dropouts and relaunches a session that died, up to
    `--relaunches` times.
- **Gate.** The gate is now `fresh-test-2.json`. The other sets are reported
  only. `--dev` prints dev accuracy and confusion every epoch.
- **Emit.** The model is copied into a `.partial` directory and renamed into
  place. A failed copy (run 4's first emit hit "not enough space on the disk")
  now leaves no half-written model.

## Open items

- **Latency** is load-sensitive on this desktop: 104 to 418 ms for the same
  graph. To confirm about 104 ms, re-measure on an idle machine (command above).
  The emitted model is in `tools/laya/work/model` (gitignored; not committed).
- **Publishing.** `src/laya.rs` still has `TODO` sha256 placeholders for the
  release assets. Publishing weights needs Michael's go-ahead; nothing was
  uploaded.
- **Desktop disk** is at 99.6% (about 2 GB free). That is not from this work:
  I deleted my own stale exports (2.7 GB), and free space kept falling.
- The lumina VM was not used. No Codex/OpenAI models were used in this
  iteration.
