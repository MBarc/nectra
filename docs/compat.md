# Keeping up with the provider CLIs

Claude Code, Codex CLI, Gemini CLI and Ollama change without notice. Two scripts
keep Nectra in step with them:

- `scripts/compat/check.py` passes or fails Nectra against the installed CLIs.
- `scripts/compat/adapt.py` runs that check and adapts Nectra to what it finds. Run it
  on a schedule.

```sh
python scripts/compat/adapt.py --live --nectra target/release/nectra
```

## What one run does

| Finds | Then |
|---|---|
| A CLI changed version | Records it in the report as a note. |
| A flag Nectra passes is gone, and exactly one similar flag is new | Treats it as a **rename**. It replaces the literal in `src/**.rs` and in check.py's flag list on a branch. |
| A flag is gone without a clear replacement, or it is a safety flag (`dangerous`, `bypass`, `skip`) | Writes an **issue**. Safety flags are never renamed for you. |
| A stream-json event Nectra reads lost a field, or never arrives | Writes an **issue**. Parser changes are never mechanical. |
| A model the account can use is not in the registry | Adds it with **`enabled = false`** and `prefers = []`, scores copied from the closest model. Review the scores, then enable it. |
| A registry model is gone from the Codex account catalog | Sets it to `enabled = false`. |
| A Gemini registry model the CLI no longer mentions | Writes an issue. The CLI bundle is not authoritative. |
| Ollama models pulled but not configured, or the reverse | Records it in the report as a note. `[ollama]` lives in your config. |
| The enabled models, their roles, `ROLES` or `TaskType` changed | Calls `tools/laya/retrain`, or records "retrain needed". |

Mechanical fixes go on a **local branch `compat/auto-<hash>`**, made from `master` in a
temporary worktree. The run tests it there (`cargo test --all-targets -- --test-threads=1`),
then commits and removes the worktree. Your checkout is never switched. Nothing is
merged or pushed. The same set of fixes never makes a second branch.

The run also updates the installed registry (`%APPDATA%\nectra\models.toml`, or
`~/.config/nectra/models.toml`), which shadows the bundled one. Before writing, it
copies the file to `models.toml.bak-<date>`, and the report includes the diff.
Because new models arrive disabled, routing only changes when a model is retired.

## Where models come from

| Provider | Source | Proves account access? |
|---|---|---|
| Codex | `codex debug models`, refreshed for the signed-in account | Yes |
| Claude | aliases named under `--model` in `claude --help` | No. With `--live`, each new alias gets one tiny turn first. |
| Gemini | `*_MODEL` constants in the installed CLI bundle | No. With `--live`, each new id gets one tiny turn first. |
| Ollama | `GET /api/tags` on `OLLAMA_HOST` | Yes, these are local models |

A model is probed only if the provider's cheapest model answered in the same run, so a
signed-out CLI never marks models unavailable. A failed probe is retried after a week.

## State and reports

`%LOCALAPPDATA%\nectra\compat` on Windows, `~/.local/state/nectra/compat` on Linux:

- `state.json`: last versions, help flags, probe results, the Laya fingerprint and
  `retrain: {needed, reason, since}`.
- `report-<date>.md`: findings, branch and test result, registry diff, issues and
  retrain status.
- `target/`: the build directory for fix branches, so rebuilds are incremental.

The exit code is 1 when something needs a person: an issue, or failing tests on the
fix branch. Otherwise it is 0.

## Laya retraining

`tools/laya/retrain` is optional and belongs to the Laya side; a `.py`, `.ps1`, `.sh`
or `.cmd` suffix also works. It runs from the repo root with no arguments.
`NECTRA_RETRAIN_REASON` and `NECTRA_RETRAIN_FINGERPRINT` are set in its environment.
Exit 0 means the retrain is done or queued. Anything else, or a missing tool, leaves
`retrain.needed = true`, and the next run tries again.

## Schedule it

### Windows (Task Scheduler), this PC

```powershell
powershell -File scripts\compat\schedule.ps1            # daily 09:00, while signed in
powershell -File scripts\compat\schedule.ps1 -At 13:30
powershell -File scripts\compat\schedule.ps1 -Remove
```

Register it from the checkout that should receive the `compat/auto-*` branches. It
checks the providers your `nectra config` selects. When something needs you, a
notification names the issue and the report.

### Linux (systemd user timer)

On the Linux machine you use, the CLIs and cargo must be installed, and the CLIs
signed in as your user.

```sh
git clone <nectra> ~/nectra && cd ~/nectra && cargo build --release
mkdir -p ~/.config/systemd/user
cp scripts/compat/nectra-compat.{service,timer} ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now nectra-compat.timer
sudo loginctl enable-linger "$USER"     # run without an open session
systemctl --user start nectra-compat     # one run now
journalctl --user -u nectra-compat       # its output
```

The units assume the checkout is at `~/nectra`. Edit `WorkingDirectory` and `ExecStart`
if it is somewhere else.

## Try it without touching anything

```sh
python scripts/compat/test_adapt.py
```

The tests feed `scripts/compat/fixtures/breaking-release.json` through the whole loop
against a throwaway repository. In that simulated release, Codex renames `--ephemeral`,
Claude drops `--dangerously-skip-permissions`, Claude's `result` event loses its
`result` field, `gpt-5.6-luna` is retired and two models are new.
