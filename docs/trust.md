# What Nectra does on your machine

Nectra is a router. The work is done by the provider CLIs you installed and
signed in to: Claude Code, Codex and Gemini CLI, plus Ollama if you configure it.
This page lists what Nectra itself reads, writes and sends, and where its
limits are.

## Your accounts

- Nectra never reads, stores or sends your provider logins. Each CLI keeps its
  own, and `/login` opens that CLI's sign-in screen.
- It runs each CLI's own unmodified binary under your login, the same way you
  would run it.
- It never calls a provider's API with your subscription credentials.

## Your code

- **Edits.** A provider changes your files only as its own CLI would. Its approval
  prompts still appear in terminal view (press F2 to answer one).
  `--dangerously-skip-permissions` turns them off for one launch. Nectra never
  saves it, but it does pass on any `extra_args` you put in a provider's section
  of `config.toml`, so a bypass flag there applies to every launch.
- **Nothing is undone automatically.** Interrupting a turn, a failed turn and
  `/again` all leave changed files changed. Use git to review and revert.
- **Handoff notes are read-only, with one exception.** When a task moves to
  another model, the outgoing model writes the note with its tools restricted to
  reading. Codex's read-only sandbox on Windows still let a file be deleted when we
  tested it ([details](read-only-verification.md)).

## What leaves your machine

- **Your message goes to the provider that wins the route**, as it would without
  Nectra.
- **A TypeSafe key adds one destination.** Each message's text, plus the previous
  message, also goes to `api.typesafe.ai` so Jev can classify it. Without a key,
  or with Ollama set to `local-only`, routing happens on your machine.
- **Switching models sends the conversation to the new provider.** That is the
  text of your messages and the final answers, not tool output. It is either up to
  128 KiB of the conversation, or the handoff note plus up to 16 KiB. Staying on
  one model resumes its own session, and only the new message is sent.
- **MCP servers you set up in one CLI are offered to the others.**
  - The environment values those servers need, which are often tokens, reach the
    other CLI through environment variables, never through command-line
    arguments.
  - Launch-time sharing writes nothing to the other CLIs' configs.
  - `nectra mcp --sync` writes each CLI's own config file, as that CLI's
    `mcp add` would, including the values. It only runs when you ask.

## What Nectra reads

To count tokens, `nectra usage` and the cost line after each chat answer read
Claude Code's and Codex's local session logs (`~/.claude/projects`,
`~/.codex/sessions`). Nothing from them leaves your machine.

## What Nectra writes

| What | Where | When |
|---|---|---|
| Chat logs | `nectra/sessions` in the local data directory (`%LOCALAPPDATA%` on Windows) | Every chat |
| Cooldowns | `cooldowns.json` in the same data directory | When a provider reports a limit; entries expire on their own |
| Config and TypeSafe key | Beside `config.toml` ([where](../README.md#configuration)) | `nectra setup`; the key file is readable only by you |
| Per-launch hook settings | A temporary folder, deleted after the turn | Terminal-view turns |
| Skill links | `~/.claude/skills`, `~/.agents/skills` | `nectra skills --link` only |
| Other CLIs' MCP configs | `~/.claude.json`, `~/.codex/config.toml`, `~/.gemini/settings.json` | `nectra mcp --sync` only |

## Provider terms

Nectra runs each provider's own CLI under your login. It doesn't extract tokens or
resell access. Whether a pattern of use is allowed is still up to each provider's
terms, and they differ:

- **Anthropic.** The Claude Code
  [legal and compliance page](https://code.claude.com/docs/en/legal-and-compliance)
  allows "an end user ... signing in to the unmodified Claude Code binary with
  their own Claude subscription". It also says that advertised Pro and Max limits
  "assume ordinary, individual usage". The
  [consumer terms](https://www.anthropic.com/legal/consumer-terms) restrict access
  "through automated or non-human means, whether through a bot, script, or
  otherwise" unless Anthropic explicitly permits it.
  - Terminal view runs the interactive Claude Code you type into.
  - Stream view runs `claude -p`, and so does `nectra run` outside a terminal.
  - Handoff notes always use `claude -p` too, when a task moves off Claude in
    either view.
  - To avoid `claude -p` entirely, stay in terminal view and set
    `handoff_summary = false` under `[routing]`.
- **OpenAI.** Codex documents scripted use with `codex exec`
  ([non-interactive mode](https://learn.chatgpt.com/docs/non-interactive-mode)), and
  recommends API keys for automation.
- **Google.** Check the [Gemini CLI](https://github.com/google-gemini/gemini-cli)
  terms for your account type.

Nectra is for one person using their own subscriptions. Don't use it to share an
account or to serve other people from your plan.

## Known gaps

- **Gemini runs in stream view, not in its own terminal.** Gemini CLI 0.64 loads
  per-launch settings only from files an administrator owns, so Nectra can't add
  the hooks its terminal view needs.
- **Provider CLIs change without notice.** `scripts/compat/check.py` checks
  Nectra against the installed versions; `scripts/compat/schedule.ps1` runs it
  daily on Windows until the GitHub workflow can.
- **The macOS builds have not yet run on a real Mac.**
