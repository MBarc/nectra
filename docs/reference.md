# Nectra reference

The full manual. The [README](../README.md) is the short tour.

## See it decide

```sh
nectra route "debug this intermittent race condition in the scheduler"
```

```json
{
  "task": "debugging",
  "provider": "openai",
  "runtime": "codex",
  "model": "gpt-5.6-sol",
  "effort": "high",
  "mode": "balanced",
  "score": 1.1582785,
  "score_margin": 0.24934578,
  "ambiguous": false,
  "reason": "local heuristic classified prompt as debugging; highest eligible score including task responsibility and complexity",
  "candidates": [
    { "model": "gpt-5.6-sol",  "provider": "openai",    "score": 1.1582785 },
    { "model": "sonnet",       "provider": "anthropic", "score": 0.9089327 },
    { "model": "gpt-5.6-luna", "provider": "openai",    "score": 0.8883139 }
  ]
}
```

*Candidate list abbreviated; the real output ranks every eligible model.*

`score_margin` tells you how close the call was, and `ambiguous` turns true when
it was close enough that you should probably choose yourself. `candidates` shows
everything it weighed. Disagree? `--model` overrides it and the conversation
continues uninterrupted.

## Why you would want this

**Stop paying the context tax.** Switching models normally means re-explaining
everything. Here the conversation is Nectra's, not the provider's, so `/model
gpt-5.6-sol` mid-thread carries your completed history across, trimmed to a
budget with an omission notice when it outgrows the target model's context.

**Routing you can argue with.** Scores come from an editable capability table in
`config/models.toml`, not a black box. `nectra route` shows the decision,
`nectra eval` scores the classifier against a corpus, and models declare which
work they want with `prefers = ["coding"]`. Bring your own corpus and tune it.

**The real CLI, not a reimplementation.** Claude, Codex and Gemini run in a true
PTY (ConPTY on Windows). You get their actual interface, animations and approval
prompts, not a lossy wrapper around them.

**It keeps going when a provider does not.** In the structured stream view
(`--provider-view stream`), recognised rate-limit and authentication rejections
arriving *before* any work has streamed fail over to another eligible model and
carry the conversation with them. The embedded terminal view cannot prove no
tools ran, so it deliberately does **not** auto-replay; it reports and lets you
choose. Cooldowns are remembered across runs until they expire, so a live limit
is not rediscovered by walking into it again.

**Local models are first class.** Point it at Ollama and local models compete on
the same table as hosted ones. `local-only` keeps routing to your configured
Ollama server rather than any hosted provider - that server is wherever
`base_url` points, so it is only on-machine if you configured it that way.

**One binary, no daemon.** Nectra itself is a single Rust executable with no
runtime of its own and no account, because there is no us - your provider CLIs,
your keys. Those CLIs keep their own requirements; Gemini CLI needs Node.

## Install

Build it with current stable Rust:

```sh
cargo build --release --locked
./target/release/nectra setup
./target/release/nectra chat
```

`nectra setup` asks which providers you have access to and checks that their CLIs
are actually installed. `nectra doctor` re-checks at any time without making a
single model call. Full build, packaging and uninstall instructions are below.

Prebuilt archives are produced by tag-based CI for Windows x64, Linux x64 and
macOS on Intel and Apple Silicon, each with a SHA-256 sidecar, published as
releases of [MBarc/nectra](https://github.com/MBarc/nectra/releases). The
install scripts download from there by default.

## One shared chat session

Start `nectra chat`, or just `nectra` in an interactive terminal. Keep typing in
one conversation; Nectra routes each new message among enabled profiles whose
CLI is installed or whose Ollama connection is configured. The chosen model is displayed above its reply.
Each chat opens with a Nectra ASCII-art banner and the installed version.
In an interactive terminal, Claude and Codex run behind a compact activity
view that clears before displaying the final answer once. F2 reveals the full native
terminal, including tool output, errors and approval prompts. Typing queues messages in the main thread;
press F2 to interact with provider approvals and F2 again to return to queuing.
**When the reply is captured, Nectra automatically
returns to its main prompt to route your next message.** Shift+PageUp/PageDown scrolls the
pane; `/quit` or `/exit` can also close the provider UI. Returning during unfinished
work requires a second Ctrl+] and does not save a partial answer as completed.

Use `nectra --provider-view stream` for the previous structured streaming view,
which retains Nectra's elapsed-time activity indicator and automatic pre-work
failover. Redirected input/output selects this view automatically. Explicit
`--provider-view terminal` requires an interactive terminal. Ollama uses its HTTP
stream and Nectra activity indicator because it has no child terminal UI. Gemini
CLI uses stream view too; see [the terminal guide](terminal.md#choosing-a-view).
See [the terminal guide](terminal.md) for controls and compatibility details.
Automatic routing may choose the same model for several turns when its policy
score remains highest. You can switch models without losing the conversation.

```powershell
.\target\release\nectra.exe chat
```

Inside the session:

```text
you> Draft a plan for our project.
...
you> /model gpt-5.6-sol
you> Implement the first part of that plan.
...
you> /model auto
you> Review what we have so far.
```

- `/models` lists configured models and executable availability.
- `/model ID` pins subsequent turns to a model; `/model auto` restores automatic
  routing across selected providers under your local routing policy.
- `/again` sends your last message again, routed in best mode to any model but
  the one that answered, ignoring a `/model` pin; `/again ID` sends it to that
  model. Nothing is replaced unless the retry can be routed. The replaced answer
  stays in the session log but leaves the conversation context; files it changed
  stay changed. After a failed turn, `/again` resends the message as it was.
- `/provider anthropic`, `/provider openai`, `/provider google`, `/provider ollama`, or `/provider auto` restricts routing
  and clears a model pin. `/mode fast|balanced|best` changes policy.
- `/paste` begins multiline input. Finish with `/send` or discard with `/cancel`.
- `/history` shows completed exchanges; `/session` shows the saved log path.
- `/clear` clears active context while preserving earlier turns in the log.
- `/quit` or EOF exits. `/help` lists commands.

Sessions save automatically under the platform's local data directory:
`%LOCALAPPDATA%\nectra\sessions` on Windows. The session path is printed when the
session opens and closes. `nectra -c` (or `nectra chat -c`) continues the newest
automatic session started in the current directory. Choose your own new file or
resume an existing one:

```powershell
.\target\release\nectra.exe chat --session .\my-session.jsonl
.\target\release\nectra.exe chat --resume .\my-session.jsonl
```

Resume from the same working directory as the original conversation. Session
files store your prompts, successful final answers, selected model IDs and errors
as local JSONL. They are not encrypted. Nectra locks the file against concurrent
writers, syncs each record, and can recover an incomplete final record after an
interruption. It never overwrites an existing file when creating a new session.
An interrupted or ordinary failed turn is not automatically rerun. In chat, a recognized
rate-limit rejection in **stream view** before any streamed work automatically retries the same context
on an available alternative, preferring the other provider. Each model is attempted
at most once per message. Unavailable model/provider selections are relaxed for that
turn with a visible explanation; `/model auto` restores normal routing preferences.

Limited models are skipped during a cooldown; account/usage-limit errors cool down
the provider. `/models` shows remaining cooldowns. Structured retry delays/reset times
are honored (up to 24 hours), otherwise the cooldown is one hour for an exhausted
usage limit and 60 seconds for other limits. Cooldowns are saved to
`cooldowns.json` in the local data directory, so a restarted chat still skips a
limited model until its cooldown expires. If all eligible models are limited,
chat stays open and you can send again later. Requests that already streamed activity
are never automatically replayed. Detection uses structured CLI error events; opaque
stderr-only errors still require manual recovery. This is reactive failover, not live
quota forecasting, and does not change one-shot execution.

Nectra owns the shared transcript. While one model keeps a task, Nectra resumes
that model's own session (Claude and Codex) and sends only the new message. When
another model takes over, it gets the outgoing model's handoff note with up to
16 KiB of the transcript, or the transcript alone when there is no note. Gemini
and Ollama get the transcript every turn. Only successful final answers enter shared context; raw tool output and
hidden provider state do not. In terminal view, per-launch provider hooks or
notifications capture completed replies separately from screen redraws. Follow-ups
typed within a provider UI stay with that provider until you return to Nectra;
captured completed exchanges are carried into the shared context. Failures leave the chat open so you can
choose another model and send a new message.

Context requests are bounded to 128 KiB of serialized UTF-8 text, preserving the
opening exchange when it fits and recent complete exchanges. The UI reports any
omitted turns; the log retains them. This is a byte budget, not a token guarantee.
Very large current messages are rejected. The session log limit is 64 MiB.

`nectra --dry-run chat` previews routes without model calls or adding new turns.
Availability means an enabled profile with an installed executable, not a
verified account entitlement. Authentication and model access remain with the
selected CLI. Profile IDs still come from the editable registry.

The adapters follow [Codex JSONL events](https://learn.chatgpt.com/docs/non-interactive-mode)
and [Claude streaming output](https://code.claude.com/docs/en/headless).

## Experimental two-provider debate

`nectra debate "Compare these approaches"` runs independent drafts, exchanges
critiques, and synthesizes one answer while preserving each author's dissent.
In chat, use `/debate on` and `/debate off`. This optional mode requires a local
Linux Docker engine and a dedicated image; it stops if isolation cannot be verified.
It normally uses five logical provider calls, with variable API and tool costs.

The implementation has offline tests; real container/provider validation is still
pending. See [setup, verification, and limitations](debate-isolation.md) before
using it. Normal routing does not require Docker.

## Build and try it

Install current stable Rust and a platform linker, then:

```sh
cargo test --all-targets --locked
cargo build --release --locked
./target/release/nectra route "debug this race condition"
./target/release/nectra doctor
```

On Windows PowerShell:

Run `./scripts/build.ps1` to validate, build and package a Windows x64 ZIP under
`dist/`. It also finds Rust in `%USERPROFILE%\.cargo\bin` when Cargo is not on PATH.

```powershell
.\target\release\nectra.exe route "debug this race condition"
.\target\release\nectra.exe doctor
.\target\release\nectra.exe --dry-run --mode best "review this architecture"
```

Run commands from the repository you want the agent to work on. Nectra preserves
that working directory. Codex may require a Git repository; it retains its own
repository checks, permissions and sandbox settings.

## Usage

```sh
nectra route "design a distributed system"
nectra --dry-run --mode fast "write tests for this module"
nectra --explain "refactor the auth service"
nectra --provider anthropic "review this architecture"
nectra --model gpt-5.6-sol "debug this error"
nectra run "implement the API"
cat task.txt | nectra route --stdin
cat task.txt | nectra run --stdin
nectra config --print
nectra models
nectra eval eval/tasks.json
nectra usage --days 14 --direct
```

In PowerShell use `Get-Content -Raw task.txt | nectra route --stdin`.
Place Nectra flags before prompt text. Use `--stdin` for multiline text or text
starting with a dash. Empty prompts and conflicting positional/stdin inputs fail.
`route` and `--dry-run` print JSON and never launch a model.
`--explain` writes a brief explanation to stderr and **still executes**.

`nectra usage` reads the provider session ids Nectra saves with each turn and
totals the tokens from Claude Code's and Codex's own logs. It prices them at the
list prices in `eval/chat-savings/prices.json`, and compares that with the same
tokens at `--baseline` prices (Opus by default). Each chat turn's log record also
notes why the router chose the model, whether it took over with a handoff note,
and whether it was an `/again` retry. `--json` includes those counts per model.
`--direct` adds Claude Code and Codex sessions that no Nectra chat recorded. Chat
prints the same running cost after each answer. Gemini and Ollama turns have no
token data. Claude Code subagent logs aren't counted on either side. On Windows,
a chat open in another Nectra is locked and left out. Chat logs that record
routing can't be resumed by Nectra 0.4.4 or older.

## Routing policy

The pipeline is: classification (Jev, or normalized word/phrase matching), task requirements, weighted
capability scores plus task-responsibility preferences, deterministic ranking, then native execution.
See [the responsibility matrix and access handling](routing.md).

- `fast` emphasizes the speed heuristic.
- `balanced` mixes quality, speed and cost-efficiency heuristics.
- `best` emphasizes quality heuristics.
- An explicit model/provider restricts initial selection. Chat reports unavailable
  selections and falls back when no work has started. Unknown/disabled IDs and
  conflicting one-shot selections still fail. Local-only policy remains enforced.
- Score ties resolve by model ID, independently of registry order.
- JSON includes all eligible candidates, the score gap, the effective mode,
  explicit-override status and an ambiguity flag. A single candidate has a null gap.
- The existing `confidence_margin` config key sets the ambiguity threshold.
  **There is no calibrated confidence probability.** Ambiguous routes still run;
  inspect with `route` or choose a model explicitly when needed.

Model capability numbers are editable product policy, not measured benchmarks,
prices, latency guarantees or proof that one provider is better. The sample corpus
checks classification regressions; it does not evaluate model output quality.
The local keyword classifier is English-only and does not understand negation,
inspect your repository or reliably resolve every mixed task.

### Jev classifier

On the bundled corpora the keyword classifier scores 38/38 (tuned), 10/20 (unseen
phrasing) and 13/40 (`eval/holdout.json`); Jev scores 37/38, 17/20 and 39/40. Jev also returns
a difficulty: below `simple_below` routes to the `simple`/`simple-coding` roles,
above `demanding_above` to `demanding`. Explicit words such as "quick" or
"complex" still apply. Configure it in `config.toml`:

```toml
[classifier]
backend = "auto"        # "auto" uses Jev when a key is available; "local" never calls it
timeout_ms = 1500
simple_below = 0.2      # Jev difficulty, 0..1
demanding_above = 0.6
```

The key comes from `TYPESAFE_API_KEY`, or from `typesafe.key` beside `config.toml`,
which `nectra setup` writes after checking the key with one live call. The key is
never stored in `config.toml`, so `nectra config --print` cannot show it. A Jev
difficulty tier only applies when some enabled model claims its role.

### Local Laya classifier

`nectra classifier laya` downloads a local model, about 750 MB, and every
file is checked against hashes pinned in the binary. It is checked on
download and again on every load, wherever `model_dir` points (a root-owned
install verified once by `nectra classifier laya --verify-only` skips the
re-hashing; see `docs/design/LAYA-MEMORY.md`). Prompts
never leave the machine.

```toml
[classifier]
backend = "laya"
# model_dir = "D:/models/laya"      # where the model lives (default: per-user data dir)
# allow_unpinned_model = false      # true loads your own retrained model; warns on every load
# laya_timeout_ms = 1000            # per message; slower answers fall back to keywords.
                                    # Unset: 5000 with 2 or fewer cores or no AVX2, else 1000
# laya_prepacking = false           # false keeps the weights in the mapped file: half the
                                    # private memory, 0-15% slower. Unset: false on Linux
                                    # with under 4 GiB RAM, true elsewhere
```

- **Chat** loads the model in the background at start, which takes about
  4.5 s. Until the model is ready, messages are classified by keywords.
- **Latency:** most messages take under 0.35 s; `route` shows when one fell
  back to keywords.
- **For lower latency,** set `laya_timeout_ms = 300`. On a busy machine about
  1 message in 9 then falls back to keywords, which costs about 4 points of
  accuracy.
- **A failed load** is remembered until Nectra restarts.
- **Accuracy:** 97.7% on a 1031-prompt held-out set (95% CI 96.6-98.4%). See
  `tools/laya/MODEL_CARD.md`.

`nectra eval` uses the configured classifier, so the same corpus compares both.
Difficulty saves tokens only when the registry has a cheap model claiming the
`simple` role (for example `haiku`).
Chat uses the prior completed user task as a routing hint for generic follow-ups
such as "do that"; the chosen model receives the shared transcript separately.

Claude profiles use its documented `opus` and `sonnet` aliases. Codex retains the
starter's `gpt-5.6-sol` and `gpt-5.6-luna` profiles. Availability, aliases and
supported effort levels depend on your installed CLI/account; edit the registry
for your deployment. Nectra does not probe access by making paid model calls.

## Configuration

Precedence is `--config PATH`, then the user's platform config directory, then
embedded defaults. A config is a complete replacement, not a partial merge.
Nectra never auto-loads configuration from the working repository.

- Windows: `%APPDATA%\nectra\config.toml`
- Linux: `$XDG_CONFIG_HOME/nectra/config.toml`, normally `~/.config/nectra/config.toml`
- macOS: `~/Library/Application Support/nectra/config.toml`

Copy both `config/nectra.toml` (rename to `config.toml`) and `config/models.toml`
to that directory, or use `--config config/nectra.toml` explicitly.
Relative registry paths resolve beside the config file. Embedded mode always uses
the embedded registry, even if an unrelated `models.toml` exists in the current directory.

The CLI mode overrides `routing.default_mode`. Invalid modes, unknown fields,
duplicate model IDs, unsupported provider/runtime pairs, nonfinite scores and
malformed effort values fail early. Config inspection and doctor do not require
a readable model registry.

Runtime commands accept an executable name on PATH or a path to an executable.
Use absolute paths for custom installations. Windows resolves native `.exe`
files, including the npm Codex package's native payload, without invoking its
PowerShell/batch shim. Relative PATH entries are ignored.
Trusted `extra_args` can set runtime options, but cannot replace Nectra's model,
effort, provider-selection flags or prompt transport.

## Execution and trust

Nectra passes arguments directly to the child process and sends prompt text over
stdin, preventing prompt text from becoming shell commands or CLI flags. Child
stdout/stderr stream to the terminal for one-shot execution. Chat parses response
events and displays assistant text while retaining final answers. Normal one-shot child exit codes are preserved;
Nectra's own errors and termination without a numeric status return 1.

The model registry permits Anthropic/Claude, OpenAI/Codex, Google/Gemini, and Ollama pairs. This is
a routing constraint, **not a network enforcement boundary**: downstream CLI
settings, environment variables, hooks, tools, executable paths and extra
arguments remain trusted user configuration. Authentication, endpoint selection,
permissions, tool execution and billing belong to those CLIs. Permission bypass is opt-in for each launch, as described below.

`doctor` checks configured executable discovery and `--version` with a 10-second
timeout per native runtime (60 seconds for Node-based CLIs). It does not check login status, model access or make a
model request. One-shot execution reports a missing selected runtime without retrying.
Chat filters missing runtimes and supports the rate-limit fallback described above.

Commands follow the [Codex CLI reference](https://learn.chatgpt.com/docs/developer-commands?surface=cli)
and [Claude Code CLI reference](https://code.claude.com/docs/en/cli-reference).
Claude aliases are described in [model configuration](https://code.claude.com/docs/en/model-config).

## Validation and releases

```sh
cargo fmt --all --check
cargo test --all-targets --locked
cargo clippy --all-targets --locked -- -D warnings
cargo run --locked -- eval eval/tasks.json
cargo build --release --locked
```

Commit `Cargo.lock` for reproducible dependency resolution. The repository has
GitHub Actions workflows for CI, tagged releases and a daily check against the
latest provider CLIs, but Actions is not running on this repository yet.
Releases are currently built by hand: Windows x64, Linux x64 and macOS Intel and
Apple Silicon archives, each with a SHA-256 sidecar.

Install scripts download from `MBarc/nectra` unless `NECTRA_REPO=owner/repository`
names a fork, and take an optional `NECTRA_VERSION=v0.3.0`. They reject
unsupported prebuilt platforms and verify the archive hash before installing.
Checksums detect corruption; they are not publisher signatures.

See [DESIGN.md](../DESIGN.md) for the design review and remaining work.

## Uninstall

Copies installed using the bundled installers include an uninstaller on PATH:

```text
uninstall-nectra
```

Use PowerShell on Windows, or a shell on Linux/macOS. Close running Nectra
sessions first. The Windows uninstaller removes `nectra.exe`, its helper script,
and the exact Nectra user-PATH entry. The Unix uninstaller removes the program
and helper from `~/.local/bin`; that shared PATH directory remains intact.
Both keep saved sessions, configuration, and unrelated programs.

From this source checkout, uninstall an installed Windows copy with:

```powershell
.\scripts\uninstall-nectra.ps1
```

Preview removal with `uninstall-nectra -WhatIf` on Windows or
`uninstall-nectra --dry-run` on Linux/macOS. Windows portable ZIP users can run
`./uninstall-nectra.ps1` from the extracted folder. An uninstaller beside the
Nectra executable automatically targets that folder, including portable copies.
Custom installation directories can be selected with `-InstallDirectory PATH`
on Windows or `--install-dir PATH` on Unix.

The uninstaller does not remove your source checkout, Rust toolchain, Claude
Code, or Codex. This project's `target/release/nectra.exe` is a local build,
separate from a copy installed into your user bin directory. Older installed
releases without a helper can use the scripts from this checkout.
# Provider setup

Both installers run `nectra setup` to ask which providers you have access to.
Selected Claude Code and Codex CLIs must resolve to native executables and pass
`--version` within 10 seconds (60 for Node-based CLIs) before setup saves. Missing/broken dependencies leave
setup incomplete, with instructions to install/repair the CLI and rerun setup.
These checks make no model calls and do not verify login or model entitlement.

The menu offers the four connections that can answer: Anthropic (Claude Code), OpenAI
(Codex), Google (Gemini CLI) and Ollama. Earlier versions also listed providers with no
adapter; configurations that saved those IDs still load, and the IDs do nothing.

Run `nectra setup` again to change your choices, or use
`nectra setup --providers anthropic,openai`. `--providers none` disables all providers.
For incremental changes use `nectra provider add anthropic`,
`nectra provider remove anthropic`, and `nectra provider list`. Adding preserves
existing selections; a first explicit add starts with only that provider. Changes
apply on the next launch, so restart/resume an already-open chat to reload them.
Removing a provider does not uninstall its CLI or delete its account credentials.
Unselected providers cannot be used by routing or rate-limit fallback. Existing
configurations without a `providers` setting retain their previous behavior.

For unattended installation set `NECTRA_PROVIDERS` to comma-separated IDs (or `none`).
An existing configuration is preserved and its selected CLI dependencies are checked.
A fresh unattended install without a selection reports setup incomplete; run setup
in a terminal. Setup does not install provider software, collect credentials, or
change provider accounts. Custom runtime paths, registry settings, and model files
are preserved; the config is rewritten as TOML, so its comments are not retained.

See [Ollama configuration](local-providers.md) for the implemented local connection.

## Gemini, colors, permissions, and login

Three themes are available: `default` (the original provider palette), `hacker`
(green/lime accents), and `ice-blue` (blue/cyan accents). Change and save a theme:

```sh
nectra theme hacker
nectra theme ice-blue
nectra theme default
nectra theme
```

Inside chat use `/theme hacker`, `/theme ice-blue`, or `/theme default`; `/theme`
shows the current choice. `nectra --theme ice-blue chat` overrides the saved theme
for one launch. Preferences live in `ui.toml` in Nectra's config directory (beside
an explicitly supplied config). Provider configuration and sessions are untouched.
Color controls and `NO_COLOR` apply to every theme; model reply text keeps its
normal terminal style. The activity indicator follows the selected theme.

`nectra provider add google` validates Gemini CLI and enables bootstrap profiles
for `gemini-2.5-pro` and `gemini-2.5-flash`. Define Google/Gemini entries in models.toml
to replace these defaults. Their scores are editable heuristics, not benchmark claims.
On Windows, npm installs are launched through Node.js and the Gemini JS entry point,
without passing prompts through cmd.exe. Configure `[gemini].command` for other installs.
Gemini uses stdin and stream-json for shared chat; its CLI may retain its own logs.

Human UI uses cyan navigation, purple Claude, green OpenAI, blue Gemini, teal Ollama,
green success, amber warnings/cooldowns, and red errors. Model reply text keeps the
terminal's normal style. `--color auto|always|never` controls Nectra output; automatic
color respects `NO_COLOR`, redirected output, and `TERM=dumb`. JSON and session files
are never decorated. Provider-owned login screens retain their own theme.

```sh
nectra chat --dangerously-skip-permissions
nectra provider login anthropic
nectra provider login openai
nectra provider login google
```

The permission flag applies to every CLI call and fallback for that launch, mapping
to Claude `--dangerously-skip-permissions`, Codex
`--dangerously-bypass-approvals-and-sandbox` (also disables its sandbox), and Gemini
`--approval-mode=yolo`. It overrides conflicting approval/sandbox extra_args, is not
saved in configuration or sessions, and must be supplied again when resuming.
Provider-enforced organizational policies still apply. Ollama remains chat-only.

Use `/login PROVIDER` inside a chat when authentication expires. Claude opens
`auth login`; Codex opens `login`; Gemini opens its interactive CLI, where `/auth`
changes authentication and `/quit` returns to Nectra. Nectra does not store credentials,
automatically log out, or replay interrupted work. Recognized structured authentication
errors exclude the provider until its login workflow closes successfully; the next
message verifies actual access. Opaque stderr-only errors may require manual login.

These integrations follow the [Gemini headless reference](https://geminicli.com/docs/cli/headless/),
[Gemini authentication guide](https://geminicli.com/docs/get-started/authentication/),
[Gemini CLI flags](https://geminicli.com/docs/cli/cli-reference/),
[Claude CLI reference](https://code.claude.com/docs/en/cli-reference), and
[Codex CLI reference](https://learn.chatgpt.com/docs/developer-commands?surface=cli).
