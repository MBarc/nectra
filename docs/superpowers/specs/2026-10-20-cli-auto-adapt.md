# Keeping up with the provider CLIs automatically

Status: built on branch `MBarc/nectra-cli-autoadapt` (2026-10-20).

## Problem

Nectra drives four tools it does not control: Claude Code, Codex CLI, Gemini CLI and
Ollama. They change flags, stream formats and model line-ups without notice. The daily
`scripts/compat/check.py` (b19d5a5) only says pass or fail. Nobody adapts the registry,
and `config/models.toml` hard-codes Michael's model ids.

## What runs

`scripts/compat/adapt.py` runs once per scheduled tick:

1. **Observe** (`observe()`): CLI versions, the help text of every command whose
   flags Nectra passes, each provider's model list, and with `--live` one raw
   stream-json turn per CLI on its cheapest model (prompt: "6 times 7").
2. **Analyse** (`analyse()`, pure, driven by fixtures in tests) compares that against
   the registry and the previous run's state, and produces findings:
   - `version`: a CLI changed version (informational).
   - `flag`: a flag Nectra passes is gone. If the same run's help text shows exactly
     one new flag close to it (difflib >= 0.6) that was not there last run, it is a
     **rename**, which is mechanical. Otherwise it is an issue.
   - `stream`: an event Nectra reads (`src/executor/conversation.rs`) is missing a
     field, or its type disappeared. This is an issue, because a parser change is never
     mechanical.
   - `model-new` / `model-retired`: from discovery (below).
   - `ollama`: pulled models that are not configured, or configured models that are not
     pulled (informational, because `[ollama]` lives in the user's own config).
3. **Fix**: mechanical findings become edits on a local branch `compat/auto-<hash>`,
   created in a temporary `git worktree` from `master`. The test command
   (`cargo test`) runs there, the result is committed, and the worktree is removed.
   The branch is kept and is never merged or pushed. If a branch with the same
   hash already exists, the step is skipped.
4. **Installed registry**: the same registry edits go to the installed
   `%APPDATA%\nectra\models.toml` (Linux: `~/.config/nectra/models.toml`), which
   shadows the bundled one. Before writing, the run saves a backup to
   `models.toml.bak-<date>`, puts a unified diff in the report, and checks that the
   new text parses as a valid registry (unique ids, same structure).
5. **Retrain**: the Laya fingerprint covers the enabled models and their `prefers`,
   the `ROLES` table in `src/policy.rs` and `TaskType` in `src/classifier.rs`. If it
   changed since the last run, or a previous retrain is still pending, the run calls
   `tools/laya/retrain`. If that tool is absent or fails, the state file records
   `retrain: {needed: true, reason, since}`.
6. **Report**: `report-<date>.md` in the state directory lists the findings, branch,
   test result, registry diff, retrain status and the issue text for each
   non-mechanical finding.

The state directory is `%LOCALAPPDATA%\nectra\compat` (Linux: `~/.local/state/nectra/compat`).
It holds `state.json` (versions, help-flag sets, stream signatures, probe results,
the Laya fingerprint and retrain status) and the dated reports.

## Model discovery: what each source can prove

| Provider | Source | Account-specific? | New | Retired |
|---|---|---|---|---|
| Codex | `codex debug models` (refreshed from the server for the signed-in account) | yes | `visibility = "list"` slugs not in the registry | registry ids absent from the catalog: auto-disabled |
| Claude | aliases named in `claude --help` under `--model` | no, so with `--live` each new alias is probed once with a tiny prompt | aliases not in the registry | never automatic (aliases resolve server-side) |
| Gemini | `*_MODEL = "gemini-…"` constants in the installed CLI bundle | no, so each new id is probed once | constants not in the registry | registry ids the bundle no longer names: issue only |
| Ollama | `GET /api/tags` | yes (local) | report only | report only |

**Safety rule:** discovered models are added with `enabled = false` and `prefers = []`.
Their capability scores are copied from the closest existing model of the same
provider, and a comment names that source. They never route until Michael enables
them, so the installed registry can be updated unattended without changing routing.
Retired models are set to `enabled = false`, which stops routing to a model that now
fails. Neither change touches the Laya fingerprint unless it disables a model that
had a role. That case is a real change, and it triggers a retrain.

## Cross-product contract: `tools/laya/retrain`

Owned by the Laya worker. Nectra's adapt loop relies only on these points:
- It is a file at `tools/laya/retrain`, or the same name with `.py`, `.ps1`, `.sh` or
  `.cmd`. `.py` runs with the current Python and `.ps1` with PowerShell. Anything
  else is executed directly.
- It is run from the repo root with **no arguments**. The reason is in the
  environment variable `NECTRA_RETRAIN_REASON`, and the new fingerprint is in
  `NECTRA_RETRAIN_FINGERPRINT`.
- Exit code 0 means the retrain is done or queued, and clears `retrain.needed`. Any
  other exit code keeps it pending, and it is retried next run.

## Constraints kept

- No secrets: discovery uses the CLIs' own logins, and nothing is written into the
  repo except the generated branch.
- The Codex `--skip-git-repo-check` flag is never added. Live turns run in a
  trusted git work folder instead.
- Live calls use the cheapest model per provider and the "6 times 7" prompt. Each new
  model is probed once, and the result is cached in `state.json`.
- The loop never pushes or merges, and never checks out a branch in the working tree
  it was started from.
