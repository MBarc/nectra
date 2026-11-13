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
9/2/11 to 12/3/8: three more hard specs caught for one more false escalation.
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

## Routing-decision log

Every routing decision is recorded so a measured task × tier table (and escalation)
can be built from real use. Chat writes the records into its session log
(`~/.local/share/nectra/sessions/*.jsonl` on Linux); `nectra run --events jsonl`
puts the record in its `route` event (`decision`) and the outcomes in the `result`
event (`decision_outcomes`). Read them back with:

```
nectra log decisions --json [--since 7d|12h|30m|UNIX_SECONDS] [--session FILE_OR_DIR]
```

One JSON object per line, oldest first. It reads chat session logs only; `run`
decisions are in that command's own event stream (`source: "run"`). `tier` and `role`
are the classifier's own answers, before the router drops an unclaimed tier or swaps
in `demanding`. The "classified with the content of" file note is removed from
`reason` unless the opt-in is set. Session logs written by this version contain
`outcome` records and a `decision` field that older Nectra versions cannot resume. The prompt is stored only as a SHA-256 hash.
Its text is added only if you set `[routing] log_prompt_text = true`.

**Schema, `schema_version` 1.** Fields may be added without a bump; a removed or
re-meant field bumps it.

| Field | Meaning |
| --- | --- |
| `schema_version` | `1` |
| `id` | `d-<16 hex>`, unique; outcomes refer to it |
| `ts` | Unix seconds |
| `source` | `chat` or `run` |
| `prompt_hash` | Hex SHA-256 of the routed prompt |
| `prompt_text` | Present only with the opt-in |
| `task_kind` | Task kind, e.g. `debugging` |
| `classifier` | `laya`, `jev` or `keywords` |
| `tier` | `simple`, `normal`, `demanding`; null for keywords |
| `difficulty` | 0..1 after the stakes signal; null for keywords |
| `confidence` | The classifier's confidence in `task_kind`; null for keywords |
| `role` | The routing role the classifier chose, or null |
| `margin` | Score gap between the pick and the runner-up; null with one candidate |
| `mode` | `fast`, `balanced`, `best` |
| `candidates` | `[{model, provider, score}]`, best first |
| `pick`, `pick_provider`, `pick_score` | The chosen model and its score |
| `reason` | Why it was picked |
| `overridden` | The user pinned a model or provider |
| `ladder` | Only when the [task x tier table](#task-x-tier-table) chose: `{kind, tier, rung, len, hedged}`, `rung` 0 = cheapest |
| `escalation` | Only on a retry: how many steps up the ladder the task has taken (1 = first) |
| `outcome` | Latest result of the turn: `completed`, `tool_error` or `limit`; null while unfinished (`nectra log decisions` only) |
| `user_action` | Latest `user_again`, `user_switch_model` or `override` after it, or null (`nectra log decisions` only) |
| `follow_up` | `id` of the decision that followed this one, or null (`nectra log decisions` only) |
| `session` | The session file (`nectra log decisions` only) |

**Outcomes** are separate records (`{"type":"outcome","outcome":{decision, outcome,
follow_up}}` in a session log) so a decision is never rewritten; the latest wins.

| `outcome` | When |
| --- | --- |
| `completed` | The model answered |
| `failed_tests` | The caller's objective test failed; `nectra run --prior-outcome` (default) records it for the attempt `--escalate-from` names. Chat does not run tests |
| `tool_error` | The agent failed, was unavailable, or returned no answer |
| `limit` | A provider rate or usage limit stopped it |
| `deadline` | `nectra run --deadline` passed before the turn finished |
| `user_again` | The user sent `/again` (links to the retry) |
| `user_switch_model` | The user pinned a different model for the next message |
| `override` | The user pinned a provider for the next message |

`follow_up` links a failed decision to the fallback that replaced it, and a decision
to the `/again` retry or switch that followed it.

## Task x tier table

Laya supplies the task **kind** and a difficulty **tier** with its confidence; a
table in the model registry says which model takes that work. Each
`[[table]]` entry is one kind x tier and an ordered **ladder**, cheapest acceptable
model first, every rung carrying its evidence (trials, pass rate, confidence
interval, the calibration run it came from):

```toml
[[table]]
kind = "debugging"          # code_generation, debugging, refactoring, code_review,
tier = "normal"             # architecture, research, writing, planning, data_analysis, general
[[table.ladder]]            # simple | normal | demanding
model = "sonnet"
n = 30
pass_rate = 0.77
ci = [0.59, 0.88]
run = "rh-v2-0412"
[[table.ladder]]
model = "opus"
n = 30
pass_rate = 0.93
ci = [0.79, 0.98]
run = "rh-v2-0412"
```

**With no table, or no entry for the kind and tier, routing is exactly what it
was** (roles and scores, above). Every model named must be in the registry.
A pinned model (`--model`, `/model`, sticky follow-ups) skips the table.

- **Tier.** From Laya's difficulty against `[classifier] simple_below` and
  `demanding_above`, so those still steer the table. The prompt-keyword rules
  ("complex", "quick") decide the tier only on the keywords backend, which has no
  difficulty; with Laya or Jev the words are ignored here.
- **Modes.** `--mode fast` takes the cheapest rung and never hedges up; `best` takes
  the strongest available rung; `balanced` takes the cheapest, hedged as below.
  An unavailable model (cooldown, no login, denied) is skipped to the next rung.
- **Confidence hedge**, on by default. When the classifier's confidence in the task
  kind is under `hedge_confidence_below` (0.6), or Laya's difficulty is within
  `hedge_margin_below` (0.05, in difficulty units) of a tier boundary, `balanced`
  starts one rung higher (`tier_hedge = "rung"`), or takes the dearer of the picks
  for the tier and its nearer neighbour (`"top2"`); `"off"` trusts the tier. Keywords
  have neither signal and never hedge.

```toml
[routing]
tier_hedge = "rung"             # rung | top2 | off
hedge_confidence_below = 0.6
hedge_margin_below = 0.05
max_escalations = 2             # 0 turns escalation off
```

### Escalation

A task that fails steps **one rung up** from the model that failed. It never
steps past the top of the ladder, or past `max_escalations` steps, and then says so
rather than falling back to something unmeasured. The triggers:

| Trigger | Where |
| --- | --- |
| A tool error that is not a limit | Chat: the next message of the task, or `/again`, climbs. `run` does not replay work that started |
| `/again` | Chat: one rung above the model that answered (not "the strongest") |
| `failed_tests`: the caller's objective test failed | `nectra run --escalate-from MODEL [--escalation N] [--prior-decision ID] [--prior-outcome failed-tests\|tool-error]` |
| The user switches model | Chat: logged as the escalation (not a second step after a failure) |

Limits and the deadline are fallbacks, not escalations. Every escalation is logged:
the failed decision's outcome has `follow_up` pointing at the new decision, which
carries `escalation` and `ladder` (`nectra log decisions`, `run`'s `route` event
and `decision_outcomes`).

### Loading a table

`nectra registry table import FILE.json [--dry-run]` writes the table from the
calibration output into the registry file between two marker comments (a later
import replaces just that block; the models are untouched) and refuses a table that
fails validation. `nectra registry table show [--json]` prints what the registry
holds. The input format is [docs/examples/tier-table.example.json](examples/tier-table.example.json):
a placeholder, not measured, and not part of the default configuration.
A rung that names a model no longer in `models.toml` is dropped when the registry loads, with one warning on stderr; a cell left with no rung routes the old way. `registry table import` still works in that state, so it can repair the table.


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
