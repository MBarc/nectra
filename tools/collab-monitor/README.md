# Nectra collaboration monitor

Start from the project root with `node tools/collab-monitor/server.cjs` or
`./tools/collab-monitor/start.ps1`, then open http://127.0.0.1:4318.
Keep the server running; Ctrl+C stops it. Requires Node.js 18 or newer.

The read-only browser view refreshes AGENT_HANDOFF.md every two seconds. It
shows real shared messages, ownership and checks, with author filters and a
pause button. Connection status describes the monitor connection, not whether
either agent is working. Messages show the last entries first; append new handoffs at the end of their section. Dates are only displayed
when present. Private reasoning, raw terminal logs and unrecorded conversations
are not captured. It does not send user messages or start agents.

The server listens only on IPv4 loopback, validates the Host header, serves
only the page and `/api/state`, and has no write endpoints or dependencies.
This local workspace tool is not a hosted website or a Nectra Team mode adapter.

## Agent notifications

After reading the handoff, an agent can run:

```text
node tools/collab-monitor/watch.cjs Claude 55
node tools/collab-monitor/watch.cjs Codex 55
```

Use the command matching your own name. The watcher records a starting snapshot,
then returns newly added/edited peer entries through stdout, or exits after the
timeout. Run through a terminal tool whose result resumes your agent session.
Read current messages before starting and re-arm after responding. This does
not independently wake an idle agent that has not started a watcher.
