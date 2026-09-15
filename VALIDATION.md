# Native terminal validation — September 12, 2026

75 tests passed with one ignored subprocess fixture; formatting and Clippy passed.
Real nested Windows ConPTY tests cover native animation, Unicode, approval input,
resizing, completed answer capture, automatic return without exit keystrokes, cancellation, and termination of
provider descendants. The local interactive demo also passed manually.

The initial blank-pane bug was reproduced and traced to ConPTY startup behavior.
The vendored portable-pty change creates an independent terminal with standard
flags rather than inheriting the parent cursor or enabling undocumented input modes.

Provider-specific handoff formats and settings preservation use local fixtures.
No live Claude, Codex, Gemini or Ollama model request or authentication change was
performed. Native UI compatibility depends on the installed provider CLI and its
hook/notification policies. See docs/terminal.md for native-mode failover limits.

# Local validation — September 10, 2026

Environment: Windows x64, Rust 1.98.1, GNU toolchain and the existing WinLibs linker.

| Check | Result |
| --- | --- |
| `cargo test --all-targets` | 36 passed; subprocess fixture is marked ignored and invoked by two process tests |
| `cargo fmt --all --check` | Passed |
| `cargo clippy --all-targets --locked -- -D warnings` | Passed |
| Offline classification corpus | 25/25 fixtures passed; not a model-quality benchmark |
| `cargo build --release --locked` | Passed; `target/release/nectra.exe` |
| Release executable version and startup | v0.3.0; ASCII banner, `/models`, and `/quit` checked in a temporary session |
| Windows ZIP | Rebuilt; SHA-256 sidecar verified |
| Native CLI discovery/version checks | Claude Code 2.1.261; Codex CLI 0.153.4 |
| PowerShell installer parsing | Passed |
| Unix installer `bash -n` | Passed |
| Workflow YAML parsing | Passed |

The Codex version check emitted sandbox-related temporary-directory warnings but
returned success. No live model request, authentication check, public release,
remote installation, or Linux/macOS CI execution was performed. Those remain
external validation steps. CI and release workflows are prepared in `.github/workflows`.

The local Windows package uses the GNU toolchain; hosted Windows releases are
configured to use MSVC. `scripts/build.ps1` reruns checks, builds and packages the
local executable; `-SkipChecks` only skips checks for an already validated tree.

v0.3 tests cover cross-provider transcript handoff, per-message routing, runtime
availability filtering, failures, multiline input, context reset, dry-run mode,
session creation/resume, exclusive locks, interrupted records, corrupt records,
working-directory validation and bounded Unicode context. Streaming adapters are
tested against Claude/Codex event fixtures. Cross-provider replies in tests use
mock backends; live account/model response compatibility is not yet verified.

Uninstaller checks (September 11): Windows PowerShell and Unix shell fixture
tests passed, including preview mode, removal of the executable and helper,
portable-directory detection, preservation of unrelated files and saved data,
and repeated execution. Windows checks also verified that unrelated user PATH
entries were unchanged. The user's real Nectra installation was not uninstalled.
Installer and release packaging now include the matching uninstall helper.
# Rate-limit failover validation

September 11, 2026: 40 tests passed, formatting and Clippy passed, and the Windows
release executable and ZIP were rebuilt.

Synthetic CLI event and mocked chat tests cover rejection before work, refusal to
replay after activity, provider failover with identical context, cooldown scope and
expiry, and exhaustion without an infinite retry loop. These checks do not consume
live model quota or simulate an actual provider account limit. Cooldowns are in memory.
# Provider setup validation — September 11, 2026

46 tests passed, including additive provider management, selective CLI checks, missing-dependency failure without
overwriting configuration, wizard input/cancellation, routing exclusion, and repeat
installation preserving configuration. Dependency failures use injected fixtures;
no provider software was installed and no authentication or paid model calls ran.
The real installed Claude Code and Codex executables also passed the setup version
checks using a temporary configuration, without changing the user's configuration.
# Ollama, Gemini, terminal theme, and reauthentication — September 11, 2026

58 tests passed. Added local HTTP mock coverage for Ollama discovery, locality
checks before sending context, shared messages, streamed Unicode output, partial
failure, busy-server fallback, and local-only enforcement. Synthetic Gemini events
cover final answers and rate limits. Provider argument tests cover launch-scoped
permission mappings without executing permission-bypassed agents. Mock login tests
cover restoring a blocked provider without replaying prior work. CLI tests verify
color controls, plain JSON, and noninteractive login rejection.

No real Ollama generation, Gemini generation, or interactive login was performed.
No provider software was installed and no account authentication was changed.
Human CLI checks validate dependencies, not account/model entitlement.
The installed Gemini CLI 0.26.0 successfully answered a direct version check.
Its cold Node startup exceeded the original 10-second deadline, so Node-based
dependency checks now allow up to 60 seconds; native executable checks retain 10.
The revised setup check passed against the installed Gemini CLI with a temporary
configuration. Final formatting and Clippy checks passed; the Windows release
executable and ZIP were rebuilt.

## Activity feedback and selectable themes - September 11, 2026

64 tests passed, with one ignored subprocess fixture; formatting and Clippy passed.
Coverage includes provider progress classification, elapsed-time animation state,
partial-line preservation, activity cleanup, theme persistence, launch overrides,
and keeping UI commands out of conversation history. The activity lifecycle test
also passed in a PTY. No live model requests or authentication changes were made.

## Natural-language model selection - September 12, 2026

14 chat tests and all-target Clippy passed after adding leading model directives.
Coverage includes the exact `use opus and plan out how you would build a shed`
request, per-turn overrides of persistent pins, history not inheriting the override,
ordinary/negated mentions, and unavailable requested models failing explicitly.
No live model calls were made for this routing change.

## Main-thread queue and quiet context handoff - September 12, 2026

78 tests passed (one ignored subprocess fixture), plus formatting and Clippy.
Native PTY fixtures now queue two messages before using F2 to answer the provider's
approval, then verify both messages are returned to Nectra. Chat integration checks
FIFO processing, per-message model selection and growing completed context.
The shared prompt and native bootstrap now request direct answers without routing
or context-loading acknowledgments. This wording was checked locally; model adherence
has not been verified with live generation.

## Visible prompt correction - September 12, 2026

Claude/Gemini now launch with the actual user message, with context supplied through
hooks instead of a visible bootstrap instruction. Existing 78 tests passed; the
new hook integration test also passed (79 total), along with Clippy. No live calls.
Long displayed prompts are bounded to 2,000 characters; full context is unchanged.

## Compact presentation - September 12, 2026

Default native presentation now shows a compact temporary activity/queue area.
F2 expands the actual provider UI for inspection and approvals. Owned terminal rows
are cleared on completion; only the captured final answer remains. Internal stop
hooks are presented as finishing the reply. Five native integration tests and the
new compact presentation regression passed, plus formatting and all-target Clippy.
Progress recognition is heuristic; unfamiliar provider output falls back to an
elapsed activity indicator and remains inspectable with F2. No live model calls.

## Responsibility routing, Haiku/Astra and model access - September 12, 2026

84 tests passed across the full 83-test suite and added native rejection boundary
test; formatting and all-target Clippy passed. Release routing smoke tests select
Haiku, Luna, Sonnet, Opus, Sol and Astra for the six documented examples. Release
ZIP rebuilt and SHA256 verified. No live provider requests were made.

Coverage includes responsibility selection, physical shed planning classification,
model-denial detection versus filesystem permissions, explicit-model fallback,
no replay after work, future exclusion and native prompt/tool boundary tracking.
Entitlements remain unverified until real use; provider responses validate access
reactively rather than through hidden test requests. See docs/routing.md.
