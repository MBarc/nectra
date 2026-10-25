# Changelog

Changes to the `nectra run` contract (flags, events, exit codes) are always listed.

## Unreleased

- Laya `laya-model-1-alpha.2` (`nectra-laya` `1~alpha.2`): routes more hard,
  high-stakes specs to the demanding model and improves task accuracy on every
  held-out set; see tools/laya/MODEL_CARD.md. The high-stakes keyword rule is
  gone. `nectra eval` reports tier accuracy, demanding-routing errors and
  keyword fallbacks for corpora with `tier` labels.
- `[knowledge] index = "PATH"` and the global `--knowledge PATH`: that file goes
  to every provider as a block with a random per-process tag, `run --no-tools`,
  `--serve` and debate included (Claude through `--append-system-prompt`, Ollama
  in its system message, Codex, agy and Gemini in front of the prompt; for all of
  them a `<system-context` in the prompt is neutralised). It is read only when the file
  and every directory above it are root's alone (administrators' on Windows),
  with no symlink or junction on the path, at most 16 KiB; otherwise Nectra stops
  with an error, and `run --events jsonl` reports a `harness_error` result. See
  [docs/reference.md](docs/reference.md#knowledge-index).
- `nectra run --events jsonl --serve`: one process answers request after request
  (`{"prompt": "..."}` per stdin line, the usual events per request), loading Laya
  once instead of once per prompt. New `ready` event, written only by `--serve`.
  See [docs/headless.md](docs/headless.md#many-tasks-in-one-process---serve).
  `--serve` also answers from Ollama models, so an Ollama-only machine works.
  A line that isn't UTF-8 or is over 1 MiB gets an error result instead of ending
  the server.
- `nectra run --events jsonl --no-tools`: answers only, no tools, files or shell,
  enforced by each CLI's flags. Claude runs with `--tools "" --strict-mcp-config`;
  Claude also gets `--setting-sources "" --settings '{"disableAllHooks":true}'
  --disable-slash-commands` and an empty working directory, so no hook, helper or
  command runs shell. It conflicts with `--resume` and `--mcp-config`.
  Ollama is chat-only; Codex, agy and Gemini are refused. See
  [docs/headless.md](docs/headless.md#answers-only---no-tools).
- Laya: `nectra classifier laya` no longer panics on Linux ("Failed to load ONNX
  Runtime dylib"). Every load now opens ONNX Runtime from the checked model
  directory by its full path, and a runtime that won't load is an error.

- Google now runs through the Antigravity CLI (`agy`): Google ended personal-account
  sign-in for Gemini CLI. New runtime `agy`; bundled models
  `gemini-3.8-flash-high|medium|low` replace `gemini-2.5-*` (capability scores are
  unmeasured placeholders), priced from Google's published `gemini-3.8-flash` rates
  (`eval/chat-savings/prices.json`). `nectra run` sessions resume via agy's
  conversation id. `/login google` opens `agy`. The `gemini` runtime stays only for
  API-key setups that declare their own models. agy's quota and sign-in errors are
  classified as `rate_limited` / `auth`; the strings are from Google's docs and agy's
  binary, not a live capture (`tests/fixtures/agy-limit-and-auth.jsonl`).

- Laya: `laya_timeout_ms` now bounds inference only; the load and ONNX Runtime
  first-run setup happen before the engine is used. The route line says why
  keywords answered: `keywords · Laya still loading`, `keywords · Laya failed to
  load: ...` or `keywords · Laya timed out: ...`. `nectra chat` loads Laya in the
  background once per session. Memory and latency on small machines:
  [docs/design/LAYA-MEMORY.md](docs/design/LAYA-MEMORY.md); a shared per-machine
  service is designed in [docs/design/LAYA-SHARED.md](docs/design/LAYA-SHARED.md).
- Laya on small machines: `laya-onnx` is vendored with a prepacking switch
  (`classifier.laya_prepacking`, off by default on Linux under 4 GiB RAM: half
  the private memory); `laya_timeout_ms` defaults to 5000 with 2 or fewer cores or
  no AVX2; a root-owned model install verified once with
  `nectra classifier laya --model-dir DIR --verify-only` skips the 667 MiB
  re-hash on every load (user-owned installs still hash in full).
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
