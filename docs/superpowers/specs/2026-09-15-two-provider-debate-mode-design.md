# Two-provider debate mode

**Status:** user approved starting external isolation on 2026-09-16. Implementation
and offline verification are underway; real container validation remains pending.
See [implementation setup and limits](../../debate-isolation.md).
**Origin:** user request `user-001`, recorded in `AGENT_HANDOFF.md`.
**Revision 6:** user ruled out Docker; participants run with no tools and
Nectra supplies the evidence. Codex CLI is excluded as a participant.
**Revision 5:** Claude profile additionally survives a hostile project config;
managed policy remains the untested residual.
**Revision 4:** Claude restricted profile verified as tool restriction only;
a disposable clone corrected as insufficient isolation.
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

## Decision: no Docker, and no tools

**The user has ruled out Docker**, as a hard dependency or at all. That decision
also removes the problem it was solving.

Isolation was only ever needed because participants were going to be handed tools
to explore the repository. **A model with no write tool does not need a sandbox** --
there is nothing to deny. So debate participants run with no tools at all, and
Nectra, a trusted local program rather than a model, selects and supplies the
evidence in the prompt.

This is strictly better for debate quality, not merely cheaper. Two participants
that each explored the repository on their own are not disagreeing about the
answer; they are disagreeing about different inputs. Identical evidence is a
precondition for a meaningful debate, and the earlier runtime review already
required it for Ollama, which cannot inspect the repository at all. Making every
participant symmetric applies that rule consistently.

The read-only promise becomes trivially checkable: assert the launch arguments
expose no mutating tool. No container, no WSL, no Docker engine.

### The cost: Codex CLI cannot participate

| Runtime | Zero-tool mode | As a debate participant |
|---|---|---|
| Claude | Yes -- `--tools` names the exact available set, verified | Eligible |
| Ollama | Yes, by construction; the adapter executes no tools | Eligible |
| Gemini | Unverified; plan mode is policy, not absence of tools | Pending verification |
| Codex CLI | **No.** Its modes are read-only / workspace-write / danger-full-access, all of which describe *executing model-generated shell commands*. Shell is intrinsic, and its read-only sandbox permits deletion. | **Excluded** |

Debate needs two *different* providers, so Claude + Ollama works today, and
Claude + Gemini once verified. Codex CLI returns as a participant only if it gains
a genuine no-tool mode, or if the user later chooses external isolation after all.

### What this costs in the code

The coordinator is generic over a one-method `DebateBackend` trait, so the
draft/exchange/converge logic, selection, dissent handling, audit journal and
their tests all survive unchanged. Only the backend implementation changes.
`src/executor/isolated.rs`, `tools/debate-container/` and
`tests/isolated_runtime.rs` become unused; they are left in place pending Codex's
review rather than deleted while it is unavailable.

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

1. **Require a real boundary.** Run debate participants where the OS, not the
   CLI, denies mutation, and treat sandbox flags as defence in depth.

   **A disposable clone is not isolation** (Codex's correction). Copying the repo
   elsewhere changes nothing if the process can still reach the original
   checkout by absolute path. Isolation has to make the original *unreachable* --
   a container, a restricted account, or a genuinely read-only mount -- not merely
   provide somewhere else to work.
2. **Ship on a verified-only allowlist.** Debate runs only where a fixture passes
   on the user's actual platform. Current evidence:
   - **Codex on Windows: fails.** Deletion escapes `--sandbox read-only`.
   - **Claude restricted profile: tool surface verified, and it survives a
     hostile *project* config.** A live call exposed only `Read`/`Glob`/`Grep`
     with no MCP, left the tree unchanged, and a second run defeated planted
     permissive `Bash`/`Write`/`Edit` permissions, a `SessionStart` hook and a
     project MCP marker -- none executed, with the markers validated locally
     first so silence could not be a false pass.

     **Still not a whole-process guarantee.** Claude's own reference states
     safe-mode continues to apply *managed* policy, including policy hooks, and
     machine-managed settings were deliberately not altered or tested. A tool
     inventory cannot prove the absence of policy-driven side effects. The
     contract must therefore either verify managed configuration as part of the
     preflight, or isolate the whole process externally.
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
