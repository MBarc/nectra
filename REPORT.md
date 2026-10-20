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
| N3 `--mcp-config` | Claude: `--mcp-config=FILE --strict-mcp-config`. Codex: `-c mcp_servers.NAME=…`, env values via the child env (never argv), user's config.toml servers disabled. Gemini: not mapped; Gemini models are excluded when `--mcp-config` is set (documented). |
| N4 `--no-mcp-share` | Done (no sharing; Nectra never links skills on `run`). Codex plugin-bundled servers can't be disabled at launch (documented). |
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

## Open issues

- **Gemini:** `--mcp-config` not mapped (Gemini models excluded instead); usage parsing
  is from Gemini CLI's source (`result.stats.models`), not verified live (Gemini is signed
  out on this desktop); no Gemini prices, so `cost_usd` is `null` for it.
- **Prices:** `eval/chat-savings/prices.json` has opus, sonnet, haiku, gpt-5.6-sol,
  gpt-5.6-luna, gpt-6-astra only. fable, gpt-6.x-sol/luna and gpt-5.6-terra report
  `cost_usd: null` and are skipped under `--max-cost-usd`. Adding their list prices is a
  data change, not code.
- **Cost guard granularity:** Claude is checked per API call (the call that crosses the
  limit is billed: run 3 stopped at $0.0040 against a $0.0001 cap); Codex/Gemini only at
  turn end.
- **Claude subagents:** usage from Task-tool subagents is counted only if Claude Code
  streams their `message_start`/`message_delta` events; not verified.
- **Codex plugin MCP servers** stay on under `--mcp-config` (only config.toml servers
  can be disabled with `-c`).
- **Linux live run:** the real-provider demo ran on Windows only; builder has no provider
  logins. The Linux path is covered by the fake-provider suite including SIGTERM.
- PonsOS can drop `--mcp-config`/`--strict-mcp-config` from `extra_args` once it passes
  `--mcp-config` to Nectra (cross-product note in the spec).
