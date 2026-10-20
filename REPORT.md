# Report: Nectra keeps up with the provider CLIs automatically

Branch `MBarc/nectra-cli-autoadapt` · 2026-10-20 · task `task_d7c6d856f763`

## What was built

- **`scripts/compat/adapt.py`** is the adaptation loop:
  1. It observes CLI versions, help text, model lists and (with `--live`) one raw stream-json turn per provider.
  2. It analyses that against the registry and the previous run.
  3. It puts mechanical fixes on a local `compat/auto-<hash>` branch, made in a throwaway worktree, with `cargo test` run there.
  4. It applies the registry part to the installed `%APPDATA%\nectra\models.toml`, after a backup and with a diff.
  5. It writes issues for everything else.
  6. It calls `tools/laya/retrain`, or records `retrain.needed` in `state.json`.

  It never merges, never pushes and never switches your checkout.
- **Model discovery.** Each provider has its own source:
  - Codex: `codex debug models`, which is the account's own catalog.
  - Claude: the aliases under `--model` in `claude --help`.
  - Gemini: the `*_MODEL` constants in the CLI bundle.
  - Ollama: `/api/tags`.

  Claude and Gemini lists don't prove account access, so each new id gets one tiny probe turn. A model is probed only when the provider's cheapest model answered in the same run, and the result is cached.
- **Safety rules:**
  - Discovered models are added **disabled**, with `prefers = []` and scores copied from the closest model. A comment on each says to review it, then enable it.
  - Models retired from the Codex catalog are disabled.
  - Flags matching `dangerous|bypass|skip` are never renamed automatically, so `--skip-git-repo-check` is never added.
  - A rename counts as mechanical only when Nectra passes the old flag as a literal.
- **Scheduling:**
  - `scripts/compat/schedule.ps1` now runs adapt.py. It is already registered on this PC, and its logs exist for 10-17 to 10-19.
  - New `scripts/compat/nectra-compat.{service,timer}` are systemd user units.
  - Both are documented in `docs/compat.md`.
- **Spec:** `docs/superpowers/specs/2026-10-20-cli-auto-adapt.md`. It includes the cross-product contract for `tools/laya/retrain`: no arguments, the reason in `NECTRA_RETRAIN_REASON`, the fingerprint in `NECTRA_RETRAIN_FINGERPRINT`, and exit 0 clears the pending flag.
- **Test fix:** `tests/setup.rs` assumed every bundled model of a selected provider is enabled. Disabled discovered entries broke that assumption, so the assertion is now "unselected providers off, selected provider has an enabled model".

## Real run on this PC (live, 2026-10-20)

`python scripts/compat/adapt.py --live --nectra target/release/nectra.exe --providers anthropic,openai,google`

| | Observed |
|---|---|
| Claude Code | 2.1.304. The 2026-10-19 scheduled log had 2.1.303. |
| Codex CLI | 0.162.1 |
| Gemini CLI | 0.64.0, **not signed in** on this PC |
| Ollama | 0.20.2. `llama3.1:8b` is pulled but not configured in `[ollama]`. |
| Claude aliases | fable, opus, sonnet. **`fable` is new**; the probe with one tiny turn answered. |
| Codex catalog | **New:** gpt-6.1-sol, gpt-6-sol, gpt-6-luna, gpt-5.6-terra. Hidden: gpt-reserve, codex-auto-review. None of the registry's models are retired. |
| Gemini bundle | 6 ids not in the registry: gemini-3-flash-preview, -3-pro-preview, -3.1-flash-lite, -3.5-flash, -3.5-flash-lite, -3.8-flash. Not verified, because the CLI is signed out. |
| check.py | 11 passed, 1 failed (Gemini stream view: no auth method set) |

Results:
- **Branch `compat/auto-532be260`** (local, not pushed) adds the 5 new models to `config/models.toml`, disabled. **Tests passed** (`cargo test --all-targets -- --test-threads=1`).
  - The first attempt was based on `master` and failed `tests/setup.rs`, which led to the test fix above.
  - The kept branch is based on this branch, so it merges cleanly only after this branch does.
- **Installed registry updated:** 5 disabled entries were appended. The backup is `%APPDATA%\nectra\models.toml.bak-2026-10-20`, and the full diff is in `%LOCALAPPDATA%\nectra\compat\report-2026-10-20.md`. Routing is unchanged, because every new entry is disabled.
- **Laya:** the fingerprint `0a3b39ec6523f5fe` is recorded. Nothing enabled changed, so no retrain.
- **Issues raised:** Gemini signed out (stream and check.py). Correct.
- **Cost:** 3 raw turns (haiku, gpt-5.6-luna, gemini-flash, which failed on auth), 1 probe (fable) and check.py's 4 live turns, each with the prompt "6 times 7". The rerun made no model calls, because the probe was cached.

## Simulated breaking release, end to end

The fixture `scripts/compat/fixtures/breaking-release.json` simulates this release:
- Codex renames `--ephemeral` to `--ephemeral-session`.
- Claude drops `--dangerously-skip-permissions`.
- Claude's `result` event loses `result`.
- `gpt-5.6-luna` leaves the Codex catalog.
- `gpt-7-nova` and `fable` are new.
- Both CLIs bump versions.

Running it through `main()` against the **real repo with real `cargo test`** (scratch state, installed registry untouched) gave:

- **Mechanical:** the rename, `fable` and `gpt-7-nova` added disabled, and luna disabled.
  - Branch `compat/auto-141188f6` touched `src/executor/conversation.rs`, `scripts/compat/check.py` and `config/models.toml`.
- **Issues:** the safety flag is gone ("it controls a provider safety check", so it was not renamed), and the Claude `result` event lost `['result']`.
- **Branch tests FAILED,** and the run exited 1: `tests/chat.rs::mode_best_escapes_a_sticky_cheap_model` pins `gpt-5.6-luna`. That is the loop working as intended: a retirement that breaks behaviour is flagged for a person instead of passing as safe.
- I deleted the drill branch afterwards, because its fake rename would break real Codex.

The same fixture runs in the unit tests against a throwaway repo. There they also check:
- `master` and the checkout stay untouched.
- The installed registry is edited, backed up and valid.
- A second run creates no duplicate branch or entries.

## Retrain trigger: both paths tested (`scripts/compat/test_adapt.py`)

- **Tool absent:** luna has the `simple-coding` role, so disabling it changes the fingerprint. The state gets `retrain.needed = true` with luna in the reason, and the report says "tools/laya/retrain is absent".
- **Tool appears later:** the next run calls `tools/laya/retrain.py`, the tool receives the reason in `NECTRA_RETRAIN_REASON`, and `needed` is cleared. A third run with nothing changed does not call it again.
- **Tool fails** (exit 3): it stays pending.

## Test evidence

- `python scripts/compat/test_adapt.py`: 12 tests, OK.
- The cargo gate is in the next section.

- `cargo fmt --all --check`: clean.
- `cargo clippy --all-targets --locked -- -D warnings`: clean.
- `cargo test --all-targets --locked -- --test-threads=1`: **175 passed, 0 failed, 1 ignored**, across 11 suites (lib 88, chat 30, native_terminal 6, …).

## Open issues

- **Gemini is signed out on this PC**, so the 6 Gemini candidates stay unverified. Running `gemini` once interactively to sign in fixes it. Google isn't in Michael's selected providers anyway.
- **The scores for discovered models are copies:** `fable` copied from haiku and `gpt-5.6-terra` from gpt-6-astra (closest-name matches). Michael should set the real scores and roles before enabling them. Enabling one with a role changes the fingerprint, which triggers a Laya retrain.
- **`tests/native_terminal.rs` (4 ConPTY tests) flakes when run in parallel.** It fails under `cargo test --all-targets` on this desktop before any change of mine, and passes alone or with `--test-threads=1`. Fix branches therefore test with one thread.
- **There is no Linux host for the timer.** The units are written and documented but not installed: lumina is off-limits, per Michael's correction.
- **The `tools/laya/retrain` contract** (no arguments, environment variables, exit code) needs the Laya worker to match it.
- **The Stream-format check covers only the fields Nectra reads** today (`conversation.rs`). Hook payload changes are caught only end to end, by check.py's terminal-view turns.
