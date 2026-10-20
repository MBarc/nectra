# Headless mode — design

**Date:** 2026-10-20 · **Branch:** `MBarc/nectra-headless` · **Contract:** [docs/headless.md](../../headless.md)

## Why

Two consumers run Nectra unattended and need machine-readable output:

- **PonsOS** agent VMs launch Nectra as the agent harness (PonsOS integration design
  §1, decisions I1–I7, requirements N1–N9).
- **The Orca swarm coordinator** launches workers through Nectra and A/B-measures
  token use against plain Claude Code, so it needs exact per-task usage and cost,
  and follow-up tasks (review fix rounds) that continue the same session.

## Decisions

| # | Decision |
|---|---|
| H1 | One switch, `nectra run --events jsonl`. The other headless flags require it; plain `run` and chat are unchanged. |
| H2 | Events are JSON lines on stdout, `route`/`text`/`tool`/`result`, each with `"v": 1`. Exactly one `result`, always last, whatever happens after argument parsing and config load. |
| H3 | Usage is the provider's own count per model call, never estimated. Claude: per message id from the stream, merged by running maximum per stream (`parent_tool_use_id`), TTL split kept. Codex: per call from its rollout log (running totals after each call; the model from `turn_context` entries), because `turn.completed.usage` is the thread's total and on a resumed thread would count history again. A resumed thread is counted from the log only after its baseline was read before the turn; `turn.completed`'s total, from a known baseline, supplies what the log hasn't shown. Gemini: `result.stats`, always flagged incomplete (no thinking tokens). Claude's `result.modelUsage`/`total_cost_usd` total the whole session on a resumed one (measured on 2.1.304), so they are only a cross-check (`provider_cost_usd`). |
| H3a | `usage_complete: true` only when the provider's final word accounts for every attempt (Claude's `result`, Codex's `turn.completed` from a known baseline, a refusal before any work); an attempt that started (Claude's `system` event, Codex's `turn.started`) and never got it is `false`. `usage`/`cost_usd` then count what was reported (a lower bound), never 0 for unknown work. |
| H4 | Cost is the existing `eval/chat-savings/prices.json` table via `usage::price`, not a new registry field: one price table for `nectra usage` and headless. Each call is priced as the model that answered, never the routed alias; an unpriced model with tokens makes `cost_usd: null`. Full names resolve only through the table's `_aliases`, never by family. Claude writes have a 1-hour and a 5-minute rate; a write without its TTL makes `cost_usd: null` (the guard takes the 1-hour rate). After each attempt the guard is checked again with its final count. |
| H5 | Error classes: N2's five plus `agent_error`, `cost_limit`, `deadline`, `cancelled`. Exit 1 = Nectra (same as every other Nectra error), 3 = the agent, 4 = a guard, 130 = cancelled. |
| H6 | Failover only before any work starts (the existing `safe_to_retry` signals), never on a resumed or `--model`-pinned task. |
| H7 | `--resume` reads a small record `<state>/runs/<id>.json` (model, provider session id, cwd). The id is validated as hex-and-dashes so it can't name another path. |
| H8 | `--mcp-config` lives on `Config` (one field) and is mapped in `mcp::for_launch`, so it reaches every launch path. Claude gets the file and `--strict-mcp-config`. Codex has no strict mode, so Nectra turns plugins and apps off, disables every server `codex mcp list` reports in the task's directory (user, project and system layers), adds the file's servers, and verifies with `codex mcp list` that exactly those are on; otherwise Codex is excluded. The probes get the launch's environment and its `--enable`/`--disable` extra arguments; any other Codex extra argument excludes Codex. They run in their own process group/job, bounded by the deadline and cancel. A variable with two values (two servers, or against Nectra's env) also excludes Codex, which forwards variables by name from one environment. Gemini models are excluded rather than run without tools. |
| H10 | Exactly one result after argument parsing and config load: the interrupt-handler setup and the run go through one boundary (`settle`), so any error, including persistence, becomes a result (`harness_error`, or a `warning` when only the session record failed) and a panic becomes `harness_error`. The deadline and cancel cover reading `--stdin` and routing (both run on a worker thread the main thread waits on). |
| H11 | Credentials are masked in every string of every stdout event and in forwarded provider stderr (never cut mid-word): credential-named values from 4 characters (a few setting words excepted), other `--mcp-config` env values from 8, and token-shaped words. |
| H9 | SIGTERM on Unix: a signal handler that only sets an atomic, watched by a thread that cancels the turn token, which kills the provider's process group. Chat's ctrlc setup is untouched (enabling ctrlc's `termination` feature would have changed chat's SIGTERM behaviour). |

## Cross-product contracts relied on or created

- **C-N1 (created):** the event schema, error classes and exit codes in
  docs/headless.md, version 1. PonsOS maps `route` → `task.routed`, `result.usage` /
  `cost_usd` → `metadata.usage`, `error_class` → task outcome.
- **C-N2 (relied on):** PonsOS passes `--dangerously-skip-permissions` itself; Nectra
  adds no permission flags.
- **Release (N7):** Linux x64 archive + `.sha256` (existing workflow), now with
  `config/models.toml` and `config/nectra.toml` inside; `nectra --version` prints
  `nectra X.Y.Z`.

## Not done

- Gemini `--mcp-config` (needs a per-launch system settings file); Gemini usage
  parsing is from source, not a live run.
- `--max-cost-usd` can overshoot by one Codex call (no mid-call usage), or one turn
  for Codex without a log (`--no-session`) and Gemini.
