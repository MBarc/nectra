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
  - Correction (Astra #3): this write came from the first run, whose fix branch had **failed** its tests. The code at the time did not gate installed changes on the branch. A rerun produced the same entries with tests passing, so the result stands. Today's code would have left the installed registry alone until then.
- **Laya:** the fingerprint `0a3b39ec6523f5fe` is recorded. Nothing enabled changed, so no retrain.
- **Issues raised:** Gemini signed out (stream and check.py). Correct.
- **Cost:** 3 raw turns (haiku, gpt-5.6-luna, gemini-flash, which failed on auth), 1 probe (fable) and check.py's 4 live turns, each with the prompt "6 times 7". The rerun made no model calls, but only because it ran without `--live`. A scheduled `--live` run always makes the baseline turns: 6 for Claude and Codex, or 8 with Gemini. On top of those come at most `--max-probes` (default 3) new-model probes. Only the probes are cached.

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
- **Branch tests FAILED,** and the run exited 1: `tests/chat.rs::mode_best_escapes_a_sticky_cheap_model` pins `gpt-5.6-luna`. The failure was flagged for a person. But at the time, the installed registry would still have taken the change if this drill had pointed at it. Since Astra #3, a failing branch blocks the installed update, and a test covers that.
- I deleted the drill branch afterwards, because its fake rename would break real Codex.

The same fixture runs in the unit tests against a throwaway repo. There they also check:
- `master` and the checkout stay untouched.
- The installed registry is edited, backed up and valid.
- A second run creates no duplicate branch or entries.

## Retrain trigger: both paths tested (`scripts/compat/test_adapt.py`)

- **Tool absent:** luna has the `simple-coding` role, so disabling it changes the fingerprint. The state gets `retrain.needed = true` with luna in the reason, and the report says "tools/laya/retrain is absent".
- **Tool appears later:** the next run calls `tools/laya/retrain.py`, the tool receives the reason in `NECTRA_RETRAIN_REASON`, and `needed` is cleared. A third run with nothing changed does not call it again.
- **Tool fails** (exit 3): it stays pending.
- **Since the Astra review,** it also stays pending when the tool can't start (`WinError 193`), when it hangs past `--retrain-timeout`, when a run is interrupted before the registry write, and when one is interrupted after the write but before the tool runs (the next run then retrains).

## Astra review

Astra (gpt-6-astra) reviewed the branch in `astra-compat-review-1.md`, and the coordinator accepted all ten findings. Each fix is its own commit, written test-first. The new tests are in the `AstraReview` class of `scripts/compat/test_adapt.py`.

Astra's re-review then found rows 1–5 and 8 only partly fixed. The text below describes what those commits did, not what the code guarantees now; see [Astra re-review](#astra-re-review) for that.

| # | Finding | Fix | Commit |
|---|---|---|---|
| 1 | Concurrent runs can overwrite each other or a user's edit | `adapt.lock` is held for the whole run with an OS lock (`msvcrt`/`flock`), which the OS releases if the process dies. A second run exits 2 with a clear message. Writes go through unique temp files (`mkstemp` + `os.replace`). The installed registry was compared with what the run read at the start, but the comparison and the write were separate steps, so a late edit could still be lost (re-review #1). | `7914b4f` |
| 2 | Failed or empty discovery can retire every model | `parse_codex_catalog` requires exit 0 and a non-empty list with string slugs (visibility was not checked until re-review #2). A catalog that would retire every enabled OpenAI model is not trusted. Claude, Gemini and Ollama failures become `discovery` issues. Failed discovery never adds or retires anything. | `f940d02` |
| 3 | Failing branch tests don't protect the installed registry | Tests failing, a branch or worktree error, or no recorded result block the installed change, and the report says why. A change with no branch, or with a branch that had nothing to change, still went through untested until re-review #3. | `947ed65` |
| 4 | Retraining can hang, or be lost on a failure or interruption | The pending flag, with reason and fingerprint, is saved before the registry changes and before the tool starts. The first-run baseline is saved before any change. The tool runs with `--retrain-timeout` (default 12 h). On Windows the timeout could still overrun while a child outlived its parent, until re-review #4. A launch error is caught, and every failure keeps the flag pending. The scheduler limits were raised to 14 h. | `3a49a64` |
| 5 | The rename safety check skips the new name, and renames are unscoped | Both names are checked against the safety list. A literal was renamed only where its nearest `"claude"`/`"codex"`/`"gemini"` label was the affected CLI. That heuristic missed shared code, and re-review #5 replaced it. | `0d02d47` |
| 6 | Probe commands don't follow a generated rename | `stream_cmd` moved into `check.py`, beside `FLAGS`, so the generated rename updates it. A test runs the branch's `check.py`. | `387464d` |
| 7 | Backups and reports are overwritten | Each change gets its own `models.toml.bak-<timestamp>`, and each run its own `report-<timestamp>.md`, both created exclusively. `report-latest.md` is a copy of the newest, and the toast points at it. | `094838c` |
| 8 | An existing branch loses its result; errors don't fail the run | An existing branch's result is read from its commit message. Worktree, commit and edit errors, or a missing result, set exit 1. A half-made branch is deleted, except on the `git worktree add` failure path, fixed in re-review #8. | `4fd2543` |
| 9 | `disable_model` silently misses valid TOML | The parser locates the model's table. Indentation, single quotes, trailing comments and `[[ model ]]` are all handled. The result is re-parsed and must differ in exactly that field; otherwise it raises, and the run reports it. | `0b03894` |
| 10 | `providers = []` turns into all providers; the fingerprint ignores selection | `choose_providers`: `""` or `[]` means none. The fingerprint follows `src/models.rs` (selected providers, Gemini only when selected, configured Ollama models when `ollama` is selected) and ignores role order. A version field re-baselines once, without a spurious retrain. | `2247e36` |
| + | The brief also asked for budgets | `--max-probes` (default 3) caps new-model probes per run. `--test-timeout` (default 1 h) bounds the fix branch's tests, and a timeout counts as failed. | `4d99df8` |

Not changed: Astra's low-priority notes on stream checks (fields only, not types) and on `tests/setup.rs` coverage. They are listed under Open issues.

## Astra re-review

Astra re-reviewed the fixes in `astra-compat-review-2.md` (verdict HOLD). It marked #6, #7, #9 and #10 fixed, the other six partly fixed, and found four new problems. The coordinator accepted all of them. Each fix is its own commit, written test-first. The tests are in the `AstraReReview` class.

| # | Astra found | What the code now guarantees | Commit |
|---|---|---|---|
| 1 | The registry comparison and the write were separate, so an edit in between was lost. Different state dirs took different locks. | The write is a compare-and-swap. The file is moved to its backup name and the moved text compared, then the new text moved in with a rename that fails if a file has appeared. An edit at any point before that last move is kept and reported. A lock beside the registry (`models.toml.lock`) is shared by runs with any state dir. **Limits:** an editor holding the file open and writing after the move writes into the backup. For a few milliseconds the registry file is absent. | `aa874b5` |
| 2 | `visibility: {}` was accepted. The all-retirement guard could be bypassed by combining the bundled and installed analyses. | Every catalog entry needs a string slug and, if present, a string visibility, or the whole catalog is rejected. Retirements that would leave a provider with no enabled model **in either registry** are all withheld, with a `discovery` issue. | `c09c57f` |
| 3 | No branch, or a branch with nothing to change, let an installed change through untested. | The installed change is written only when its **exact text** passed the tests. That is either the fix branch's registry, compared byte for byte, or the new text tested on its own at the base in a temporary worktree. | `cae1c24` |
| 4 | A child that outlived its launcher held the pipe; on Windows a 0.25 s timeout took 3.1 s. | Commands start suspended in their own Windows job object (a session on Linux). The job is killed on a timeout and when the command ends, so children that outlive their parent are killed too. Output goes to temporary files, not pipes. check.py's CLI calls use the same runner. Tested: a child holding the output no longer delays the return, and is dead afterwards. **Limit:** on Linux, a child that calls `setsid` itself escapes. | `87338d4` |
| 5 | The nearest-label heuristic left the shared permissions filter on `--sandbox`. | No ownership heuristic remains. A rename replaces every occurrence of the flag in `src/`, `tests/` and check.py, shared code included. It is proposed only for a flag that no other CLI is known to use (in `FLAGS`, this run's help or the last run's). Otherwise it is an issue. For Astra's `--sandbox` example, the plan covers the filter, the read-only launch, the resume handling and the tests. | `f30e500` |
| 8 | A `worktree add` failure returned before cleanup; cleanup failures were ignored. | Every path, that one included, runs the same cleanup: remove the worktree (falling back to deleting the folder), prune, and delete a branch that never got its commit. Whatever can't be removed is named in the report under "Cleanup failed", and the run exits 1. | `7202e5a` |
| new-1 | Retirement guard bypass across registries. | Fixed with #2. | `c09c57f` |
| new-2 | Cached "passed" results ignored `--base` and `--test-cmd`. | The branch hash covers the fixes, the base commit and the test command. An existing branch's result counts only if its commit sits directly on that base. | `739c337` |
| new-3 | A retirement of a model the installed registry leaves out blocked all its other changes. | Such a retirement is skipped for that registry. `disable_model` stays strict for a model that is present. | `c067943` |
| new-4 | A refused write still left its retrain pending. | The flag is still saved before the write. If the write is refused, the fingerprint and retrain state go back to what they were. **Limit:** a run killed between saving the flag and writing still retrains once, which is the safe direction. | `9ace50b` |

The docs had the same overstatements, now corrected in `docs/compat.md` and the spec. They now describe the compare-and-swap and its limits, the exact-text validation, how timeouts work, and the exit codes, including **2 when another run holds a lock**. With exit 2, the scheduled task now shows no notification. It used to show one pointing at a stale report.

Cost to know: on this PC the installed registry differs from the bundled one (the 5 disabled entries), so any change to it now runs the test suite a second time against that text.

Astra's third review (`astra-compat-review-3.md`, verdict HOLD) showed that parts of the table above still claimed too much: rows 1 ("a few milliseconds"), 2 ("every catalog entry"), 3 ("byte for byte"), 4 (containment when the job could not be made) and 8 (prune failures). See the next section for what the code guarantees now.

## Astra round 3

Astra marked #1, #5, #6, #7, #9, #10 and new-1 to new-4 fixed, #2, #3, #4 and #8 partly fixed, and found one new high-severity problem. The coordinator accepted all of them. Each fix is its own commit, written test-first. The tests are in the `AstraRound3` class.

| # | Astra found | What the code now guarantees | Commit |
|---|---|---|---|
| N1 | The original was moved aside before the replacement was written. A failed `mkstemp`, write or final rename, or a kill in between, left the registry missing for good. | The new text is written in full before the original moves, and a failure there leaves the registry untouched. If the final move fails, the original is moved back. A journal (`models.toml.swap`) names the backup while the registry is moved aside, and the next run, under the registry lock, moves it back first and says so in its report. **Limit:** after a kill, the registry stays missing until that next run, and Nectra can't load it meanwhile. | `185c598` |
| 2 | Validation ran after the dict was built, so a duplicate slug could hide a malformed entry. | Every entry is checked as listed, before the dict is built. A slug listed twice rejects the whole catalog. | `534cb76` |
| 3 | The branch comparison stripped whitespace. | The fix branch's `config/models.toml` is compared with the proposed text exactly, nothing stripped. Any difference, whitespace included, means the text is tested on its own. Line endings are normalised to `\n` when the run reads the installed file, and it writes `\n`. | `1ed8a78` |
| 4 | If Windows refused the job object, the command ran uncontained, and the timeout could miss its children. | If the job can't be created or assigned, the suspended process is killed before it runs, and the call fails with an error. CLI calls report it, tests count as failed, and the retrain stays pending. | `4154249` |
| 8 | A failed `git worktree prune` was discarded. | It is reported under "Cleanup failed", and the run exits 1. | `83923e0` |
| docs | Git and the `check.py` run used unbounded `subprocess.run`. | All git calls are bounded at 600 s and the `check.py` run at 1 h, through the same runner. No `subprocess.run` remains in adapt.py or check.py. | `4c4ec79` |

The docs (`docs/compat.md`, the spec) now say the same, including the registry being absent between the moves and what happens after a kill.

## Test evidence

Results after the Astra round 3 fixes (target/ rebuilt from scratch, then deleted again):
- `python scripts/compat/test_adapt.py`: **67 tests, OK.** That is 12 original, 29 for the first review (two rename tests rewritten for the new rule), 16 for the re-review and 10 for round 3.
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
- **Left from Astra's low-priority notes:**
  - Stream checks test field presence, not types or success status. `check.py --live` covers the end-to-end answer.
  - Claude alias parsing still depends on the `--model` help wording. A change there now shows up as a `discovery` issue, not as silent loss.
  - `tests/setup.rs` doesn't yet test a selected provider with both enabled and disabled entries.
- **Live runs since the review:** none against Michael's installed registry, as instructed. Every Astra fix, from both rounds, was exercised on fixtures and temporary copies. The only real CLI calls were `--version` smoke tests of the new process runner.
