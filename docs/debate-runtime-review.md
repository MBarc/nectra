# Debate runtime review

Reviewed 2026-09-15. This is a design review, not a verified sandbox implementation.

## Read-only conclusion

The current executor does **not** enforce read-only operation. `arguments()` in
`src/executor/mod.rs` accepts configured extra arguments and translates
`skip_permissions` into provider bypass flags. The stream adapter inherits these
arguments. Do not reuse that path unchanged for debate.

Define the promise as **no provider-initiated repository mutations**. Provider
authentication, logs, caches and Nectra journal writes mean “no writes anywhere”
is a different and much stronger requirement.

| Runtime | Available restriction | Assessment |
|---|---|---|
| Codex | `--sandbox read-only` with `--ask-for-approval never` (global option before `exec`, or explicit approval configuration) | Documented sandbox enforcement for model command execution; not proof that every configured integration is contained. Requires controlled configuration and platform verification. |
| Claude | `--tools Read,Glob,Grep`, with no shell or mutation tools; disable MCP, hooks and other execution-capable customizations | A restricted tool surface can support repository inspection. Plan mode or `--allowedTools` alone is not an OS write boundary. Native Windows has no built-in Bash sandbox. |
| Gemini | `--approval-mode=plan` | Policy-restricted planning, not a blanket no-write guarantee. Plan files can be written, their location is configurable, and policy/MCP customization matters. Exclude from enforced v1 until a controlled adapter is verified. |
| Ollama | Current Nectra adapter sends chat messages without executing model tools | No model-driven local filesystem tool execution through this adapter. It cannot independently inspect the repository; supply the same bounded evidence to both participants. |

Sources: [OpenAI approvals and sandbox guidance](https://learn.chatgpt.com/docs/agent-approvals-security),
[Claude CLI reference](https://code.claude.com/docs/en/cli-reference),
[Claude sandbox platform support](https://code.claude.com/docs/en/sandboxing),
[Gemini Plan Mode](https://geminicli.com/docs/cli/plan-mode/).

Local help inspection: Codex 0.154.0 advertises read-only sandboxing; Claude
2.1.273 advertises `--tools`, `--restricted`, `--strict-mcp-config` and settings
controls. These checks did not invoke models or prove denial behavior.
Claude's `--tools` controls built-ins, not MCP; `--allowedTools` controls approval,
not tool availability. A candidate Claude profile is restricted mode plus only
Read/Glob/Grep, no MCP, and disabled customizations, with managed settings checked.
Do not describe this as a whole-process OS sandbox.

Recommended boundary: an explicit debate execution policy, independent of normal
permission flags. Reject bypass options and uncontrolled extra arguments, disable
external tool/customization paths, fail closed on unsupported versions/platforms,
and test attempts at direct writes, shell writes, hooks and MCP side effects in a
disposable fixture. If arbitrary shell commands must remain available across all
providers, use an externally enforced read-only repository mount and controlled
scratch area; CLI planning flags alone are insufficient.

## Required spec corrections

1. **Fallback preserves restrictions.** With one eligible provider, run a labelled
   single-provider read-only answer. Never enter the normal writable backend.
   Cancellation aborts the whole turn; it must not trigger survivor fallback.
2. **Dissent comes from its author.** Parse and retain each revision's dissent
   separately. Nectra appends the other participant's original field; the converger
   cannot faithfully manufacture that field. Define malformed/missing-field handling.
   The second author cannot dissent about a final answer it has never seen: label
   this as unresolved concerns from the exchange, not a final-answer endorsement.
3. **Parallelism requires new plumbing.** `Backend::respond` takes `&mut self` and
   a mutable writer. It is not presently a concurrent interface. Use independent
   call contexts/output buffers and one turn-level cancellation token. Do not call
   `conversation()` twice concurrently: its global interrupt registration holds
   one active token. Use `conversation_with_cancellation` beneath one coordinator.
4. **Eligibility is more than ranking.** Router candidates have model/provider/score,
   not runtime/effort/context limit. Resolve those from the validated registry.
   Apply availability, cooldown, read-only capability and local-only constraints
   before selection. Explicit pins and local-preference filters can leave only one
   provider; do not silently discard user constraints to create diversity.
5. **Budget each phase.** Shared history plus the question, peer draft, revision
   and instructions must fit each recipient's context limit. Set bounded outputs,
   retry limits and a turn deadline. Five logical calls is not five API requests
   when tools or retries are involved, nor a fixed cost.
6. **Journal compatibility is real work.** `session.rs` uses a closed, tagged event
   enum with `deny_unknown_fields`. Add typed debate events/participants and define
   resume/version behavior; do not smuggle two model IDs into a single model field.
   Partial phase records may remain auditable without becoming completed history.
7. **Correct latency wording.** Three serial round trips requires both draft calls
   parallel and both exchange calls parallel. Serial exchanges produce four rounds.
8. **Avoid unsupported product claims.** Provider diversity is a choice, not proof
   of independence or correctness. A third judge need not be a third provider.
   Routing score is a heuristic, not demonstrated judging ability. Keep the mode
   explicitly selected; a close routing score alone is not justification to spend
   five calls automatically. Plain text cannot provide collapsed draft panels.

Suggested implementation split: Claude owns selection, orchestration, journal and
CLI surfaces; Codex owns the restricted executor and shared cancellation boundary.
First agree on the enforceable runtime capability contract above, then test it
before advertising read-only execution. No application code changed in this review.
