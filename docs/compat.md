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
| A flag Nectra passes is gone, and exactly one similar flag is new | Treats it as a **rename**. On a branch, it replaces every occurrence in `src/`, `tests/` and check.py, shared code such as the permissions filter included. |
| A flag is gone without a clear replacement, either name is a safety flag (`dangerous`, `bypass`, `skip`), or another CLI also uses the old flag | Writes an **issue**. Safety flags are never renamed for you, and a flag two CLIs share can't be renamed for one of them. |
| A stream-json event Nectra reads lost a field, or never arrives | Writes an **issue**. Parser changes are never mechanical. |
| A model the account can use is not in the registry | Adds it with **`enabled = false`** and `prefers = []`, scores copied from the closest model. Review the scores, then enable it. |
| A registry model is gone from the Codex account catalog | Sets it to `enabled = false`, unless that would leave OpenAI with no enabled model in the bundled or the installed registry. Then it is an issue. |
| A Gemini registry model the CLI no longer mentions | Writes an issue. The CLI bundle is not authoritative. |
| Ollama models pulled but not configured, or the reverse | Records it in the report as a note. `[ollama]` lives in your config. |
| The enabled models, their roles, `ROLES` or `TaskType` changed | Calls `tools/laya/retrain`, or records "retrain needed". |

Mechanical fixes go on a **local branch `compat/auto-<hash>`**, made from `master` in a
temporary worktree. The run tests it there (`cargo test --all-targets -- --test-threads=1`),
then commits and removes the worktree. Your checkout is never switched. Nothing is
merged or pushed. The hash covers the fixes, the base commit and the test command, so
an earlier branch's result is reused only for the same change tested the same way.
A branch that never got its commit is deleted. A worktree that cannot be removed, a
branch that cannot be deleted and a failed `git worktree prune` are named in the
report, and the run exits 1.

The run also updates the installed registry (`%APPDATA%\nectra\models.toml`, or
`~/.config/nectra/models.toml`), which shadows the bundled one:
- Only a change whose **exact text passed the tests** is written. That is the fix
  branch's registry when it is identical, compared as exact text with nothing stripped
  (line endings are normalised to `\n` when the run reads the file, and written back
  that way). Otherwise the run tests the new text on
  its own, on the base, in another temporary worktree. On this PC the installed copy
  differs from the bundled one, so expect that second test run.
- The write is a compare-and-swap. The new text is first written in full to a
  temporary file beside the registry. Then the registry is moved to its own
  `models.toml.bak-<time>` (one per change, never overwritten) and the moved text
  compared with what the run read at its start. The new text is moved in only if no
  file has appeared meanwhile. If you edit the registry at any point before that last
  move, your edit stays and the run reports a conflict. An editor that keeps the file
  open and writes after the move writes into the backup instead.
- Between the two moves the registry file is absent, normally for milliseconds, and
  Nectra cannot load it then. If the last move fails, the original is moved back. If
  the run is killed in between, `models.toml.swap` names the backup, and the next run
  moves it back before doing anything else and says so in its report. Until that next
  run, the registry stays missing.
- A lock beside it (`models.toml.lock`) keeps apart runs that use different state
  directories.

The report includes the diff. Because new models arrive disabled, routing only
changes when a model is retired.

## Where models come from

| Provider | Source | Proves account access? |
|---|---|---|
| Codex | `codex debug models`, refreshed for the signed-in account | Yes |
| Claude | aliases named under `--model` in `claude --help` | No. With `--live`, each new alias gets one tiny turn first. |
| Gemini | `*_MODEL` constants in the installed CLI bundle | No. With `--live`, each new id gets one tiny turn first. |
| Ollama | `GET /api/tags` on `OLLAMA_HOST` | Yes, these are local models |

A model is probed only if the provider's cheapest model answered in the same run, so a
signed-out CLI never marks models unavailable. A failed probe is retried after a week.
At most `--max-probes` (default 3) new models are probed per run. The rest wait for
the next run, so a CLI release that names many new models stays cheap.

Discovery has to succeed before it can change anything:
- A Codex catalog counts only if `codex debug models` exited 0 and listed at least
  one model, and every entry has a string `slug` and, if present, a string
  `visibility`. Each entry is checked as listed, and a slug listed twice rejects the
  whole catalog.
- A catalog that would retire every enabled OpenAI model, in either registry, is not
  trusted.
- A Claude help text with no aliases, a missing Gemini bundle, or an unreachable
  Ollama with configured models each become a `discovery` issue. None of them can
  retire anything.

The fix branch's tests time out after `--test-timeout` (default 1 hour), and then
count as failed.

Every command the run starts is bounded the same way: CLI calls (120 s; 300 s for a model turn, 60 s for the
Codex catalog), tests, the retrain tool, git (600 s) and the `check.py` run (1 h). On
Windows each runs in its own job object, which keeps hold of children even after
their parent exits. On a timeout, and when the command ends, the job is killed, so
nothing it started keeps running. If Windows refuses to create or assign the job, the
command is killed before it starts and counts as failed to start; it is never run
uncontained. On Linux the same is done with a session; a child that starts its own
session escapes it. Output goes to temporary files rather than pipes, so a leftover
child cannot stall the run. Ollama discovery is an HTTP request with a 3 s timeout.

## State and reports

`%LOCALAPPDATA%\nectra\compat` on Windows, `~/.local/state/nectra/compat` on Linux:

- `state.json`: last versions, help flags, probe results, the Laya fingerprint and
  `retrain: {needed, reason, since}`.
- `report-<time>.md` (one per run, never overwritten; `report-latest.md` is a copy of the
  newest): findings, branch and test result, registry diff, issues and
  retrain status.
- `target/`: the build directory for fix branches, so rebuilds are incremental.

Exit codes:
- `0`: nothing needs you.
- `1`: something needs a person. That is an issue, failing tests, a registry change
  that was blocked or hit a conflict, or a cleanup that failed.
- `2`: another run holds the state directory's `adapt.lock` or the registry's
  `models.toml.lock`. Nothing was done. The scheduled task shows no notification for
  this.

## Laya retraining

`tools/laya/retrain` is optional and belongs to the Laya side; a `.py`, `.ps1`, `.sh`
or `.cmd` suffix also works. It runs from the repo root with no arguments.
`NECTRA_RETRAIN_REASON` and `NECTRA_RETRAIN_FINGERPRINT` are set in its environment.
Exit 0 means the retrain is done or queued. Any of these leaves `retrain.needed = true`,
and the next run tries again:
- a missing tool;
- a tool that cannot start;
- any other exit code;
- a run longer than `--retrain-timeout`. The default is 12 hours, because training is
  slow.

Processes the tool leaves behind are killed when it exits, as for every command
above. A tool that only queues a job must hand it to something outside the run,
such as a scheduled task or a systemd unit, and return.

The pending flag, with its reason and fingerprint, is saved to `state.json` before
the registry changes and before the tool starts, so an interrupted run never loses
it. If the registry write is then refused because the file changed, the flag goes
back to what it was. The scheduled task and the systemd unit allow 14 hours: room
for the fix-branch tests plus a full retrain.

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
