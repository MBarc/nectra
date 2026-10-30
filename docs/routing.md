# Task responsibilities and model access

Models declare which work they want. A registry entry carries a `prefers` list of
routing roles, and a prompt resolves to one role; a model claiming that role gets
the preference. Any model can claim a role, including generated `ollama/<name>`
and `gemini-*` ids.

A registry where **no** model declares `prefers` keeps the older behaviour, in
which the preference is matched against a fixed table of bundled model ids. That
keeps an installed registry routing as it always did until it opts in. `nectra
models` prints each model's declared roles and warns when a registry predates the
field.

Ordinary runtime loading never rewrites your registry. A model you delete stays
deleted; the compatibility adaptation tool is the explicit exception because it
may update an installed registry after observing a provider change.

The bundled registry declares these roles:

| Role | Task | Declared by |
| --- | --- | --- |
| `simple` | Short answers, small edits | haiku |
| `simple-coding` | Coding in balanced and fast mode | gpt-5.6-luna |
| `general` | Conversation, writing, research synthesis | sonnet |
| `planning` | Planning and architecture | opus |
| `coding` | Coding in best mode, or in balanced when nobody claims `simple-coding` | gpt-5.6-sol |
| `demanding` | Explicitly demanding or complex tasks | gpt-6-astra, fable |

When two models declare a role, the higher-scoring one leads and the other is the
nearest fallback. With `demanding`, a Claude-only setup sends hard work to fable,
and a mixed setup falls back from Astra to fable when Codex is rate-limited.

These are product routing preferences, not measured capability claims. Preferences
add 0.30 to the existing quality/speed/cost score; other eligible models remain
fallbacks. Fast mode prefers Haiku for nontechnical tasks and Luna for technical
ones. Balanced mode also sends ordinary coding to the `simple-coding` model first,
because in [`eval/chat-savings`](../eval/chat-savings/RESULTS.md) it passed every
check at 3-4% of the strong coders' cost. Explicit demanding-task signals (or a
demanding Jev difficulty) take precedence over both. Demanding work goes to the
`demanding` model whatever role the classifier filed it under, coding included;
only demanding planning stays with the `planning` model.

Demanding signals: complex, difficult, hardest, multi-step, multistep, end-to-end,
escalate, deep reasoning. Light-task signals: quick, simple, brief, briefly,
short, trivial, minor. These describe the size of the work, not its kind:
"summarize" and "extract" name a task and no longer count, because summarising a
large document is not cheap work. Without Jev, a non-coding message of four words or fewer ("thanks, that worked") also counts as light. These keyword signals are the `keywords` backend and the
fallback whenever the model is unavailable. With `[classifier] backend = "laya"`
the local Laya model classifies each message's task, difficulty and role instead.
It runs on this machine, so nothing leaves it. See [the model card](../tools/laya/MODEL_CARD.md).
Laya tends to rate long, risky specs as moderate, so `[routing] stakes_weight`
(default 0.30, 0 turns it off) adds a score computed from the task text: the
`=== TASK ===` section of a referenced Orca dispatch file, otherwise the prompt
itself, never a file's preamble. It counts constraint phrases such as "must
never" or "rollback", numbered requirements ("1." or "2)", not years or
decimals), bullets, length, and several named components or file paths (not
URLs or "and/or"). It applies only to Laya's difficulty, capped at 1, and was
fitted on `eval/laya/long-spec-tuning.json` with `tools/laya/sweep-stakes`. On
the held-out `long-spec-heldout.json` it lifts router demanding TP/FP/FN from
9/2/11 to 12/3/8: four more hard specs caught for one more false escalation.
The everyday sets (fresh-test, -2, -3, dev, old60) route identically at 0 and
0.30. Physical building prompts such as "plan how to build a shed" classify
as planning rather than coding. Responsibilities are defined in src/policy.rs;
capability estimates remain editable in the registry.

Use `/models` to see configured IDs and session observations. `use gpt-6-astra and
solve this complex problem` selects Astra for one turn. `/model haiku` pins a
preference; `/model auto` restores automatic routing.

## Measuring the classifier

Two corpora, with different jobs.

`eval/tasks.json` is the regression corpus. Every classifier fix is validated
against it, so it reports 100% and **cannot** tell you whether the classifier
generalises -- it only tells you nothing already fixed has broken.

`eval/generalization.json` is the control. Its prompts are deliberately never
tuned against. Raising its score legitimately means adding new unseen prompts and
re-measuring, not adjusting keywords until these particular sentences pass. A test
holds it to a floor, so a change that helps the regression corpus while hurting
unseen phrasing fails the build.

The gap between them is the honest picture: near 100% on the tuned corpus, around
half on unseen prompts. Almost every miss is a prompt containing none of the
keyword vocabulary, which no amount of additional keywords fixes in general --
classification is local lexical matching, not a semantic judge. Treat the floor as
a guard against regression, not as a target to optimise.

```sh
nectra eval eval/tasks.json
nectra eval eval/generalization.json
```

## Access and fallback

Installing a provider CLI does not prove account entitlement to a model. Models
start with unverified access. Actual requests validate provider acceptance; a
completed reply records that the last request succeeded. This does not promise
future access or attest which underlying model a provider alias resolves to.

Ollama is chat-only. Code generation, debugging, refactoring and review go to a model that can edit files whenever one is eligible, even under `prefer-local`; under `local-only` the decision's `reason` says the answer can only describe the change.

Recognized model-access/unsupported-model rejections exclude that model for the
session. Other models from the same provider remain eligible. Missing CLIs,
known rejections, login requirements and cooldowns are considered before dispatch.
If a requested model is unavailable, chat explains and picks another eligible
model for the same task. Local-only restrictions remain enforced.

Structured rejection before any work automatically retries, even after an explicit
model request. Each candidate is attempted at most once per message. If work has
started, or native telemetry cannot establish a pre-work boundary, the failed turn
is not replayed; subsequent turns can use alternatives. Native Claude hooks can
establish a prompt-before-tools boundary. Codex terminal failures without such
telemetry remain conservative; stream view provides structured failure tracking.
Unknown errors are not guessed to be entitlement failures. No hidden probe calls
are made, no credentials are read by the router, and no login state is changed.

`/model retry` clears model-access observations without replaying anything. Logging
in via `/login PROVIDER` also clears that provider's model observations. Restarting chat resets
model-access observations, but **cooldowns persist between runs** so a rate limit
you hit is not rediscovered by walking into it again; they are stored under the
local data directory and expire on their own. If every eligible model is unavailable,
the chat remains open rather than looping indefinitely. These retries apply to
chat; one-shot route/run commands retain their explicit constraints.
