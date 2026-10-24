# Headless mode: `nectra run --events jsonl`

`nectra run --events jsonl` routes one task, runs it unattended on the chosen
provider CLI, and reports it as JSON lines on stdout. It is the contract for
programs that use Nectra as an agent harness, such as PonsOS agent VMs and the
Orca swarm coordinator. Interactive chat and plain `nectra run` are unchanged.

```sh
nectra --config /etc/pons/nectra/config.toml --dangerously-skip-permissions \
  run --stdin --events jsonl --no-session < prompt.txt > transcript.jsonl
```

Schema version: **1**. Every line carries `"v": 1`. An incompatible change to any
event bumps it and gets a changelog entry.

## Flags

Global flags apply per task: `--model ID` pins a model (routing off), `--mode
fast|balanced|best`, `--provider anthropic|openai|google`, `--stdin`,
`--config FILE`, `--dangerously-skip-permissions`. These need `--events jsonl`:

| Flag | Meaning |
|---|---|
| `--resume ID` | Continue the task whose result event had `session_id` ID: same model, same provider session, only the new prompt is sent. Run it from the same directory. |
| `--max-cost-usd N` | Stop once the task's API-equivalent cost passes N USD. Only models with a price are routed to. See [when it is checked](#usage-and-cost). |
| `--deadline T` | Stop after T: seconds, or `90s`, `10m`, `2h`. Counts from the start of the run, across reading `--stdin`, routing and failover. |
| `--no-session` | Write nothing: no session record, no cooldown state. Claude runs with `--no-session-persistence`, Codex with `--ephemeral`. `session_id` is `null`. |
| `--state-dir DIR` | Keep session records (`DIR/runs/ID.json`) and cooldowns (`DIR/cooldowns.json`) here. Default: the user data directory's `nectra` folder, the same cooldowns chat uses. |
| `--mcp-config FILE` | Give the agent exactly the servers in FILE (standard `{"mcpServers": {...}}`). See [MCP](#mcp-servers). |
| `--no-mcp-share` | Don't add servers from the user's other CLI configs (Nectra's [sharing](reference.md)). |
| `--serve` | Answer many tasks in one process, reading requests from stdin. See [Many tasks in one process](#many-tasks-in-one-process---serve). |
| `--no-tools` | Answers only: no tools, files or shell. See [Answers only](#answers-only---no-tools). |

## Events

One JSON object per line, in this order: one or more `route` (one per attempt),
any number of `text` and `tool`, then exactly one `result`, always last. Nothing
else is written to stdout. Diagnostics go to stderr.

### `route`

```json
{"v":1,"type":"route","attempt":1,"model":"haiku","provider":"anthropic","runtime":"claude",
 "task":"code_generation","mode":"fast","score":1.07,"reason":"jev classified ...","resumed":false}
```

A second `route` means the first model failed before doing any work and another
took the task ([failover](#failover)).

### `text`

`{"v":1,"type":"text","model":"haiku","text":"Fixed the "}`. Assistant text as the
provider streams it: deltas for Claude, whole messages for Codex and Gemini.

### `tool`

`{"v":1,"type":"tool","model":"haiku","phase":"start","name":"Bash","id":"toolu_01..."}`,
then `"phase":"end"` with the same `id`, plus `"is_error"` when the provider says.
Names are the provider's: Claude's tool name, Codex's item type
(`command_execution`, `file_change`, `mcp_tool_call`...), Gemini's tool name.

### `result`

```json
{"v":1,"type":"result","is_error":false,"error_class":null,"error":null,
 "text":"Fixed the typo in README.md.",
 "session_id":"5f0c...","model":"haiku","provider":"anthropic",
 "usage":{"input":8,"cache_read":131826,"cache_write":22130,"cache_write_5m":0,"output":437},
 "usage_complete":true,"cost_usd":0.00596356,"provider_cost_usd":0.00596356,
 "models":[{"model":"claude-haiku-5-5","routed_as":"haiku","provider":"anthropic",
            "usage":{"input":8,"cache_read":131826,"cache_write":22130,"cache_write_5m":0,"output":437},
            "usage_complete":true,"cost_usd":0.00596356}],
 "attempts":[{"model":"haiku","error_class":null}],
 "route":{"task":"...","model":"haiku","candidates":[...], "...": "the full decision, as `nectra route` prints it"},
 "warnings":[],"duration_ms":8123,"exit_code":0}
```

| Field | |
|---|---|
| `is_error`, `error_class`, `error` | Outcome; `error` is the message, `null` on success. |
| `text` | The final answer, `null` on failure. |
| `session_id` | Pass to `--resume`. `null` with `--no-session`, when the runtime can't resume (Gemini), or when the session record couldn't be saved (see `warnings`). A task stopped by a guard still gets one. |
| `model`, `provider` | The model that ran last. |
| `usage` | Tokens for this task, summed over every model call it made. `input` excludes cached input. `cache_write` includes `cache_write_5m`, the part Claude wrote at its 5-minute cache rate; the rest is at the 1-hour rate when Claude says so (see `cost_usd`). |
| `usage_complete` | `true` only when the provider's final word accounts for every attempt: Claude's `result`, Codex's `turn.completed` total from a known starting point, or a refusal before any work (a usage limit, a sign-in, an unavailable model). `false` when an attempt started and that word never came (a guard, a cancel, a crash), for a resumed Codex thread whose log couldn't be read before the turn, and for any Gemini run (thinking tokens aren't reported). `usage` and `cost_usd` then count what was reported, a lower bound; they never drop work that was reported. |
| `cost_usd` | API-equivalent USD for `usage`, each model at its own price, or `null` when a model that used tokens has no price, or Claude wrote to the cache without saying at which TTL. Never 0 for unpriced work. |
| `provider_cost_usd` | Claude Code's own `total_cost_usd`, for cross-checking; `null` for other runtimes. On a resumed session it covers the whole session, not this task. |
| `models` | `usage`, `usage_complete` and `cost_usd` per model. `model` is the model the provider says answered (Claude Code may use more than one in a task; Codex's from the attempt's own `turn_context` in its log, or `unknown`, which has no price, when the log didn't name one); `routed_as` is the Nectra model id the attempt was routed to. Failover attempts appear here too. |
| `attempts` | Each model tried, in order, with its `error_class` and `error`. |
| `route` | The last routing decision in full. |
| `warnings` | Problems that didn't change the outcome, such as a session record that couldn't be written. |

Every run that gets past argument parsing and config loading ends with exactly
one `result`, whatever fails after that: the interrupt handler can't be set up,
an unwritable state directory, a full disk or a bug in Nectra (`harness_error`).
Every string in every event on stdout, and provider diagnostics on stderr, have
credentials masked as `[redacted]`: values of 4 characters or more of variables
whose names hold `KEY`, `TOKEN`, `SECRET`, `PASSW`, `CREDENTIAL` or `AUTH` (Nectra's
own, and `--mcp-config` `env`), except `true`, `false`, `none`, `null` and `auto`;
any other `--mcp-config` `env` value of 8 characters or more; and token-shaped
words (`sk-...`, `ghp_...`, `Bearer ...`, JWTs). The `env` values of every server
in `--mcp-config` count, including those Codex can't take. A reply streamed in
pieces goes out in `text` events cut only after a word: the end of what has come
so far waits, through any `tool` events in between, while it may be part of a
word, a known credential, or the token after `Bearer`. So a `text` event can come
after a `tool` event that arrived in the middle of its word. Diagnostics go out by line (a line over 64
KiB at its last space, or else all but its last 4 KiB), never cut inside a known
credential. A credential Nectra doesn't know and that has no token shape isn't
masked. The interrupt handler stays Nectra's until the `result` is written, so a
Ctrl+C while it is being put together cancels rather than exits.

### Error classes and exit codes

| `error_class` | Exit | Meaning |
|---|---|---|
| `null` | 0 | Completed. |
| `harness_error` | 1 | Nectra's own failure: an empty prompt, an unreadable `--mcp-config`, unknown or misplaced `--resume`, provider CLI missing. |
| `no_model` | 1 | No eligible model (none enabled, all tried, all cooling down, or the pinned one is excluded). |
| `model_unavailable` | 3 | The provider rejected the model. |
| `auth` | 3 | The provider CLI needs a login. Nectra never opens a login flow in headless mode. |
| `rate_limited` | 3 | The provider's rate or usage limit. A cooldown is recorded unless `--no-session`. |
| `agent_error` | 3 | Any other failure of the agent: a failed turn, a crash, an incomplete stream. |
| `cost_limit` | 4 | `--max-cost-usd` stopped it. |
| `deadline` | 4 | `--deadline` stopped it. |
| `cancelled` | 130 | SIGINT, or SIGTERM on Unix. |

Exit 1 is every Nectra error, including ones before the run starts (an unreadable
`--config`, which also gets no result event), and exit 2 is a command-line usage
error from the argument parser. Exits 3 and up are always the agent's or a guard's,
and always come with a result event.

## Usage and cost

Usage is the providers' own token counts, never estimates, per model call:

- **Claude Code:** each message's usage from the stream (`message_start`, then
  the final `message_delta`), one entry per message id. Counters only grow, so a
  repeated or late snapshot of a message never lowers it, and each stream (the
  main agent, or a subagent under its `parent_tool_use_id`) tracks its own open
  message. A subagent's whole `assistant` messages count too, for calls whose raw
  stream isn't forwarded. Cache writes keep their TTL split
  (`cache_creation.ephemeral_5m_input_tokens` / `ephemeral_1h_input_tokens`); a
  write outside the split has no known rate, so `cost_usd` is `null` (the cost
  guard holds it at the dearer 1-hour rate). Claude's `result.modelUsage` and
  `total_cost_usd` are not used for `usage`: on a resumed session they total the
  whole session (seen on 2.1.304).
- **Codex:** its own log of the thread (`$CODEX_HOME/sessions/**/rollout-*-<thread>.jsonl`,
  or `..._<n>.jsonl` after `thread/revert`; the newest whose first line names the
  thread), which records the thread's running total after every model call, and
  the model in each `turn_context` entry. Each call is the growth since the last
  one. A resumed thread starts from the total already in the log, read whole
  before the turn starts; if it can't be read then, or a record in it is cut
  short or unreadable, nothing is counted from the log (it would include
  history) and the turn is `usage_complete: false`. `turn.completed.usage` is
  the thread's running total too (Codex 0.162.1): from a known starting point,
  whatever of it the log hasn't shown is counted as the rest of the turn, which
  is all of it without a log (`--no-session`, which runs Codex `--ephemeral`).
  Calls are charged to the model in this attempt's own `turn_context`, never one
  from history; when the log names none, to `unknown` (so `cost_usd: null`), and
  without a log to the model Nectra launched Codex with (`--model`). Cached input
  is moved out of `input` into `cache_read`.
- **Gemini:** `result.stats.models` (`input`, `cached`, `output_tokens`), read
  from Gemini CLI's source, not yet verified against a live run. Thinking tokens
  aren't in these stats, so Gemini usage is always `usage_complete: false`.

Prices are `eval/chat-savings/prices.json` (the same table as `nectra usage`), in
USD per million tokens, for the model that answered, never the model it was
routed as: an unpriced model inside a priced run makes `cost_usd` `null`. A full
name a provider reports (`claude-haiku-5-5`) has a price only when the table's
`_aliases` lists it; another version of the same family has none. Claude
models have a 1-hour (`cache_write`) and a 5-minute (`cache_write_5m`) write
rate. Models without a price (currently fable, gpt-6.x, gpt-5.6-terra and Gemini)
can't be routed to under `--max-cost-usd`.

`--max-cost-usd` is a threshold checked whenever usage arrives, not a ceiling:
the call in flight counts what it has reported, and what is reported is already
spent.

- **Claude:** at the start of every model call, once its input and cache counts
  arrive, and at its end. By then the call's input and cache are billed, and its
  output is only reported at its end, so one call can overshoot by all of its
  input, cache and output.
- **Codex:** after every model call, at the next event Codex prints once the
  call is in its log. A Codex call can't be stopped part way, and with
  `--no-session` (no log) the check happens only after the whole turn.
- **Gemini:** only after the whole turn.

Once an attempt that succeeded ends, the guard is checked again with its final
count, so a turn whose usage is first known at its end (Codex without a log,
Gemini) and went past the limit ends as `cost_limit`, not as a success; an
attempt that failed keeps its own error class. So the run can end over the
limit by one call (Claude, Codex) or one turn (Codex without a log, Gemini).
Usage charged to a model without a price (`unknown` included) doesn't count
towards it.

## Failover

When a model fails before doing any work (rate limit, model rejected, login
needed, or its CLI is missing), the task goes to the next eligible model: a new
`route` event and an entry in `attempts`. Once work has started, a failure is
final, because the agent may already have changed files. A resumed task, or one
pinned with `--model`, never moves to another model.

## Resume

A result's `session_id` names a record of the model and the provider session
(`<state-dir>/runs/<id>.json`, written whole through a rename; on Unix the folder
is 0700 and the file 0600). `nectra run --events jsonl --resume <id> "fix the
review findings"` continues that provider session: Claude with `--resume`, Codex
with `codex exec resume`. The agent keeps what it read and ran, the prompt cache
is warm, and only the new prompt is sent. The follow-up's `usage` counts only its
own calls; its `session_id` is the same. Run it from the directory the session
started in: Claude keeps sessions per project directory.

## MCP servers

With `--mcp-config FILE`, exactly the servers in FILE are given to the agent:

- **Claude:** `--mcp-config=FILE --strict-mcp-config`.
- **Codex:** Codex has no strict mode and loads servers from its user config,
  trusted project configs (`.codex/config.toml`) and plugins. So for each task
  Nectra turns plugins and apps off (`features.plugins=false`,
  `features.apps=false`), asks `codex mcp list --json` in the task's directory which
  servers are still on and turns each of those off by name, and adds FILE's
  servers as `--config mcp_servers.NAME={command, args, env_vars}`, with `env`
  values passed in the child's environment, never on the command line. Then it
  asks `codex mcp list` again with all of that: unless exactly FILE's servers are
  on, Codex models aren't routed to and the reason goes to stderr. Both questions
  are asked as the launch will run: with the servers' environment and the Codex
  `extra_args` that switch features (`--enable`, `--disable`). Sandbox, approval
  and colour settings (`--sandbox`/`-s`, `--ask-for-approval`/`-a`, `--color`,
  `--skip-git-repo-check`, `--full-auto`, `--yolo`,
  `--dangerously-bypass-approvals-and-sandbox`) can't change which servers load
  and are left out of the questions; any other extra argument (`--cd`, say) keeps
  the task off Codex. They count against `--deadline` and stop on a cancel, and
  anything they leave running is killed (and Nectra waits up to 3 s for that)
  before Nectra writes its result. Codex also isn't used when FILE has a server
  it can't take as written (not stdio, a name outside `[A-Za-z0-9_-]`), or when one
  variable has two values, either between two servers or against Nectra's own
  environment: Codex passes variables by name from one environment, so a server
  would get another's credential.
- **Gemini:** not supported yet. Gemini CLI has no launch-time server option (a
  per-launch settings file through `GEMINI_CLI_SYSTEM_SETTINGS_PATH` is the likely
  route). With `--mcp-config`, Gemini models aren't routed to, so a task never runs
  without its tools.

Ollama is used headless only with `--serve` or `--no-tools`: it runs no tools and
reports no usage (its `cost_usd` is 0), so it answers a front end but takes no
agent's task.

## Credentials and the environment

Nectra reads no credential and masks the ones it can recognise in what it
passes on (see [result](#result)). Each provider CLI inherits Nectra's whole
environment unchanged, so it finds its own:

| Runtime | Variables it reads |
|---|---|
| Claude Code | `ANTHROPIC_API_KEY`, `ANTHROPIC_AUTH_TOKEN`, `ANTHROPIC_BASE_URL`, `CLAUDE_CONFIG_DIR`, or its stored login |
| Codex | `OPENAI_API_KEY`, `CODEX_HOME`, or its stored login |
| Gemini CLI | `GEMINI_API_KEY`, `GOOGLE_API_KEY`, `GEMINI_CLI_HOME`, or its stored login |
| Jev (routing) | `TYPESAFE_API_KEY`; without it routing is local and no prompt leaves the machine |

Headless mode never opens a terminal UI, a login flow or an approval prompt: the
prompt goes to the CLI's print/exec mode over stdin and stdin is then closed.
Approvals are the CLI's own non-interactive defaults: without
`--dangerously-skip-permissions`, Claude denies a tool that needs approval and
Codex stays in its sandbox, neither asks. Nectra adds no permission flags the
caller didn't pass.

## Many tasks in one process: `--serve`

For a front end that asks question after question (a chat window), one process per
prompt pays the Laya load every time: about 13 s and 850 MiB on 2 vCPU. With
`--serve` the process stays up, loads Laya once and answers each request:

```sh
nectra run --events jsonl --no-session --serve
```

- Laya (when `classifier.backend = "laya"`) loads before the first request is
  read, then `{"v":1,"type":"ready"}` is written. Requests sent earlier wait in
  stdin, so none routes with keywords. Start the process when the window opens.
- Each stdin line is one request, `{"prompt": "..."}`; blank lines are skipped.
  Each is answered with the usual events, `route`, `text`, `tool`, ending in exactly
  one `result` (with `usage` and `cost_usd`), before the next line is read. A line
  that isn't a request gets a `result` with `error_class` `harness_error`.
- Every flag applies to every task: `--model`, `--mode`, `--provider`,
  `--deadline` (per task), `--max-cost-usd` (per task), `--mcp-config`. `--resume`
  isn't available; each request is routed afresh, so send earlier exchanges in the
  prompt.
- It runs the same agents as one-shot headless runs, with their permission
  defaults ([below](#credentials-and-the-environment)), and Ollama models too, so
  an Ollama-only machine works. An Ollama answer arrives as one `text` event.
  For a front end that must run nothing, add `--no-tools`.
- Closing stdin ends it with exit code 0. SIGTERM during a task cancels it, writes
  its `cancelled` result and exits 130; between tasks it exits 143 at once. Ctrl+C
  during a task cancels only that task and the next request is read; between tasks
  it exits 130.

## Answers only: `--no-tools`

The agent gets no tool use, no file access and no shell, enforced by the CLI's own
launch flags, never by prompt text. A runtime that can't be held to that is refused:

| Runtime | Under `--no-tools` |
|---|---|
| Claude Code | `--tools "" --strict-mcp-config`, no MCP servers, none of `claude.extra_args`, no permission flags |
| Ollama | Chat only already; a reply that asks for tools fails |
| Codex | Refused: its read-only sandbox still reads files and runs commands |
| agy, Gemini CLI | Refused: plan mode still reads files |

Refused models aren't routed to; pinning one with `--model` fails with `no_model`
before anything is launched. Checked live (Claude Code 2.1.310): asked to create a
file, Claude printed a tool call as text and nothing was created.

## Signals

On Unix, SIGTERM or SIGINT stops the provider's whole process group (Nectra starts
each in its own group) and Nectra exits 130 within about two seconds, after its
`cancelled` result event. On Windows, Ctrl+C does the same through the job object.

## For PonsOS

```
/usr/local/bin/nectra --config /etc/pons/nectra/config.toml --dangerously-skip-permissions \
  [--model <id>] run --stdin --events jsonl --no-session \
  --mcp-config /etc/pons/mcp.json --no-mcp-share [--mode fast|balanced|best]
```

With `--mcp-config` doing the mapping, `extra_args` no longer needs `--mcp-config`
or `--strict-mcp-config`. Map `error_class` straight to the task outcome, the
`route` event to `task.routed`, and `result.usage`/`cost_usd` to `metadata.usage`.

## Release checklist (run contract)

A change to the flags above, any event's fields, or the exit codes:

1. Bump `VERSION` in `src/headless.rs` if it is incompatible (a field removed or
   renamed, a meaning changed). Adding a field or an error class is compatible.
2. Update this file and add a `CHANGELOG.md` entry under the release.
3. `cargo test --test headless` (fake providers) on Windows and Linux;
   `python scripts/headless_demo.py --provider anthropic` and `--provider openai`
   against real providers: it checks usage against each provider's own session
   log and cost against Claude Code's `total_cost_usd`.
4. Tag `vX.Y.Z`. The release workflow builds `nectra-x86_64-unknown-linux-gnu.tar.gz`
   with a `.sha256`, and the archive carries `config/models.toml` and
   `config/nectra.toml`. `nectra --version` prints `nectra X.Y.Z` on its first line.
