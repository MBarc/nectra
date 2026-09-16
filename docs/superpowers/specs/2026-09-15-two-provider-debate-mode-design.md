# Two-provider debate mode

**Status:** design, not approved. No code written.
**Origin:** user request `user-001`, recorded in `AGENT_HANDOFF.md`.
**Revision 3:** records the verified read-only failure on Windows Codex.
**Revision 2:** incorporates Codex's runtime review (`docs/debate-runtime-review.md`),
which corrected eight points in revision 1. Corrections are marked below.

## What the user asked for

A Nectra mode where one person talks to two providers at once and the providers
coordinate, rather than the user reading two replies and picking.

Two decisions are settled:

1. **Shape: debate, then one answer.** Both providers draft independently, each
   sees the other's draft, then Nectra returns one converged answer that
   **surfaces where they disagreed** instead of smoothing it away.
2. **Read-only, then hand off.** Providers inspect the repository but do not
   change it. You execute the result afterwards in normal single-provider mode.

## The blocker: read-only is not enforced today

Revision 1 treated this as a detail. It is the gating constraint.

`arguments()` in `src/executor/mod.rs` accepts configured extra arguments and
translates `skip_permissions` into provider bypass flags, and the stream adapter
inherits them. **That path cannot be reused for debate.**

**Scope the promise precisely: no provider-initiated repository mutations.** Not
"no writes anywhere" — provider auth, logs, caches and Nectra's own journal all
write legitimately. Claiming the stronger version would be false.

| Runtime | Available restriction | Verdict |
|---|---|---|
| Codex (Windows) | `--sandbox read-only` | **VERIFIED FAILING.** Denies create, overwrite, rename and mkdir -- but **allows deletion**. Reproduced independently by both agents on 2026-09-15 with Codex 0.154.0. See `docs/read-only-verification.md`. |
| Claude | `--restricted` plus `--tools Read,Glob,Grep`, no MCP | Untested. A restricted tool surface, **not** an OS write boundary. `--tools` governs built-ins, not MCP; `--allowedTools` governs approval, not availability. |
| Gemini | `--approval-mode=plan` | Policy-restricted planning only; plan files are writable to a configurable location. **Excluded from v1.** |
| Ollama | Adapter sends chat messages; no tool execution | No model-driven filesystem access. Cannot inspect the repo, so it must be handed the same bounded evidence as its opponent. |

### The verified result changes the design

`--sandbox read-only` permitting deletion is not a partial pass. Deletion is the
most destructive mutation available, so on this platform a debating model could
remove repository files while Nectra advertised read-only. **The v1 promise
cannot be "no repository mutations" on Windows Codex.**

Three honest options, for the user to choose:

1. **Require a real boundary.** Run debate participants against an externally
   enforced read-only mount or a disposable clone, and treat CLI sandbox flags as
   defence in depth rather than the guarantee. Strongest, most work.
2. **Ship on a verified-only allowlist.** Debate runs only where the fixture
   passes. Today that is nothing on Windows, so the mode would not ship here
   until Claude's profile is tested or upstream fixes deletion.
3. **Ship with the promise weakened** to "restricted, not guaranteed", stating
   plainly that deletion is possible. Cheapest and, given deletion specifically,
   the hardest to defend to a user who loses files.

Recommendation: **(1) for the real fix, (2) as the interim**, and not (3) while
the escaping operation is deletion. Whichever is chosen, the wording must match
what the fixture actually proves on the user's platform, checked at runtime and
failing closed.

**Design rule:** debate uses an explicit execution policy independent of the
normal permission flags. It rejects bypass options and uncontrolled extra
arguments, disables external tool and customization paths, and **fails closed**
on unsupported versions or platforms. If arbitrary shell commands must stay
available, CLI planning flags are insufficient — that requires an externally
enforced read-only mount plus a controlled scratch area.

Denial behaviour must be **tested in a disposable fixture** — direct writes,
shell writes, hooks and MCP side effects — before the mode advertises read-only.
Until that passes, the honest label is "requested, not enforced."

## Architecture

### Selecting the debaters

Router candidates carry model, provider and score — **not** runtime, effort or
context limit. Resolve those from the validated registry, then filter on
availability, cooldown, read-only capability and local-only policy **before**
selecting.

Prefer two different providers, but **never discard an explicit user pin or a
local-only constraint to manufacture diversity**. If the user's constraints leave
one provider, that is the answer, not a problem to route around.

### The three phases

| Phase | What happens | Calls |
|---|---|---|
| Draft | Both get the same prompt and shared transcript, independently | 2 |
| Exchange | Each sees the other's draft and revises, emitting its own dissent | 2 |
| Converge | The higher-scoring model writes the final answer from both revisions | 1 |

Five *logical* calls. **Not five API requests** — tools and retries mean the real
cost is variable, and the UI must say so rather than quoting a fixed number.

**Latency:** three serial round trips only if both drafts run in parallel *and*
both exchanges do. Serial exchanges make it four.

### Dissent belongs to its author

Revision 1 had the converging model emit the other side's dissent. It cannot do
that faithfully — asking a model to state its opponent's surviving objection is
exactly how the objection gets softened.

Each participant emits its **own** dissent field during the exchange phase.
Nectra carries both through mechanically. Malformed or missing fields are
reported as such, never silently dropped.

One honesty constraint: the second author never sees the final answer, so its
dissent is **"unresolved concerns from the exchange"** — not an endorsement of,
or objection to, the converged text.

### Parallelism needs new plumbing

`Backend::respond` takes `&mut self` and a mutable writer; it is not a concurrent
interface today. `conversation()` registers a global interrupt with a single
active token, so **it must not be called twice concurrently.** Debate needs
independent call contexts and output buffers beneath one coordinator, using
`conversation_with_cancellation` with one turn-level cancellation token.

### Session and journal

`session.rs` uses a closed, tagged event enum with `deny_unknown_fields`. Adding
debate means **typed debate events with an explicit participants list**, plus
defined resume and version behaviour. Do not smuggle two model ids into the
single `model` field.

Partial phase records may stay auditable without becoming completed history.
**Cancellation aborts the whole turn** and saves nothing as a completed answer —
it must not fall through to a survivor answer.

### Failure handling

If only one provider is eligible, run a **labelled single-provider read-only**
answer. It must never fall back into the normal writable backend — that would
turn a degraded debate into an unrestricted agent, which is the one outcome this
mode exists to prevent.

### Surface

`nectra debate "question"` one-shot, `/debate` to toggle in chat. Stream view
only for v1; two live PTY panes are out of scope.

The mode is **always explicitly selected.** A close `score_margin` is not
justification to spend a multi-call debate automatically. Plain text also cannot
provide collapsed draft panels — drafts are shown plainly or behind a command.

## What could go wrong

- **Confident agreement on a wrong answer.** Provider diversity is a design
  choice, not proof of independence or correctness, and routing score is a
  heuristic, not demonstrated judging ability. The output states that the models
  agreed — it never implies the answer was verified.
- **Cost surprise.** Stated before the first call, as a range.
- **Read-only claimed but not delivered.** Mitigated only by the fixture tests
  above. Until they pass, the wording stays "requested, not enforced."

## Out of scope

Three or more debaters. Debate during file edits. Two live terminal panes.
Auto-executing the agreed plan. Auto-triggering on routing ambiguity.

## Implementation split

- **Claude:** selection, orchestration, journal events, CLI surface.
- **Codex:** the restricted executor and the shared cancellation boundary.

Agree the enforceable runtime capability contract first, and test it, before any
user-facing claim of read-only execution.

## Open questions for the user

1. **Cost.** Five logical calls per turn, real cost variable. Acceptable, or make
   the exchange phase optional for a cheaper two-call-plus-convergence debate?
2. **Gemini.** Excluded from v1 because plan mode is not a write boundary.
   Acceptable, or worth including with an explicit "not enforced" label?
3. **Read-only is now proven absent** for Windows Codex: the sandbox allows
   deletion. Which of the three options above do you want -- an externally
   enforced boundary, a verified-only allowlist that currently ships nothing on
   Windows, or a weakened promise that admits files can be deleted?
