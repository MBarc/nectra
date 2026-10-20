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
| H3 | Usage is counted from the provider's stream per model call (Claude per message id, Codex per turn, Gemini per model), never estimated. Claude's `result.modelUsage` is not used: on a resumed session it totals the whole session (measured on Claude Code 2.1.304: the second turn's `modelUsage` equalled turn 1 + turn 2). |
| H4 | Cost is the existing `eval/chat-savings/prices.json` table via `usage::price`, not a new registry field: one price table for `nectra usage` and headless. Unpriced models report `cost_usd: null` and are excluded under `--max-cost-usd`. |
| H5 | Error classes: N2's five plus `agent_error`, `cost_limit`, `deadline`, `cancelled`. Exit 1 = Nectra (same as every other Nectra error), 3 = the agent, 4 = a guard, 130 = cancelled. |
| H6 | Failover only before any work starts (the existing `safe_to_retry` signals), never on a resumed or `--model`-pinned task. |
| H7 | `--resume` reads a small record `<state>/runs/<id>.json` (model, provider session id, cwd). The id is validated as hex-and-dashes so it can't name another path. |
| H8 | `--mcp-config` lives on `Config` (one field) and is mapped in `mcp::for_launch`, so it reaches every launch path. Claude gets the file and `--strict-mcp-config`; Codex gets each server as `-c mcp_servers.NAME=…` with env values in the child's environment and the user's own config.toml servers disabled. Gemini models are excluded rather than run without tools. |
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
- Codex plugin-bundled MCP servers can't be disabled at launch.
- Codex/Gemini cost is only known at turn end, so `--max-cost-usd` stops them late.
