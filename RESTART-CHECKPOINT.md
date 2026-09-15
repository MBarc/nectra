# Nectra checkpoint - September 12, 2026

Embedded provider terminal implementation is complete and packaged.

Default interactive launches now embed Claude, Codex, and Gemini in a real
PTY/Windows ConPTY pane. Native animations, colors, tool output and prompts are
rendered with keyboard forwarding, paste, resize and scrollback. Completed answers
automatically return to the shared routed prompt; a second Ctrl+] press confirms
cancellation when incomplete. Ollama retains HTTP streaming.

Clean answer capture uses per-launch provider hooks/notifications, separate from
terminal screen output. Existing Claude settings and Codex notifications are
preserved. Codex original notification argv is carried in launch arguments, not
loaded from a mutable hook file. Windows scoped jobs clean up owned descendants.

The blank pane was fixed by a documented vendored portable-pty ConPTY flags patch.
See vendor/portable-pty/NECTRA-PATCH.md and docs/terminal.md.

Validation: 75 tests passed, one ignored subprocess fixture; fmt and Clippy pass.
Real nested Windows ConPTY tests cover animation, approval input, resize, clean
answer handoff, automatic return without exit keystrokes, cancellation and descendant cleanup. Final hook
hardening was retested with all four terminal integration tests and Clippy.
No live provider generation or authentication changes were performed.

Release: target/release/nectra.exe and dist/nectra-windows-x64.zip (SHA256 sidecar).
The older extracted dist/nectra.exe was intentionally not overwritten because
it was running the user's session. Close it and update from the new ZIP to use
this implementation.

Native failure cannot prove that no tools ran, so it does not automatically replay
work on another provider. --provider-view stream retains safe pre-work automatic
failover. Native captured rate-limit/auth failures still update routing state.
Mouse forwarding and terminal graphics protocols are not implemented.

Project: C:\Users\micha\Desktop\projects\nectra
Cargo: C:\Users\micha\.cargo\bin\cargo.exe
Debug fixture: target/debug/nectra.exe __terminal-demo (no provider requests).
User prefers autonomous implementation without repeated approval questions.

Follow-up fix: leading chat directives `use MODEL`, `please use MODEL`,
`switch to MODEL`, and `ask MODEL` now choose a configured model for that turn.
They override existing pins for that turn only; unavailable models fail explicitly.
14 chat tests and all-target Clippy passed. See docs/terminal.md.

Latest behavior: native pane input defaults to a Nectra message queue. Enter submits;
F2 explicitly toggles provider keyboard control for approvals. Chat drains queued
messages in FIFO order through normal commands/routing with completed context.
Drafts in progress keep the pane open until submitted or erased. Queue is in memory.
Shared context and native bootstrap ask models not to acknowledge routing/handoffs.
78 tests and Clippy passed, including real PTY queue/approval tests.

Latest correction: Claude/Gemini visible startup prompt is the actual user message
(up to 2,000 characters), not an internal handoff instruction. Hooks match that
visible prompt and inject full context. Codex context-file mechanism unchanged.
79 tests passed across full suite plus new hook test, and Clippy passed.

Latest presentation: compact activity and main queue by default; F2 toggles full
native terminal and direct input. Completion clears owned rows before final answer
printing, eliminating duplicate visible answers and return banners. Stop hooks
are summarized as Finishing reply. Native progress extraction is heuristic with
elapsed fallback. Five PTY/hook tests, presentation regression and Clippy passed.

Latest completed feature: Haiku and GPT-6 Astra added. Original four-ID registries
receive missing new profiles in memory without rewriting user settings. Task
responsibility bonus now separates six model roles; physical building tasks classify
as planning. docs/routing.md documents exact rules and limitations.

Chat tracks unverified / last request succeeded / model rejected observations.
Explicit model requests fall back when unavailable; structured pre-work access,
auth or rate-limit errors relax turn pins and retry remaining eligible candidates.
Native rejection hooks retry only with observed begin and no tool work. Uncertain
native failures do not replay work. /model retry or /login resets observations.
84 tests passed, fmt/Clippy passed, release examples and ZIP checksum verified.
No live provider calls or account changes. Existing running extracted executable
was not overwritten; update from dist/nectra-windows-x64.zip after closing it.
