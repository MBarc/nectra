# Changelog

Changes to the `nectra run` contract (flags, events, exit codes) are always listed.

## Unreleased

- `nectra run --events jsonl`: unattended runs with a versioned JSON-lines event
  stream (schema 1: `route`, `text`, `tool`, `result`), typed `error_class` and
  exit codes, exact per-model usage and API-equivalent cost, `--resume`,
  `--max-cost-usd`, `--deadline`, `--no-session`, `--state-dir`, `--mcp-config`
  and `--no-mcp-share`. See [docs/headless.md](docs/headless.md).
- Release archives include `config/models.toml` and `config/nectra.toml`.
