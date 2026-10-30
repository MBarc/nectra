# Astra v1 follow-up triage

## M8 — ORT verification/load race

Validated by source inspection: the runtime was verified from the configured
directory and then loaded by pathname. This was real. The loader now copies the
verified native runtime into a private temporary staging directory and loads
that copy, so replacing the configured runtime after verification cannot change
the native library being loaded.

## M9 — Windows descendant cleanup

Validated by source inspection: the ordinary child path spawned first and only
then assigned the child to a Windows job. This was real. Ordinary provider
launches now use `CREATE_SUSPENDED`, assign the process to its kill-on-close job,
and resume it afterward. Native PTY launch remains dependent on the PTY API's
child-creation behavior and is covered by the existing Windows compatibility
runner; no live Windows race reproduction was available on the builder.

## M10 — agy repeated-step accounting

Validated with a regression fixture: two identical `DONE` updates previously
created two ledger calls. This was real. Accounting now deduplicates by
conversation id and `step_index`, with a unit test for repeated completion.
