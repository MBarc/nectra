# Task responsibilities and model access

Nectra's bundled roster now includes haiku and gpt-6-astra. Existing registries
containing all four original bundled IDs receive these two missing profiles in
memory; existing entries, including disabled ones, are preserved. Custom registries
without that original roster remain unchanged. Provider selections still apply.

| Task | Preferred model |
| --- | --- |
| Quick summaries, extraction, short answers, simple edits | haiku |
| Simple coding or technical tasks | gpt-5.6-luna |
| Everyday conversation, writing, research synthesis | sonnet |
| Planning and architecture | opus |
| Implementation, debugging, refactoring, review, data analysis | gpt-5.6-sol |
| Explicitly demanding or complex tasks | gpt-6-astra |

These are product routing preferences, not measured capability claims. Preferences
add 0.30 to the existing quality/speed/cost score; other eligible models remain
fallbacks. Fast mode prefers Haiku for nontechnical tasks and Luna for technical
ones. Explicit demanding-task signals take precedence over that speed preference.

Demanding signals: complex, difficult, hardest, multi-step, multistep, end-to-end,
escalate, deep reasoning. Light-task signals: quick, simple, brief, short,
summarize, extract. Classification remains local keyword matching, not a semantic
AI judge. Physical building prompts such as "plan how to build a shed" classify
as planning rather than coding. Responsibilities are defined in src/policy.rs;
capability estimates remain editable in the registry.

Use `/models` to see configured IDs and session observations. `use gpt-6-astra and
solve this complex problem` selects Astra for one turn. `/model haiku` pins a
preference; `/model auto` restores automatic routing.

## Access and fallback

Installing a provider CLI does not prove account entitlement to a model. Models
start with unverified access. Actual requests validate provider acceptance; a
completed reply records that the last request succeeded. This does not promise
future access or attest which underlying model a provider alias resolves to.

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
in via `/login PROVIDER` also clears that provider's model observations. Restarting
chat resets observations and cooldowns. If every eligible model is unavailable,
the chat remains open rather than looping indefinitely. These retries apply to
chat; one-shot route/run commands retain their explicit constraints.
