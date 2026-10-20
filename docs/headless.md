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
| `--max-cost-usd N` | Stop once the task's API-equivalent cost passes N USD. Only models with a price are routed to. |
| `--deadline T` | Stop after T: seconds, or `90s`, `10m`, `2h`. Counts from Nectra's start, across failover. |
| `--no-session` | Write nothing: no session record, no cooldown state. Claude runs with `--no-session-persistence`, Codex with `--ephemeral`. `session_id` is `null`. |
| `--state-dir DIR` | Keep session records (`DIR/runs/ID.json`) and cooldowns (`DIR/cooldowns.json`) here. Default: the user data directory's `nectra` folder, the same cooldowns chat uses. |
| `--mcp-config FILE` | Give the agent exactly the servers in FILE (standard `{"mcpServers": {...}}`). See [MCP](#mcp-servers). |
| `--no-mcp-share` | Don't add servers from the user's other CLI configs (Nectra's [sharing](reference.md)). |

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
 "usage":{"input":5,"cache_read":40913,"cache_write":21393,"output":301},
 "cost_usd":0.00468,
 "models":[{"model":"claude-haiku-5-5","routed_as":"haiku","provider":"anthropic",
            "usage":{"input":5,"cache_read":40913,"cache_write":21393,"output":301},"cost_usd":0.00468}],
 "attempts":[{"model":"haiku","error_class":null}],
 "route":{"task":"...","model":"haiku","candidates":[...], "...": "the full decision, as `nectra route` prints it"},
 "duration_ms":8123,"exit_code":0}
```

| Field | |
|---|---|
| `is_error`, `error_class`, `error` | Outcome; `error` is the message, `null` on success. |
| `text` | The final answer, `null` on failure. |
| `session_id` | Pass to `--resume`. `null` with `--no-session`, or when the runtime can't resume (Gemini). A task stopped by a guard still gets one. |
| `model`, `provider` | The model that ran last. |
| `usage` | Tokens for this task, summed over every model call it made. `input` excludes cached input. |
| `cost_usd` | API-equivalent USD for `usage`, or `null` when some call's model has no price. |
| `models` | `usage` and `cost_usd` per model. `model` is the name the provider reported (Claude Code may use more than one model in a task); `routed_as` is the Nectra model id the attempt was routed to. Failover attempts appear here too. |
| `attempts` | Each model tried, in order, with its `error_class` and `error`. |
| `route` | The last routing decision in full. |

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

Usage is the providers' own token counts, not estimates:

- **Claude Code:** each API call's `message_start` and final `message_delta` usage,
  one entry per message id. Claude's `result.modelUsage` is not used because on a
  resumed session it totals the whole session, not the task (seen on 2.1.304).
- **Codex:** `turn.completed.usage`; `cached_input_tokens` is moved out of
  `input_tokens` into `cache_read`.
- **Gemini:** `result.stats.models` (`input`, `cached`, `output_tokens`). Read from
  Gemini CLI's source, not yet verified against a live run. Thinking tokens may not
  be included.

Prices are `eval/chat-savings/prices.json` (the same table as `nectra usage`), in
USD per million tokens for input, cache read, cache write and output. Models
without a price (currently fable, gpt-6.x and Gemini) give `cost_usd: null` and
can't be routed to under `--max-cost-usd`.

`--max-cost-usd` is checked on every usage update: per API call for Claude (at the
call's start, then as output streams), once per turn for Codex and Gemini, so a
Codex or Gemini task is only stopped at its end. When a guard stops a task, the
call in flight counts the output it had streamed.

## Failover

When a model fails before doing any work (rate limit, model rejected, login
needed, or its CLI is missing), the task goes to the next eligible model: a new
`route` event and an entry in `attempts`. Once work has started, a failure is
final, because the agent may already have changed files. A resumed task, or one
pinned with `--model`, never moves to another model.

## Resume

A result's `session_id` names a record of the model and the provider session
(`<state-dir>/runs/<id>.json`). `nectra run --events jsonl --resume <id> "fix the
review findings"` continues that provider session: Claude with `--resume`, Codex
with `codex exec resume`. The agent keeps what it read and ran, the prompt cache
is warm, and only the new prompt is sent. The follow-up's `usage` counts only its
own calls; its `session_id` is the same. Run it from the directory the session
started in: Claude keeps sessions per project directory.

## MCP servers

With `--mcp-config FILE`, exactly the servers in FILE are given to the agent:

- **Claude:** `--mcp-config=FILE --strict-mcp-config`.
- **Codex:** each stdio server as `--config mcp_servers.NAME={command, args, env_vars}`,
  with `env` values passed in the child's environment, never on the command line.
  The servers in the user's own `~/.codex/config.toml` are switched off for the
  launch (`mcp_servers.NAME.enabled=false`). Servers bundled by Codex plugins can't
  be switched off this way and stay on. A server Codex can't take as written (not
  stdio, a name outside `[A-Za-z0-9_-]`, or an env value that differs from the same
  variable in Nectra's own environment) keeps the task off Codex.
- **Gemini:** not supported yet. Gemini CLI has no launch-time server option (a
  per-launch settings file through `GEMINI_CLI_SYSTEM_SETTINGS_PATH` is the likely
  route). With `--mcp-config`, Gemini models aren't routed to, so a task never runs
  without its tools.

Ollama is never used headless: it runs no tools and reports no usage.

## Credentials and the environment

Nectra reads no credential and logs none. Each provider CLI inherits Nectra's whole
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
   `python scripts/headless_demo.py` against real providers.
4. Tag `vX.Y.Z`. The release workflow builds `nectra-x86_64-unknown-linux-gnu.tar.gz`
   with a `.sha256`, and the archive carries `config/models.toml` and
   `config/nectra.toml`. `nectra --version` prints `nectra X.Y.Z` on its first line.
