# Laya task classifier: report

**Result.** The run-4 model scores **96.3% (312/324) on held-out
fresh-test-2, 95% Wilson CI 93.6-97.9%**. That set is the gate, and no model
saw it before this evaluation. The gate's bar is a point estimate over 95%,
which it meets. The interval does not by itself show accuracy above 95%.

On **fresh-test-3**, a larger held-out set labelled and committed before its
single evaluation, it scores **97.7% (1007/1031), 95% Wilson CI 96.6-98.4%**.
The lower bound clears 95%. See "fresh-test-3" below.

On latency, the gate evaluation that emitted the model measured a **mean
of 202 ms/message**, under the 300 ms bar, on a loaded desktop. That is a
mean, not a per-message bound. Per message, the steady state measured a
median of 173 ms, a p95 of 338 ms and a maximum of 543 ms. Nectra now enforces
a per-message deadline at runtime (see "Runtime latency").

- The same architecture measured **104 ms/message** on an idle desktop. That
  was run 3, a different set of weights; run 4 has not been measured idle.
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
| `fresh-test-3.json` (1031) | held out, evaluated once | **1007 (97.7%)** | not run | not run |
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

### fresh-test-3 (held out, evaluated once)

This set was built to narrow fresh-test-2's interval. The process is in
`eval/laya/fresh-test.README.md`.

- **Writing and labelling.** Claude models only, under the frozen rubric.
- **Near-duplicates.** Those of the training prompts and follow-ups and of
  every earlier set were dropped.
- **Labelling.** Two blind labelers agreed on 1029/1031 items, and an Opus
  adjudicator settled the other 2.
- **Order.** The labels were committed in 5a66555 before the evaluation.
- **The evaluation.** It ran once, on 2026-10-21 at 16:34 local, using the
  shipped (pinned) model with no deadline. Its output is
  `eval/laya/fresh-test-3-result.json`.
- **Not trained on.** Nothing was trained or tuned on this set.

**Result: 1007/1031 = 97.7%, 95% Wilson CI 96.6-98.4%.** The lower bound
clears 95%.

| class | recall |
|---|---|
| architecture | 104/106 (98.1%) |
| code_generation | 100/100 (100%) |
| code_review | 106/107 (99.1%) |
| data_analysis | 105/106 (99.1%) |
| debugging | 104/108 (96.3%) |
| general | 76/78 (97.4%) |
| planning | 101/107 (94.4%) |
| refactoring | 103/107 (96.3%) |
| research | 103/106 (97.2%) |
| writing | 105/106 (99.1%) |

There were 24 misses. The most common pairs were debugging -> code_review (2)
and refactoring -> debugging (2). Every other pair occurred once.

**Caveat.** This set comes from the same synthetic, rubric-shaped
distribution as fresh-test-2, so it narrows the interval on that
distribution, not on real traffic.

## Runtime latency (per message, in the product)

Measured with the shipped model on an i5-9600K at about 40% CPU from other
work, using
`NECTRA_LAYA_MODEL=<dir> cargo test --release -- --ignored latency --nocapture`.

| | |
|---|---|
| load, including sha256 of every file | 4376 ms |
| first message after load | 276 ms |
| steady state (323 dev messages) | median 173, p95 338, max 543 ms |
| over 300 ms | 36/323 |

What Nectra does about latency now:

- **Deadline.** Each message gets `classifier.laya_timeout_ms`, default
  1000. A late answer is dropped and the message is classified by keywords.
  `route` shows the reason ("laya unavailable: no answer within 1000 ms").
- **Why the default is 1000 ms.** It favours accuracy over about 0.5 s of
  routing latency, since a model turn takes seconds anyway. No measured
  message came near 1000 ms.
- **The low-latency option** is `laya_timeout_ms = 300`, the bar. It makes
  300 ms hold for every message, at a cost: on this loaded desktop dev.json
  scored 91.0% at 300 ms against 95.1% without a deadline.
- **Warm-up.** `nectra chat` starts loading at launch on a background thread
  and runs one throwaway message to pay ONNX Runtime's first-run cost.
  Messages that arrive in the first ~4.5 s use keywords instead of waiting.
- **One-shot commands.** `route` and `eval` wait for the load on their first
  message, outside the deadline.
- **Evaluation.** `retrain` evaluates with no deadline, so the gate measures
  the model rather than the machine.

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
  decided on dev only.
  - **What git shows.** They were committed in 7fa16a7, 107 s before
    fresh-test-2 (64ef5bc), and before any model saw fresh-test-2. That
    proves the commit order, not that the rubric was frozen before labelling
    began.
  - **Why it matters little.** Both labelers agreed with the writers on all
    324 prompts, so no tie-break decided a fresh-test-2 label.
- **Dev prompts in the rubric.** Two rubric examples are dev.json prompts
  verbatim:
  - the binary-search review;
  - the UK drone question.

  Every generator and labeler reads the rubric, so the dev numbers are not
  clean held-out figures. The rubric stays frozen, because changing it would
  change what fresh-test-3 was labelled under.
- **Follow-ups.** The training-data near-duplicate filter checked prompts
  only. Three run-4 training follow-ups match a `fresh-test.json` prompt
  verbatim; none matches fresh-test-2, fresh-test-3 or dev. `retrain` now
  filters follow-ups too.
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
- **Emit.** The model is copied into a `.partial` directory. Two renames
  then swap it in: `out` becomes `out.old`, `.partial` becomes `out`, and
  `out.old` is deleted.
  - **Interrupted swaps.** A swap killed partway is finished or undone by the
    next run. `test_retrain.py` kills it at each step to check this.
  - **The `emitted` field** in `report.json` is written only once the model
    is in place.
  - **Failed copies.** A failed copy (run 4's first emit hit "not enough space
    on the disk") or a failed rename leaves the old model as it was.
- **Pinning.** `classifier.model_dir` no longer turns hash pinning off. Any
  file that doesn't match its pin, including the ONNX Runtime library, needs
  `classifier.allow_unpinned_model = true`. `retrain`'s evaluation config sets
  that explicitly.

## Open items

- **Latency** is load-sensitive on this desktop: 104 to 418 ms for the same
  graph. Run 4 has not been measured idle; use the command above or the
  `latency` test.
  The emitted model is in `tools/laya/work/model` (gitignored; not committed).
- **Publishing.** The release assets are pinned in `src/laya.rs` and staged by
  `scripts/release-laya.sh`. Publishing the weights needs Michael's go-ahead,
  and nothing has been uploaded.
- **Desktop disk** is at 99.6% (about 2 GB free). That is not from this work:
  I deleted my own stale exports (2.7 GB), and free space kept falling.
- The lumina VM was not used. No Codex/OpenAI models were used in this
  iteration.
