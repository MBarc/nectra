# Debate isolation

Debate uses a separate container executor. It never falls back to the normal
writable provider backend when setup, verification or a provider fails.

## Setup

This implementation is experimental: offline tests pass, but the real container
image and provider authentication still require validation. On this development
machine Docker Desktop's Linux engine returns HTTP 500, so no real isolation
success is claimed yet. Nectra stops before provider calls if preflight fails.

Run a local Docker engine using Linux containers, then build the dedicated image:

```powershell
docker build -t nectra-debate:local tools/debate-container
nectra debate --check-isolation
```

Then run `nectra debate "Review the design tradeoffs in this repository"`, or
enable `/debate on` inside `nectra chat`. `/debate off` restores ordinary routing.
Model and provider pins remain constraints; they can produce a labelled single
isolated answer. Two participants normally use five logical calls across parallel
drafts, parallel exchanges, and one synthesis. Agreement is not verification.
Provider tools can generate additional API requests and cost.

Each participant's exchange dissent is displayed unchanged alongside the final
answer. A missing or malformed dissent is disclosed and the result is labelled
incomplete. Ctrl+C aborts the turn instead of returning a survivor answer.
Each phase has a typed record in a separate `*.debate-*.jsonl` audit beside the
normal session. Only completed rendered answers, including concerns and notices,
enter resumable conversation history. Keeping the audit separate preserves
compatibility with existing session readers. Audit files contain prompts and
provider output; treat them like the session transcript.

The image contains pinned Claude Code and Codex CLI versions. This is an optional
debate dependency; ordinary Nectra routing does not require Docker. Builds download
the Linux base image, packages and provider CLIs. Authentication is not included
in the image or build context.

`NECTRA_DEBATE_IMAGE` selects an already-built image (default
`nectra-debate:local`); `NECTRA_DEBATE_DOCKER` can select the native Docker CLI.
Use a trusted image. Preflight resolves its immutable image ID, so changing a tag
after verification does not change the image used for the turn. Remote Docker
endpoints and `DOCKER_HOST` overrides are rejected.

For authentication, set `ANTHROPIC_API_KEY` or `OPENAI_API_KEY`, or use an existing
local login credential file (`~/.claude/.credentials.json`, or
`$CODEX_HOME/auth.json`, defaulting to `~/.codex/auth.json`). Only the selected
provider credential file is mounted read-only and copied into temporary HOME.
Host settings, plugins, MCP definitions and machine-managed policy are not copied.
Token refreshes in the container are temporary; refresh the local login if needed.
Credential helpers/keychain-only logins are not supported by this adapter.

## Boundary and verification

The repository is mounted at `/workspace` read-only, excluding nested mounts.
Containers run as an unprivileged user with all capabilities dropped,
no-new-privileges, a read-only root filesystem, resource limits and temporary
HOME and `/tmp`. No Docker socket, host home directory, or writable host mount is
provided. Provider startup happens outside the repository, using fixed arguments
instead of host `extra_args` or permission-bypass settings.

Before provider calls, a disposable probe must successfully read a mounted file
and fail to create, overwrite, delete, rename or create a directory there. The
same operations must succeed in temporary scratch storage. Nectra then verifies
the original fixture files and directory entries are unchanged. A startup error
is a failed preflight, not evidence of isolation. This is exercised on each
preflight, not inferred from the presence of `--sandbox read-only`.

Containers retain ordinary bridge networking to reach provider APIs. This design
protects local repository filesystem mutations; it is not a promise of no network
side effects or protection against a compromised Docker daemon/kernel. Do not
expose unauthenticated host control services to containers. Symlinked host paths
outside the mounted repository are not separately mounted. Nested mounts are
omitted rather than risk leaving them writable.

Each call has its own container and output buffer. A single turn-level interrupt
token can cancel both parallel calls. Calls have a five-minute execution deadline;
container control operations and cleanup are bounded separately. Nectra explicitly
removes the named container on success, stream errors and cancellation, because
killing the local Docker client alone would not stop the container. Cleanup
failure is reported. If the Docker daemon becomes unreachable, inspect/remove
remaining `nectra-debate-*` containers after restoring it.

Initial supported container adapters: Claude and Codex. Gemini and Ollama are
excluded from this executor. A local-only routing policy is preserved; it cannot
be silently relaxed to produce two cloud participants.

## Development validation

`cargo test --test isolated_runtime --locked` uses a fake Docker transport to check
preflight failure, normal completion, malformed output, cancellation and removal.
It makes no model calls and does not prove the Docker filesystem boundary. The
real preflight must also pass against the built image on each supported platform.
