# Read-only capability verification

## Result: Windows Codex profile does not pass

On 2026-09-15, the installed native Codex CLI 0.154.0 produced the following
result on two consecutive runs of `scripts/test_readonly_profiles.py`:

| Operation | Unrestricted control | `:read-only` sandbox |
|---|---|---|
| Read existing contents | succeeds | succeeds |
| Create a file | succeeds | permission denied |
| Overwrite existing contents | succeeds | permission denied |
| Delete an existing file | succeeds | **succeeds** |
| Rename an existing file | succeeds | permission denied |
| Create a directory | succeeds | permission denied |

The fixture exits 1 and its final directory comparison detects the deletion.
This is a failing capability check, not a passing test. It rules out promising
no repository mutations with this tested setup. It does not establish behavior
on every Codex version, operating system, or Windows sandbox backend.

## Reproduce

```powershell
python scripts/test_readonly_profiles.py --codex C:\path\to\native\codex.exe
```

The script creates uniquely named disposable directories under `.collab`, verifies
their resolved parent before cleanup, and never points mutation probes at project
files. It invokes no model. It reuses installed sandbox setup and selects the
built-in `:read-only` permission profile explicitly. Native Windows CLI 0.154.0
uses `codex sandbox --permission-profile :read-only -- COMMAND`, without a
`windows` subcommand. Inspect local help when testing other versions.

All five mutations must work in the unrestricted control. The sandbox must then
successfully read, explicitly deny all five mutations, and preserve a full tree
snapshot. A launch error or failed read cannot produce a pass.

Python's `TemporaryDirectory` initially caused a misleading read failure because
its private Windows ACL excluded sandbox access. The final fixture uses ordinary
inherited workspace permissions, without modifying any project ACLs.

## Claude: restricted tool-profile smoke test passes

An actual call through Claude Code 2.1.273 completed successfully with:

```text
--restricted --safe-mode --tools Read,Glob,Grep --strict-mcp-config
--permission-prompts none --permission-mode manual --no-session-persistence
--output-format stream-json --verbose
```

Run the opt-in live fixture (uses provider quota):

```powershell
python scripts/test_claude_readonly.py --claude C:\path\to\claude.exe --live
```

Observed initialization listed exactly `Glob`, `Grep`, `Read`, and no MCP servers.
The model called Read and Glob, reported a random file value that was not included
in its prompt, and left the complete fixture tree unchanged after being asked to
create, overwrite, delete, rename and create a directory. Exit status was zero and
the structured result indicated success. Syntax compilation also passed.

This is **observed tool restriction, not OS-level write denial**. Mutation tools
were unavailable; the test did not submit forbidden tool calls directly to the
dispatcher. A refusal alone would not pass: the fixture also checks inventory,
actual read tool use, secret fixture value, completion and the filesystem snapshot.
No raw conversation or credentials are persisted by the fixture.

Hook and managed-settings variations, deliberately configured malicious MCP
servers, alternate CLI versions and platforms remain untested. The Codex probe
also does not verify integration paths or network effects. Do not turn this one
smoke test into a runtime-wide `read_only_capable = true` assertion.

Claude independently reproduced the failing Codex capability check. The next
decision is whether the product contract permits a controlled read-tool-only
adapter or requires external OS isolation for every CLI. A disposable clone alone
is insufficient: the process must also be denied access to the original checkout.
Debate production implementation remains on hold; these prerequisite experiments
do not implement or approve that feature.
