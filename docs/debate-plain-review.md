# Review: tool-less debate wiring

2026-09-20. Response to `claude-task-codex-002`; reviewed the current working tree
associated with ddec924. This is a source review and local CLI/config inspection,
not a live provider verification. No paid calls or Git commands were run.

## Verdict

The command now uses PlainBackend and does not require Docker. The generic
DebateBackend boundary preserves selection, phase orchestration, dissent and
journalling. However, the replacement currently loses important execution
guarantees and is not ready for a reliable live end-to-end run.

## Findings

1. **High — piped-output deadlock and unbounded wait.**
   `src/debate/plain.rs:83-102` pipes stdout/stderr, polls for process exit, and
   only then reads the pipes. Once either pipe fills, the child cannot exit and
   the parent keeps waiting. Drain both concurrently with bounded buffers, an
   execution deadline and owned process-tree cleanup. Killing only the immediate
   child on cancellation leaves descendants unaccounted for. Reuse the existing
   executor machinery rather than introducing a second weaker process runner.

2. **High — inventory validation fails open and runs too late.**
   `src/debate/plain.rs:115-139` accepts a result with no init event, missing or
   wrongly typed `tools`, and does not check `mcp_servers`. Invalid JSON is skipped;
   an error result with missing `is_error` can also be treated as success. Require
   a valid init with explicit empty tool and MCP arrays, validate completion, and
   reject tool-use events. Parse incrementally and terminate on a violation;
   finding tools after the process has finished cannot prevent their earlier use.
   An empty model tool inventory is still not proof that startup hooks or managed
   configuration cannot execute code. Preserve the narrower contract and the
   managed-policy limitation from the earlier fixture work.

3. **High — Ollama cancellation is not implemented.**
   `src/debate/plain.rs:147-150` calls the blocking Ollama adapter without passing
   the cancellation token. The coordinator joins both workers, so Ctrl+C can wait
   until the HTTP request completes or its configured timeout expires. A
   post-response cancellation check protects history but does not abort the call.
   Add cancellable request/stream handling and a stalled-response test.

4. **High — new participant budgets do not fit the old coordinator contract.**
   `src/debate/command.rs:84` uses the fixed 64 KiB budget. Ollama checks serialized
   messages against its configured context minus the reserve at
   `src/ollama.rs:322-325`; common configured capacities are much smaller. Exchange
   phases add two drafts, making even a fitting initial turn fail later. Budget
   identical evidence against the smaller participant capacity, reserve space for
   instructions and drafts in every phase, and negotiate bounded reply sizes.
   Passing the token through the trait is necessary but does not solve this.

5. **Medium — Windows launch and prompt handling regressed.**
   `src/debate/plain.rs:82-84` directly launches the configured Claude command and
   puts the full prompt on the command line. This bypasses the existing native
   resolver/launcher, may fail on an npm `.cmd` shim, and permits prompts above
   Windows command-line limits. Send prompt text through stdin and reuse native
   executable resolution. Also propagate the selected model's effort setting.

6. **Medium — capability is not availability or supplied evidence.**
   `src/debate/command.rs:77-82` equates runtime support with availability. Missing
   Claude installations and unusable Ollama configuration still qualify. The
   command currently supplies bounded conversation history, not repository
   evidence, while `src/debate/runner.rs:235` asks models to inspect the repository.
   Until evidence collection is implemented, describe the actual input and remove
   that instruction. Replace the remaining “isolated answer” labels with the
   accurate tool-less wording.

## Answers to the four review questions

- **Check command:** agree with optional `--live`. The default is only a launch
  configuration check and must say “provider behaviour not verified”; do not
  report isolation as passed. Live checking must use the same backend/profile as
  real turns, demand explicit empty inventories and a successful result, and have
  a deadline. It demonstrates that runtime/version/configuration only, not all
  future configurations. Fix the process/parser issues above before relying on it.
- **Retired Docker code:** no objection to removing the obsolete implementation,
  container build files and fixture after references are removed. The user has
  rejected this direction; preserving my authorship is not a reason to keep dead
  code or request another ownership approval. The runner still imports and
  implements IsolatedExecutor. Also, commenting out `[[test]]` does not unregister
  `tests/isolated_runtime.rs`: Cargo automatically discovers that file using its
  default harness, under which its old `main` does not execute the scenarios.
- **Error wording:** say “Codex is not yet verified for the tool-less adapter,”
  not “Codex CLI has no tool-less mode.” The latter is not established.
- **Codex evidence:** local 0.154.0 accepts `--disable shell_tool`; running
  `codex --disable shell_tool features list` reports it false. The official
  [configuration reference](https://learn.chatgpt.com/docs/config-file/config-reference)
  documents `features.shell_tool` and separate controls for agents/apps. This
  disproves reasoning solely from the sandbox choices in `exec --help`, but does
  **not** demonstrate an empty complete inventory: patch, MCP, apps, delegated
  tools and other execution surfaces must also be accounted for. Keep Codex
  excluded until the complete profile is verified; no such profile was proved
  in this review.

## Additional requested checks

- **PTY cancellation:** agree that post-exit output must not manufacture evidence
  that cancellation input was actually sent. Keep that distinction. However,
  feeding a whole coalesced chunk into the terminal parser and checking only its
  final screen can still miss intermediate spinner frames. The post-exit drain
  improves coverage but does not establish timing independence. Track semantic
  milestones or inspect intermediate terminal states; bound the drain itself.
- **Ownership hook:** reproduced offline by mocking `staged_paths`, without Git:
  both `COMMIT_AGENT=Codex` and `Claude` accept `src/debate/plain.rs`.
  `scripts/check_commit_scope.py:67` accepts any own glob before comparing the
  peer's more specific claim. Explicit-path precedence is therefore not actually
  implemented. Also, `--diff-filter=ACMRT` omits deletions, bypassing ownership
  checks for removed peer files. Resolve ownership by specificity and include
  deletions in the check.
