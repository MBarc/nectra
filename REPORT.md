# Nectra headless mode — report

Branch `MBarc/nectra-headless` (from master 5623d73). Contract: [docs/headless.md](docs/headless.md).
Design: [docs/superpowers/specs/2026-10-20-headless-mode.md](docs/superpowers/specs/2026-10-20-headless-mode.md).

## What was built

`nectra run --events jsonl` runs one routed task unattended and writes versioned JSON
lines (schema `v: 1`): `route` per attempt, `text`, `tool` start/end, and exactly one
`result` last. Plain `run` and chat are unchanged.

| Req | Status |
|---|---|
| N1 event stream | Done: route/text/tool/result, provider-neutral, `v` on every line. |
| N2 typed failures | Done: `model_unavailable, auth, rate_limited, harness_error, no_model` + `agent_error, cost_limit, deadline, cancelled`. Exit 1 Nectra, 3 agent, 4 guard, 130 cancelled. |
| N3 `--mcp-config` | Claude: `--mcp-config=FILE --strict-mcp-config`. Codex: plugins/apps off, every other server `codex mcp list` reports disabled, FILE's servers added (env values via the child env, never argv), then verified with `codex mcp list`; otherwise Codex is excluded. Gemini: not mapped; Gemini models are excluded when `--mcp-config` is set (documented). |
| N4 `--no-mcp-share` | Done (no sharing; Nectra never links skills on `run`). |
| N5 non-interactive | Done: never a pane, login or prompt; env passed through unchanged; per-runtime variable table in the docs. |
| N6 `--no-session` / `--state-dir` | Done: `--no-session` writes nothing (test asserts no state dir), Claude `--no-session-persistence`, Codex `--ephemeral`. |
| N7 release | Checklist + CHANGELOG in docs; release archives now ship `config/models.toml` and `config/nectra.toml` (workflow change). **No release published.** |
| N8 SIGTERM | Done on Unix: cancels the turn, kills the provider process group, `cancelled` result, exit 130 (tested on builder). |
| N9 `--mode`/`--provider` per task | Already global flags; covered by a test. |
| Swarm: `--resume` | Done: same model, same provider session (`--resume` / `codex exec resume`), only the new prompt is sent; same `session_id`. |
| Swarm: exact usage | Done: per model call from the stream, summed per model (`models[]`, incl. failover attempts), `cost_usd` from the price table, full route decision in `result.route`. |
| Swarm: guards | `--max-cost-usd`, `--deadline` → typed `cost_limit` / `deadline`, exit 4. |

### A finding worth knowing

Claude Code's `result.modelUsage` (and `total_cost_usd`) are **cumulative across a resumed
session**: measured on 2.1.304, turn 2's `modelUsage` was exactly turn 1 + turn 2
(`cacheRead 40621 = 19314 + 21307`). Anything that reads per-task usage from Claude's
result event over-counts follow-ups. Nectra sums each message's `message_start` /
`message_delta` usage instead; on a fresh session that sum matched Claude's own result
exactly (and its $0.00531963 to the cent with our haiku prices).

## Evidence

### Tests

- `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`: clean (Windows and Linux).
- New `tests/headless.rs` (fake Claude/Codex, no model calls): routed run + resume,
  cost guard, deadline, rate-limit failover, no_model, `--mode/--provider/--no-session`,
  `--mcp-config` (secret stays off argv, reaches the child env), and SIGTERM on Unix.
  All pass on Windows and on builder (Linux x86_64).
- 5 new unit tests in `src/headless.rs` (usage summing from the live Claude shape,
  Codex cache split, tool pairing, error classes/exit codes, flag parsing, session-id
  path safety).
- Full suite: lib 93 passed; chat 30; regression 25 (26 on Linux); ollama 6; providers 4;
  registry 5; runtime_fixes 2; setup 6; themes 3; stream_process ok.
- `native_terminal` (pre-existing, untouched code): on Windows it failed 2–4 of 6 while
  the desktop was loaded by other workers; run interleaved with pristine master it went
  6/6 for both, 3 times each. On Linux, `cancelling_native_work_does_not_save_a_fake_answer`
  fails on **pristine master too** (3/3 runs on builder), so it predates this branch.

### Real-provider demo (`python scripts/headless_demo.py`, Windows, Claude Code 2.1.304, Codex 0.162.1)

Throwaway git repo with `greet.py` returning `"Helo, "`. Fast mode, routed, cheapest models.
Run 1 also shows a **real failover**: the router picked gpt-5.6-luna, Codex answered with
the account's usage limit before doing any work, Nectra recorded `rate_limited` and the
task went to haiku. Total spend about $0.011. (`�` in the Codex error is the console
encoding of a curly apostrophe.)

```
=== 1. routed headless run
$ nectra --mode fast --dangerously-skip-permissions run --events jsonl --state-dir C:\Users\micha\AppData\Local\Temp\nectra-headless-vdsrbkip\state Fix the spelling mistake in the greeting string in greet.py. Edit only that line. Reply with one short sentence.
{"attempt":1,"mode":"fast","model":"gpt-5.6-luna","provider":"openai","reason":"local heuristic classified prompt as debugging; highest eligible score including task responsibility and complexity","resumed":false,"runtime":"codex","score":1.2127735614776611,"task":"debugging","type":"route","v":1}
{"attempt":2,"mode":"fast","model":"haiku","provider":"anthropic","reason":"local heuristic classified prompt as debugging; close heuristic scores; use --model to choose explicitly","resumed":false,"runtime":"claude","score":0.8872044682502747,"task":"debugging","type":"route","v":1}
{"id":"toolu_01N5nBDc3WGJKHSiRL37gbaX","model":"haiku","name":"Glob","phase":"start","type":"tool","v":1}
{"id":"toolu_01N5nBDc3WGJKHSiRL37gbaX","is_error":false,"model":"haiku","name":"Glob","phase":"end","type":"tool","v":1}
{"id":"toolu_01Vq2Se9mPNzUv7M5uV87Cqi","model":"haiku","name":"Read","phase":"start","type":"tool","v":1}
{"id":"toolu_01Vq2Se9mPNzUv7M5uV87Cqi","is_error":false,"model":"haiku","name":"Read","phase":"end","type":"tool","v":1}
{"id":"toolu_016WURV3Zi7tXtxNLEmrQKVL","model":"haiku","name":"Edit","phase":"start","type":"tool","v":1}
{"id":"toolu_016WURV3Zi7tXtxNLEmrQKVL","is_error":false,"model":"haiku","name":"Edit","phase":"end","type":"tool","v":1}
{"model":"haiku","text":"Fixed \"","type":"text","v":1}
{"model":"haiku","text":"Helo\" to \"H","type":"text","v":1}
{"model":"haiku","text":"ello\" in `greet.py","type":"text","v":1}
{"model":"haiku","text":"` line","type":"text","v":1}
{"model":"haiku","text":" 2; nothing else chang","type":"text","v":1}
{"model":"haiku","text":"ed.","type":"text","v":1}
{"attempts":[{"error":"You’ve hit your usage limit. Upgrade to Pro (https://chatgpt.com/explore/pro), visit https://chatgpt.com/settings/usage to purchase more credits or try again at 5:35 AM.","error_class":"rate_limited","model":"gpt-5.6-luna"},{"error_class":null,"model":"haiku"}],"cost_usd":0.0058538900000000005,"duration_ms":57686,"error":null,"error_class":null,"exit_code":0,"is_error":false,"model":"haiku","models":[{"cost_usd":0.0058538900000000005,"model":"claude-haiku-5-5","provider":"anthropic","routed_as":"haiku","usage":{"cache_read":126369,"cache_write":21832,"input":8,"output":446}}],"provider":"anthropic","route":{"ambiguous":true,"candidates":[{"model":"haiku","provider":"anthropic","score":0.8872044682502747},{"model":"sonnet","provider":"anthropic","score":0.8814254403114319},{"model":"opus","provider":"anthropic","score":0.7579585909843445}],"effort":"","mode":"fast","model":"haiku","overridden":false,"provider":"anthropic","reason":"local heuristic classified prompt as debugging; close heuristic scores; use --model to choose explicitly","runtime":"claude","score":0.8872044682502747,"score_margin":0.0057790279388427734,"task":"debugging"},"session_id":"11d4c5ca-b132-4d21-8810-58cf3f4e9dff","text":"Fixed \"Helo\" to \"Hello\" in `greet.py` line 2; nothing else changed.","type":"result","usage":{"cache_read":126369,"cache_write":21832,"input":8,"output":446},"v":1}
--- exit 0; schema ok; model haiku; usage {'cache_read': 126369, 'cache_write': 21832, 'input': 8, 'output': 446}; cost_usd 0.0058538900000000005; session 11d4c5ca-b132-4d21-8810-58cf3f4e9dff

=== 2. --resume follow-up
$ nectra --mode fast --dangerously-skip-permissions run --events jsonl --state-dir C:\Users\micha\AppData\Local\Temp\nectra-headless-vdsrbkip\state --resume 11d4c5ca-b132-4d21-8810-58cf3f4e9dff Now add a one-line docstring to greet. Reply with one short sentence.
{"attempt":1,"mode":"fast","model":"haiku","provider":"anthropic","reason":"local heuristic classified prompt as general; explicit constraints applied; highest eligible score including task responsibility and complexity","resumed":true,"runtime":"claude","score":1.1893036365509033,"task":"general","type":"route","v":1}
{"id":"toolu_01GWE6PBSvq7FfCVY3wu9Fyu","model":"haiku","name":"Edit","phase":"start","type":"tool","v":1}
{"id":"toolu_01GWE6PBSvq7FfCVY3wu9Fyu","is_error":false,"model":"haiku","name":"Edit","phase":"end","type":"tool","v":1}
{"model":"haiku","text":"Added the","type":"text","v":1}
{"model":"haiku","text":" docstring `","type":"text","v":1}
{"model":"haiku","text":"\"\"\"Return a greeting","type":"text","v":1}
{"model":"haiku","text":" for the given name.\"\"\"` to","type":"text","v":1}
{"model":"haiku","text":" `greet`","type":"text","v":1}
{"model":"haiku","text":" in `greet.py`.","type":"text","v":1}
{"attempts":[{"error_class":null,"model":"haiku"}],"cost_usd":0.00153636,"duration_ms":24632,"error":null,"error_class":null,"exit_code":0,"is_error":false,"model":"haiku","models":[{"cost_usd":0.00153636,"model":"claude-haiku-5-5","provider":"anthropic","routed_as":"haiku","usage":{"cache_read":79876,"cache_write":3136,"input":4,"output":220}}],"provider":"anthropic","route":{"ambiguous":false,"candidates":[{"model":"haiku","provider":"anthropic","score":1.1893036365509033}],"effort":"","mode":"fast","model":"haiku","overridden":true,"provider":"anthropic","reason":"local heuristic classified prompt as general; explicit constraints applied; highest eligible score including task responsibility and complexity","runtime":"claude","score":1.1893036365509033,"score_margin":null,"task":"general"},"session_id":"11d4c5ca-b132-4d21-8810-58cf3f4e9dff","text":"Added the docstring `\"\"\"Return a greeting for the given name.\"\"\"` to `greet` in `greet.py`.","type":"result","usage":{"cache_read":79876,"cache_write":3136,"input":4,"output":220},"v":1}
--- exit 0; schema ok; model haiku; usage {'cache_read': 79876, 'cache_write': 3136, 'input': 4, 'output': 220}; cost_usd 0.00153636; session 11d4c5ca-b132-4d21-8810-58cf3f4e9dff

=== 3. cost guard stop
$ nectra --mode fast --dangerously-skip-permissions run --events jsonl --state-dir C:\Users\micha\AppData\Local\Temp\nectra-headless-vdsrbkip\state --max-cost-usd 0.0001 Write a docstring for every function in greet.py.
{"attempt":1,"mode":"fast","model":"haiku","provider":"anthropic","reason":"local heuristic classified prompt as code_generation; close heuristic scores; use --model to choose explicitly","resumed":false,"runtime":"claude","score":0.889451801776886,"task":"code_generation","type":"route","v":1}
{"attempts":[{"error":"cost limit of $0.0001 reached; the agent was stopped","error_class":"cost_limit","model":"haiku"}],"cost_usd":0.00399562,"duration_ms":14869,"error":"cost limit of $0.0001 reached; the agent was stopped","error_class":"cost_limit","exit_code":4,"is_error":true,"model":"haiku","models":[{"cost_usd":0.00399562,"model":"claude-haiku-5-5","provider":"anthropic","routed_as":"haiku","usage":{"cache_read":19262,"cache_write":18964,"input":2,"output":20}}],"provider":"anthropic","route":{"ambiguous":true,"candidates":[{"model":"haiku","provider":"anthropic","score":0.889451801776886},{"model":"sonnet","provider":"anthropic","score":0.8815842866897583},{"model":"opus","provider":"anthropic","score":0.7576535940170288}],"effort":"","mode":"fast","model":"haiku","overridden":false,"provider":"anthropic","reason":"local heuristic classified prompt as code_generation; close heuristic scores; use --model to choose explicitly","runtime":"claude","score":0.889451801776886,"score_margin":0.007867515087127686,"task":"code_generation"},"session_id":"b88554d0-2de2-473a-8e47-c550d7924853","text":null,"type":"result","usage":{"cache_read":19262,"cache_write":18964,"input":2,"output":20},"v":1}
--- exit 4; schema ok; model haiku; usage {'cache_read': 19262, 'cache_write': 18964, 'input': 2, 'output': 20}; cost_usd 0.00399562; session b88554d0-2de2-473a-8e47-c550d7924853

--- final greet.py
def greet(name):
    """Return a greeting for the given name."""
    return "Hello, " + name

ALL CHECKS PASSED
```

## Astra review (round 1)

All eleven numbered findings and the TOP 3 were accepted by the coordinator and fixed
in 9fd4525. Astra's second review found several of these fixes incomplete; where a row
here differs from [round 3](#astra-review-round-3), round 3 is what the code does now. Each has a test; "unit" is `src/headless.rs`/`src/redact.rs`, "e2e" is
`tests/headless.rs` (fake Claude/Codex).

| # | Finding | Fix | Test |
|---|---|---|---|
| 1 | Codex resume double-counts (turn.completed is the thread total) | Confirmed in Michael's own Codex 0.162.1 logs: `total_token_usage` keeps growing across resumed turns. Usage now comes from Codex's rollout, one entry per model call (growth of the running total); a resumed thread's existing total is the baseline. `turn.completed` is used only for a new thread without a log (`--no-session`). | unit `resumed_codex_thread_counts_only_its_own_calls` (real numbers from a resumed rollout), e2e "Codex resume counts only the new turn" |
| 2 | Failed/interrupted work reported as zero-cost | Calls the provider did report always count (Codex's log keeps calls made before a crash). Work whose final count never arrived is flagged: `usage_complete: false` (top level and per model). Rejected-before-work attempts stay complete at 0. | unit `interrupted_codex_turn_keeps_logged_calls_and_is_flagged`, `interrupted_claude_call_counts_its_spend_and_is_flagged`; e2e "interrupted paid Codex turn"; live run 3 (cost stop) reports `usage_complete: false` |
| 3 | Claude cache-write TTL ignored | `cache_creation.ephemeral_5m/1h` kept; new `usage.cache_write_5m`; `cache_write_5m` prices (1.25× input) added to prices.json; 1-hour stays `cache_write` (2× input). Unsplit writes are taken as 1-hour (dearer, Claude Code's default). | unit `claude_cache_writes_are_priced_by_their_ttl`; e2e expects 625 of 2500 at the 5-minute rate |
| 4 | Unpriced model inherits the routed model's price | Removed the fallback: each call is priced as the model that answered (Claude's message model, Codex's `turn_context` model). An unpriced model with tokens makes `cost_usd: null`. | unit `a_model_without_a_price_is_never_given_another_ones` |
| 5 | Persistence errors skip the result | The run is one closure under `catch_unwind`; any error becomes a `harness_error` result, a panic too. A session record that can't be written is a `warnings` entry (the task's own outcome stands, `session_id: null`). | e2e "persistence failure still ends in one result" |
| 6 | Codex explicit MCP not strict (project layers) | Reproduced with real Codex 0.162.1: a trusted project's `.codex/config.toml` server is listed and loaded. Now: plugins and apps off, every server `codex mcp list` reports in the task's directory disabled, FILE's added, then `codex mcp list` must show exactly FILE's servers or Codex is excluded (reason on stderr). | e2e "project and plugin MCP servers can't leak into Codex" (incl. a server that ignores the override → Codex excluded, Claude still runs) |
| 7 | Explicit MCP servers share credentials | One variable with two values, between servers or against Nectra's env, excludes Codex (it forwards variables by name from one environment). Claude gets each server's own env from FILE. | e2e "conflicting MCP credentials keep the task off Codex" |
| 8 | Claude replay/interleaving | Per-field running maximum per message id (a replayed `message_start` can't lower counts); the open message is tracked per stream (`parent_tool_use_id`); whole `assistant` messages also count, for subagent calls without raw events. | unit `replayed_and_interleaved_claude_messages_count_once_each`; the e2e fake replays a `message_start` |
| 9 | Gemini output excludes thinking | Gemini usage is always `usage_complete: false`, documented. | unit `codex_cached_input_is_not_counted_twice_and_models_stay_apart` |
| 10 | Deadline/signals start after stdin and routing | Headless reads its own prompt; stdin and routing run on a worker thread the main thread waits on, bounded by the deadline and cancel. | e2e "deadline while stdin stays open" (stdin never closed → `deadline`, exit 4, nothing launched) |
| 11 | Session records not private | Folder created 0700, record written through a 0600 temp file and renamed into place. | Linux run (below) |
| — | Credentials in diagnostics/errors | Provider stderr is forwarded line by line through `redact`; every `error` string too. Masks credential-named env values, `--mcp-config` env values, `sk-`/`ghp_`/`github_pat_`/`xox?-`/`AIza` tokens, `Bearer …`, JWTs. | unit `credentials_are_masked_and_prose_is_not`; e2e "credentials are redacted" (Claude stderr, Codex error) |
| — | Codex cost guard is after the fact | Now checked after every Codex model call (from its log), stated plainly in the docs: a Codex call can't be stopped part way, and without a log (`--no-session`) the check is at turn end. | e2e "interrupted paid Codex turn" reads the log mid-turn |

### Independent check against the providers' own accounting

`scripts/headless_demo.py` now compares each task's `usage` with the provider's own
session log (Claude Code: `~/.claude/projects/<slug>/<session>.jsonl`, unique assistant
message ids; Codex: its rollout) and `cost_usd` with Claude Code's `total_cost_usd`
(new result field `provider_cost_usd`). Live, Windows, Claude Code 2.1.304,
`python scripts/headless_demo.py --provider anthropic`:

```
=== 1. routed headless run
--- exit 0; schema ok; model haiku; usage {'cache_read': 131826, 'cache_write': 22130, 'cache_write_5m': 0, 'input': 8, 'output': 437}; cost_usd 0.00596356
--- independent check, run 1: Nectra {'cache_read': 131826, 'cache_write': 22130, 'cache_write_5m': 0, 'input': 8, 'output': 437}
    provider log {'cache_write': 22130, 'cache_write_5m': 0, 'input': 8, 'output': 437, 'cache_read': 131826}
    cost: Nectra 0.00596356, Claude Code 0.00596356
=== 2. --resume follow-up
--- exit 0; schema ok; model haiku; usage {'cache_read': 77689, 'cache_write': 335, 'cache_write_5m': 0, 'input': 4, 'output': 188}; cost_usd 0.00093829
--- independent check, --resume: Nectra {'cache_read': 77689, 'cache_write': 335, 'cache_write_5m': 0, 'input': 4, 'output': 188}
    provider log {'cache_write': 335, 'cache_write_5m': 0, 'input': 4, 'output': 188, 'cache_read': 77689}
    session cost: Nectra 0.00690185, Claude Code 0.00690185
=== 3. cost guard stop
--- exit 4; schema ok; model haiku; usage {'cache_read': 19263, 'cache_write': 18921, 'cache_write_5m': 0, 'input': 2, 'output': 14}; cost_usd 0.00398403
    (usage_complete: false — the call was cut off before its output count)
ALL CHECKS PASSED
```

Usage equals Claude Code's own log exactly on the fresh and the resumed task, and the
cost equals Claude Code's own `total_cost_usd`; on the resumed session Claude's figure is
the whole session's, and it equals Nectra's two tasks added together. Claude Code wrote
every cache entry at the 1-hour TTL here (`cache_write_5m: 0`).

### Tests after the review

- Windows: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings` clean;
  `cargo test` all green (100 unit tests, 14 headless e2e scenarios, every other suite).
- Linux (builder, `~/nectra-headless`): fmt and clippy clean; all 15 headless scenarios
  pass, including SIGTERM and the 0700/0600 session-record check. Two failures there are
  not this branch's and show up on unchanged master too: `native_terminal`
  `cancelling_native_work_does_not_save_a_fake_answer` (fails every time on Linux), and
  `tests/chat.rs`, where a different test fails in about one run in four (master: 2 of 8
  runs, each time a different test; these tests use a mock backend and none of the code
  changed here).

## Astra review (round 3)

Astra's re-review (`astra-headless-review-2.md`, verdict HOLD) marked 1–6 and 9 partly
fixed and found N1–N8. The coordinator accepted all of them. One commit per item,
test first ("unit" = `src/*.rs` tests, "e2e" = `tests/headless.rs`).

| # | Finding | Fix | Test | Commit |
|---|---|---|---|---|
| N1 | Codex's model never read: the kind is the rollout entry's `type`, not the payload's | Read `entry.type == "turn_context"`, `payload.model`. Fixtures now use the shape of a real Codex 0.162.1 rollout (checked on this desktop). The demo checks Nectra's model against the rollout's. | unit `resumed_codex_thread_counts_only_its_own_calls` (answering model differs from the routed one) | e167e58, 3523deb |
| N2 / 1 | A missing or late resume baseline counts history | `based` is set only when the resumed thread's log was read before the turn; otherwise nothing is counted from the log and the turn is `usage_complete: false`. Rollout lookup matches `…-<thread>.jsonl` and thread/revert's `…-<thread>_<n>.jsonl`, requires `session_meta.id` to be the thread, and takes the newest. | unit `a_resumed_codex_thread_without_its_baseline_never_counts_history`, `the_active_rollout_of_a_thread_is_found_by_its_id` | 39905a9 |
| N3 / 2 | `usage_complete` doesn't prove completeness | Codex: `turn.started` counts as work; from a known baseline, what `turn.completed`'s thread total has beyond the log is counted as the rest of the turn (so a missing tail is counted, not dropped). Claude: a run that sent `system` and never its `result` is incomplete. A refusal safe to send elsewhere (no activity: usage limit, sign-in, unavailable model) is complete at 0; this was seen live, a Codex usage-limit refusal now reports `usage_complete: true`, $0. | unit `a_codex_log_missing_its_tail_is_completed_from_the_turns_total`, `a_run_cut_off_before_any_count_is_never_called_free` | 4f3dd24, 5b16431 |
| 9 | Gemini result without stats looked complete | No per-model stats ⇒ an incomplete entry. | unit `a_gemini_result_without_stats_is_not_complete` | fdcf0f8 |
| 3 | A write without its TTL priced as 1-hour | `cost_usd` is `null` for it (and per model); the cost guard holds it at the dearer 1-hour rate. Live Claude Code 2.1.304 sends the split, so live costs stay exact. | unit `a_claude_cache_write_without_its_ttl_has_no_price` | 319c971 |
| 4 | Any `claude-*` name took its family's price | Full names resolve only through `prices.json` `_aliases` (`claude-opus-5-5`, `claude-sonnet-5-5`, `claude-haiku-5-5`); `claude-opus-4-8` etc. have no price, in `nectra usage` too. | unit `only_listed_model_versions_get_a_price` | 027223a |
| N6 | Codex `--no-session` skips the cost guard | The guard is checked again with each attempt's final count; an attempt that succeeded but went over the limit becomes `cost_limit` (exit 4). A failed attempt keeps its own class. | e2e "Codex without a log still meets the cost guard" | 05afd6f |
| N4 / 6 | MCP probes don't see the launch's arguments or environment | Probes get the servers' environment and Codex `extra_args` `--enable`/`--disable`; sandbox, approval and colour arguments (`-s`, `-a`, `--color`, `--skip-git-repo-check`, `--full-auto`, `--yolo`, `--dangerously-bypass-approvals-and-sandbox`) are left out of the probes; any other Codex extra argument (`--cd`, …) excludes Codex under `--mcp-config`. (`--config`, `--profile`, `--model` are already refused in `extra_args` by config validation.) | e2e "MCP is checked against Codex's own extra arguments" (`--enable plugins` re-adds a plugin server ⇒ excluded; `--cd` ⇒ excluded) | 3e7b218 |
| N5 | Probes ignore deadline and cancel | `executor::output_until`: own process group/job, killed at the deadline or a cancel; `--mcp-config` loading also runs under `bounded()`, and its deadline/cancel keep their error classes. | e2e "a hung `codex mcp list` meets the deadline" | 9404f1e |
| N7 / 5 | Interrupt-handler setup could exit without a result | Setup and the run go through one `settle()` boundary: setup error, run error or panic ⇒ `harness_error`. | unit `setup_failures_errors_and_panics_all_become_one_outcome` | 50d5a04 |
| N8 | Credentials reach stdout through Codex `error` events; short secrets; split at flush | `emit()` masks every string of every event. Credential-named values masked from 4 characters (except `true`/`false`/`none`/`null`/`auto`), other `--mcp-config` env values from 8. Diagnostics are flushed through the last line end, or the last space of a line over 64 KiB (all but its last 4 KiB without one). | e2e "credentials are redacted" (Codex `error` event, a 5-character key); unit `a_stream_is_only_cut_between_words`, `credentials_are_masked_and_prose_is_not` | 8e8ee78 |

Live after round 3 (Windows, Claude Code 2.1.304, `python scripts/headless_demo.py
--provider anthropic`): ALL CHECKS PASSED. Run 1 usage {input 8, cache_read 133908,
cache_write 23736, output 387} equals Claude Code's log, cost $0.00628058 equals its
`total_cost_usd`; the `--resume` follow-up equals its log too, and the session cost
$0.00745435 equals Claude Code's; the cost-guard run ends `cost_limit` with
`usage_complete: false`. Codex is still at its usage limit (until 3:38 PM today), so the
Codex half stays open.

What is still not guaranteed, by design (as corrected in round 4): a credential Nectra
doesn't know and that has no token shape isn't masked; `--max-cost-usd` is a threshold,
not a ceiling, and can overshoot by one Claude call (its input and cache as well as its
output), one Codex call, or one turn of Codex without a log or of Gemini; a resumed Codex
turn whose log can't be read is reported incomplete rather than counted.

## Astra review (round 4)

Astra's third review (`astra-nectra-headless-3.review.md`, of 461f61f, verdict HOLD)
found A–F. The coordinator accepted all of them. Each code fix has a regression test
written before it. A, B, C's MCP collection and C's stdout deltas were seen failing
first; C's diagnostics-cut test and D's test use the changed signatures, so they could
not run against the old code; E's check can only catch the race on Unix.

| # | Finding | Fix | Test | Commit |
|---|---|---|---|---|
| A (P1) | A cut-short or garbled historical rollout counted as a verified baseline (1000 + unterminated 1500 ⇒ 700 charged for a 200-token turn) | A baseline read fails when the log doesn't end in a line end or a non-blank line isn't JSON; then nothing is counted from the log and the turn is `usage_complete: false`. | unit `a_cut_short_or_garbled_codex_log_is_no_baseline` (both cases) | ffeee1a |
| B (P1) | Tail recovery charged recovered tokens to history's model | Baseline reads never record a model. Calls and the recovered rest are charged to this attempt's own `turn_context` model, else `unknown` (unpriced ⇒ `cost_usd: null`); without a log, to the `--model` Nectra launched. | unit `recovered_codex_usage_is_never_charged_to_historys_model` (no context ⇒ `unknown`; this turn's context ⇒ its model) | ffeee1a |
| C (P1) | Forced stderr cut split a known secret; stdout deltas masked one by one; MCP secrets of servers with `cwd`/restrictions not collected | `redact::ready` never cuts inside a known credential or a prefix of one at the end, nor inside a character, and a forced cut keeps max(longest credential − 1, 4 KiB). Reply deltas go through `redact::Stream` (held back at a partial word, flushed before tool events and at the attempt's end). `--mcp-config` collects every server's `env`. Spec's "never cut mid-word" replaced with what holds. | unit `a_credential_is_never_cut_in_two` (73,728-byte line, `48213` across byte 69,632), `a_reply_in_pieces_is_masked_whole`, `every_server_claude_is_handed_has_its_credentials_masked`; e2e "credentials are redacted" (a key split across two Claude deltas) | 8a3feba |
| D (P2) | `settle()` dropped `TurnCancellation` before the result was emitted | `settle` borrows the owner; `run()` drops it after the result event. | unit `the_cancellation_owner_outlives_the_run` | e2d9958 |
| E (P2) | `bounded()` returned without the MCP probe worker, so `process::exit` could beat probe cleanup | `bounded()` gives its work a stop token; on deadline or cancel it cancels it and waits up to a grace period (3 s for `--mcp-config`, 0 for stdin and routing, which own no process) for the worker to kill and reap the probe. | e2e "a hung `codex mcp list` meets the deadline" now also checks the probe's PID is gone when Nectra exits | de1453a |
| F (P2) + nits | Claude guard called a ceiling; overshoot said to be output only; REPORT rows N6/N4 and spec H8 overstated | docs/headless.md and spec H4: a threshold checked when usage arrives, overshoot of one Claude call includes its input and cache, unpriced usage doesn't count. N6 row: only a successful attempt becomes `cost_limit`. N4 row and spec H8: sandbox/approval/colour arguments are skipped, not excluding. | docs only | ed6b8dd |

Tests after round 4: Windows `cargo fmt --check` and `cargo clippy --all-targets -- -D
warnings` clean, `cargo test` all green (115 unit tests, 17 headless e2e scenarios, every
other suite). Linux (builder, `~/nectra-headless`): fmt and clippy clean, 115 unit tests
and all 18 headless scenarios pass, SIGTERM included. On Linux `native_terminal`
fails the same way on this branch and on 461f61f (3 runs each):
`cancelling_native_work_does_not_save_a_fake_answer` every time,
`undersized_resize_keeps_provider_work_alive` in some runs. `tests/chat.rs` failed one of
two runs, a different test each time, as before. The live Codex check is still open (see
below).

## Astra review (round 5)

Astra's fourth review (`astra-nectra-headless-4.review.md`, of 03b2917, verdict HOLD)
marked A, B, D, E and F fixed, and C partly fixed, with two regressions caused by C. The
coordinator accepted all three. Each test was written first and seen failing (the
non-UTF-8 test by panicking). All three fixes are in 8a9f421. This supersedes round 4's
C row: the reply is no longer flushed before tool events.

| # | Finding | Fix | Test |
|---|---|---|---|
| 1 (P1) | A tool event (a subagent's included) flushed the reply, so `482` → tool → `13` showed a known secret in two pieces | The reply stream is held across tool events and flushed only when the attempt ends; a `text` event can come after a `tool` event that arrived mid-word. Docs and spec H11 say this instead of "masked as one text". | e2e "credentials are redacted" (the key split around a subagent's `tool_use`) |
| 2 (P1, regression) | `Bearer ` went out alone, then its token unmasked | `redact::ready` never cuts right after a trailing `Bearer`, so its token goes out with it, in one delta or across deltas. | unit `a_bearer_token_is_masked_however_it_streams` (3 splits); e2e, both forms |
| 3 (P2, regression) | Stderr that isn't UTF-8 (a leading 0x80, no line end) underflowed the cut and failed the turn | Only a forced cut backs off to a character start, and never below 0; other bytes are left to the lossy decoding. | unit `bytes_that_arent_utf_8_never_break_the_cut` |

Tests after round 5: Windows `cargo fmt`, `cargo clippy --all-targets -- -D warnings`
clean, `cargo test` all green (117 unit tests, 17 headless scenarios, every other suite).
Linux (builder): fmt and clippy clean, 117 unit tests and all 18 headless scenarios pass,
SIGTERM included.

## Astra review (round 6)

Astra's fifth review (of a179ba6, verdict HOLD) marked round 5's three findings fixed and
found one regression, which the coordinator confirmed. The test was written first and seen
failing ("attempt to subtract with overflow").

| # | Finding | Fix | Test |
|---|---|---|---|
| 1 (P2, regression) | A forced cut subtracted max(longest credential − 1, 4 KiB) from the pending length unsaturated, so a registered credential longer than over 64 KiB of stderr with no space underflowed and failed the turn | `saturating_sub` again, as in round 4. The other subtractions on these paths are guarded (`len() >= 6` for `Bearer`, credentials are at least 4 bytes, the call index follows a push). `ready` now passes the credentials to an inner `cut`, so the test needn't register a 100 KB credential for every other test. | unit `a_credential_longer_than_the_stream_never_breaks_the_cut` |

Tests after round 6: Windows `cargo fmt`, `cargo clippy --all-targets -- -D warnings`
clean, `cargo test` all green (118 unit tests, every other suite). Not rerun on Linux:
the change is platform-independent arithmetic in one function.

Tests after round 3: Windows `cargo fmt --check`, `cargo clippy --all-targets -- -D
warnings` clean, `cargo test` all green (109 unit tests, 17 headless e2e scenarios, every
other suite). Linux (builder, `~/nectra-headless`): fmt and clippy clean, 109 unit tests
and all 18 headless scenarios pass, SIGTERM and the 0700/0600 record check included.

## Open issues

- **Gemini:** `--mcp-config` not mapped (Gemini models excluded instead); usage parsing
  is from Gemini CLI's source (`result.stats.models`), not verified live (Gemini is signed
  out on this desktop); no Gemini prices, so `cost_usd` is `null` for it.
- **Prices:** `eval/chat-savings/prices.json` has opus, sonnet, haiku, gpt-5.6-sol,
  gpt-5.6-luna, gpt-6-astra only. fable, gpt-6.x-sol/luna and gpt-5.6-terra report
  `cost_usd: null` and are skipped under `--max-cost-usd`. Adding their list prices is a
  data change, not code.
- **Cost guard granularity:** Claude is checked at the start and end of every API call
  (the call that crosses the limit is billed: run 3 stopped at $0.0039 against a $0.0001
  cap); Codex after every model call (from its log); Codex with `--no-session` and Gemini
  only at the end of the turn, which then ends `cost_limit` if it went over.
- **Live Codex cross-check not run:** Codex was at its usage limit again (until 3:38 PM
  today), so the Codex half of the independent check (`python scripts/headless_demo.py
  --provider openai`, which now also checks the answering model) hasn't run against a
  real Codex. The rollout format and the resume numbers in the tests are taken from real
  Codex 0.162.1 logs on this desktop; round 2 showed fixtures alone can hide a schema
  mistake (N1), so this run matters.
- **Claude subagents:** counted from their streamed events and their whole `assistant`
  messages (merged by message id); not exercised against a real subagent run.
- **Linux live run:** the real-provider demo ran on Windows only; builder has no provider
  logins. The Linux path is covered by the fake-provider suite including SIGTERM.
- PonsOS can drop `--mcp-config`/`--strict-mcp-config` from `extra_args` once it passes
  `--mcp-config` to Nectra (cross-product note in the spec).
