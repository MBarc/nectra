# Changelog

Changes to the `nectra run` contract (flags, events, exit codes) are always listed.

## Unreleased

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
- Credentials are masked in provider diagnostics and `error` strings.
