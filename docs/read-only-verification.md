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

## Claude and remaining coverage

Claude's restricted Read/Glob/Grep profile is still a proposed tool restriction,
not an experimentally verified whole-process write boundary. No Claude model
call was made here. Hook, MCP, managed-settings and command-tool exclusion remain
to be tested separately; a model merely declining a write is not proof of denial.
The Codex fixture also does not verify those integration paths or network effects.

Next engineering step: Claude independently reproduces this failing capability
check. Choose an externally enforced read-only mount/container or another verified
backend before accepting a no-mutations capability contract. Debate production
implementation remains on hold; this prerequisite investigation does not change
that decision.
