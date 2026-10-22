# Changelog

Changes to the `nectra run` contract (flags, events, exit codes) are always listed.

## Unreleased

- Laya: `laya_timeout_ms` now bounds inference only; the load and ONNX Runtime
  first-run setup happen before the engine is used. The route line says why
  keywords answered: `keywords · Laya still loading`, `keywords · Laya failed to
  load: ...` or `keywords · Laya timed out: ...`. `nectra chat` loads Laya in the
  background once per session. Memory and latency on small machines:
  [docs/design/LAYA-MEMORY.md](docs/design/LAYA-MEMORY.md); a shared per-machine
  service is designed in [docs/design/LAYA-SHARED.md](docs/design/LAYA-SHARED.md).
- `nectra run --events jsonl`: unattended runs with a versioned JSON-lines event
  stream (schema 1: `route`, `text`, `tool`, `result`), typed `error_class` and
  exit codes, exact per-model usage and API-equivalent cost, `--resume`,
  `--max-cost-usd`, `--deadline`, `--no-session`, `--state-dir`, `--mcp-config`
  and `--no-mcp-share`. See [docs/headless.md](docs/headless.md).
- Release archives include `config/models.toml` and `config/nectra.toml`.
- Schema 1 additions (compatible): `usage.cache_write_5m`, `usage_complete` (top
  level and per model), `provider_cost_usd` and `warnings` in the result event.
- Codex usage is read per model call from Codex's own log, so a resumed Codex
  task no longer counts the thread's history, and `--max-cost-usd` is checked
  after every Codex call instead of at turn end.
- Claude cache writes are priced at the 5-minute or 1-hour rate they were made
  at (new `cache_write_5m` prices); cost uses only the answering model's price.
- `--mcp-config` holds Codex to exactly the file's servers, project-level and
  plugin servers included, or keeps the task off Codex.
- Credentials are masked in every event on stdout and in provider diagnostics,
  from 4 characters for credential-named variables.
- Codex's answering model is read from its log as Codex writes it; a resumed
  Codex thread is counted from its log only from a baseline read before the
  turn, and `turn.completed` fills in what the log missed. Full model names
  get a price only when `prices.json` lists them (`_aliases`). A Claude cache
  write without its TTL makes `cost_usd` null.
- `usage_complete` is false for a run that started and never got the
  provider's final count, and for a Gemini result without stats.
- `--max-cost-usd` is checked once more when each attempt ends, so Codex
  `--no-session` can't pass the limit and succeed.
- `--mcp-config` checks Codex with its extra arguments and environment, and
  `codex mcp list` is bounded by `--deadline` and cancel.
- A resumed Codex thread whose log has a cut-short or unreadable record is
  `usage_complete: false`, not counted from it. Codex calls are charged to the
  model the turn's own log entry names, or `unknown` (no price), never to one
  from the thread's history.
- `text` events of a streamed reply are masked across deltas, so a credential
  split between two isn't shown; every `--mcp-config` server's `env` values are
  masked. `text` events may now be held back until a word ends, even past a
  `tool` event.
- A Ctrl+C while the result is written cancels instead of exiting without it,
  and a stopped `codex mcp list` is killed before Nectra exits.
