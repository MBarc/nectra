# Two-provider debate mode

**Status:** design, not approved. No code written.
**Origin:** user request `user-001`, recorded in `AGENT_HANDOFF.md`.

## What the user asked for

A Nectra mode where one person talks to two providers at once and the providers
coordinate, rather than the user reading two replies and picking.

Two decisions are already settled:

1. **Shape: debate, then one answer.** Both providers draft independently, each
   sees the other's draft, then Nectra returns one converged answer that
   **surfaces where they disagreed** instead of smoothing it away. Two models
   agreeing tells you little; the disagreement is the signal.
2. **Read-only, then hand off.** Providers may read the repo and run read-only
   commands so the debate is grounded in real code, but neither writes. The
   output is an answer or a plan; you execute it afterwards in normal
   single-provider mode. No file ownership, conflict detection, or rollback
   machinery is needed.

## Why this fits Nectra

The hard parts already exist. `router::route` returns a **ranked candidate list
with scores**, so choosing the two debaters is a selection over data Nectra
already computes, not a new mechanism. `Session::prompt` builds a bounded shared
transcript. `Backend::respond` is already the seam every provider goes through.

The risky part is not orchestration. It is proving the providers cannot write.

## Architecture

### Selecting the debaters

Take the top two candidates **from different providers**. Same-provider debate is
mostly an echo, and provider diversity is the point.

```
candidates: [gpt-5.6-sol 1.158, sonnet 0.909, gpt-5.6-luna 0.888, ...]
            ^ first                ^ first from a different provider
```

If only one provider is eligible — everything else disabled, cooling down, or
missing its CLI — **fall back to a normal single-provider turn** and say so on
one line. Debate mode must never be the reason a question goes unanswered.

### The three phases

| Phase | What happens | Calls |
|---|---|---|
| Draft | Both get the same prompt and shared transcript, independently and in parallel | 2 |
| Exchange | Each sees the other's draft and revises, or defends its position | 2 |
| Converge | The higher-scoring model writes the final answer from both revisions | 1 |

Five provider calls per turn. That cost is the whole reason this is a mode and
not the default, and the UI states it before the first call.

**Convergence is done by the higher-scoring model, not a third one.** A third
call would need a third provider and could not be grounded in the debate.

**Dissent is captured mechanically, not editorially.** The converging model is
asked to emit the answer and a separate dissent field; whatever the other model
said it still disagreed with is carried through **verbatim**. A model summarising
its opponent's objection will soften it — that is exactly the failure this mode
exists to prevent.

### Enforcing read-only — the real risk

This is the part that is not yet solved and must not be hand-waved.

Nectra has `--dangerously-skip-permissions` for the permissive direction. Debate
mode needs the opposite, and the enforcement differs per provider:

- **Codex CLI** has a sandbox with a read-only mode.
- **Claude Code** has permission modes and tool allowlists.
- **Gemini CLI** differs again.
- **Ollama** is chat-only; nothing to restrain.

**Open question, needs Codex:** whether each CLI can be launched in a mode that
*guarantees* no writes, or only discourages them. If a provider cannot be
constrained, the honest options are to exclude it from debate mode or to state
plainly that read-only is requested, not enforced. **Do not claim enforcement the
launch arguments do not deliver.** `src/executor/` is Codex's half and it has
already worked through the one-shot payload and sandbox arguments, so this
question belongs to it.

### Session and journal

A debate turn is **one** exchange in the journal, with the converged answer as
the assistant text and `model` recorded as both participants. Drafts are written
as separate journal events so the debate is auditable, but they never enter the
conversation context — otherwise every later turn pays for three extra copies of
the transcript.

Cancelling mid-debate saves nothing as a completed answer, matching how the
existing terminal view refuses to save partial work.

### Surface

`nectra debate "question"` for one-shot, and `/debate` in chat to toggle the mode
for subsequent turns. Both reuse the existing routing and session machinery.

Output shows both drafts collapsed, then the converged answer, then dissent under
a heading that cannot be missed. In the stream view this is text. In the terminal
view, two PTY panes are **out of scope for the first version** — debate runs in
the structured stream view and says so.

## What could go wrong

- **Confident agreement on a wrong answer.** Debate raises confidence without
  necessarily raising accuracy. The dissent field is the mitigation; if the
  models agree, the answer says they agreed rather than implying verification.
- **Cost surprise.** 5 calls where the user expected 1. Mitigated by stating the
  cost before the first call.
- **Latency.** Drafting is parallel; exchange and convergence are serial. Expect
  roughly three sequential provider round trips.
- **One provider failing mid-debate.** Degrade to the surviving provider's
  answer, labelled as undebated. Reuse the existing cooldown and failover paths
  rather than inventing a second error model.

## Explicitly out of scope

Three or more debaters. Debate during file edits. Two live terminal panes.
Auto-executing the agreed plan. Persisting debates across sessions beyond the
journal entries described above.

## Open questions for the user

1. **Cost.** Five calls per debate turn. Acceptable, or should the exchange phase
   be optional so a cheap debate is two calls plus convergence?
2. **Trigger.** Always-on toggle, or automatic when `ambiguous` is already true?
   Nectra already computes `ambiguous` and `score_margin`, so it can tell when a
   routing decision was close — that is arguably the exact situation a debate is
   worth paying for.
3. **Read-only.** If a provider cannot be *proven* read-only, exclude it from
   debate mode, or include it with a clearly stated caveat?
