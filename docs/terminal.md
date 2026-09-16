# Live provider terminal

Run `nectra` normally in a terminal. While the selected provider runs in a real
child terminal, the default view shows compact activity and Nectra's message queue.
The temporary activity area clears before the completed answer is printed once.
Provider branding, empty prompts and housekeeping stay out of the normal view.

Press F2 to inspect the full native terminal, including tool output, diffs and
approval prompts, and interact with it. Press F2 again to resume Nectra queuing.
Native progress is recognized where possible; otherwise an elapsed activity
indicator remains visible. Stop-hook processing is labeled "Finishing reply".

## Controls

- Type and press Enter to queue a message in Nectra while the provider works.
- Use Left/Right, Home/End, Backspace and Delete to edit the queued draft. Movement
  and deletion keep Unicode grapheme clusters (including emoji) intact. Long drafts
  scroll horizontally so the insertion cursor stays visible.
- Press **F2** to give the provider keyboard control for an approval or other native
  interaction. Press F2 again to return to the main-thread queue.
- Completed replies automatically return to Nectra's main prompt.
- Press **Shift+PageUp / Shift+PageDown** to scroll the pane's recent output.
- Resize the terminal to resize the provider pane.
- In provider-input mode (F2), **Ctrl+C** is forwarded to the provider for its normal
  interrupt behavior. In the main queue, use Ctrl+] twice to cancel the active turn.
- If work has not completed, the first Ctrl+] offers cancellation. Press it again
  to stop and return, or Escape to stay. Interrupted work is not automatically replayed.
- The provider's `/quit` or `/exit` command can also return to Nectra. A completed
  captured answer is retained on a normal exit.

While work is active, ordinary input goes to Nectra's queue. Queued messages run in
submission order after the current reply, each through the router with updated context.
The footer shows your draft and the number queued during this turn. If a reply finishes
while you are drafting, finish and press Enter (or erase the draft) to return.
F2 explicitly switches to provider input; its footer identifies that mode.
After answer capture and draft submission, the pane closes automatically and the next message goes through
Nectra's router at the `you>` prompt. To select Opus, enter `/model opus` there. Nectra's
`/model`, `/provider`, `/mode`, `/theme`, and `/history` commands run at that prompt.

The pane retains up to 10,000 scrollback rows while active. Final captured replies
are printed into the main thread and saved in the shared session. Screen redraws
and raw terminal control sequences are not added to model context. This is a text
terminal emulator; graphics protocols and the provider's mouse UI are not supported.

## Choosing a view

```powershell
nectra --provider-view auto
nectra --provider-view terminal
nectra --provider-view stream
```

`auto` is the default: use the live terminal when stdin and stdout are terminals;
otherwise use structured streams. `terminal` requires an interactive terminal at
least 20 columns by 8 rows. `stream` retains the previous inline answer display and
Nectra's status spinner. Ollama always uses its HTTP stream; it has no native CLI
subterminal to embed.

During CLI-backed chat in stream view, **Ctrl+C** cancels the current turn and
returns to the chat prompt. Partial output is neither saved as a completed reply
nor automatically replayed. This control does not yet cover Ollama HTTP requests
or one-shot `run` commands.

CLI chat cleanup terminates the launched Windows job or Unix process group before
waiting for its pipes. After a provider exits, inherited pipes receive a short
drain period; shutdown and pipe joins are bounded. A cancelled or malformed turn
cannot hang the chat indefinitely waiting for a child that keeps a pipe open.
Native panes use the same scoped cleanup. Shared daemons and processes that
deliberately escape the launched group are not terminated. Linux/macOS process-group
behavior requires platform CI validation; local regression checks run on Windows.

Shrinking a native window below 20 columns by 8 rows temporarily pauses display
and typing while the provider continues running. Enlarge the window to resume;
Ctrl+] twice can still cancel while it is small.

The permission-bypass flag is still per launch and applies to the chosen provider:

```powershell
nectra --dangerously-skip-permissions
```

Without that flag, approvals belong to the provider and are handled in its pane.
Nectra's answer-capture hooks do not approve tools or change provider security settings.

## Shared context and compatibility

Claude uses a per-launch `UserPromptSubmit` hook to receive the shared transcript
and a `Stop` hook to return its final answer. Gemini uses `BeforeAgent` and
`AfterAgent`. Codex receives a short reference to a temporary UTF-8 context file
and reports its final answer through its notification interface. Reading that file
uses Codex's normal tools and permissions. Large conversation text is not placed in
the initial process command line.

Claude explicit `--settings` and Gemini system defaults are copied into temporary
settings before Nectra appends its hooks. Original settings files are not rewritten.
Existing Codex notifications are forwarded after capturing the answer. Temporary
context and hook files are removed when the provider pane closes. Provider CLIs may
also keep their normal native session history according to their own settings.

Use current CLIs with these hook/notification interfaces enabled. Managed policies,
disabled hooks, or incompatible CLI versions can prevent answer capture. Nectra
then reports that no completed answer was captured rather than saving screen text
as a fabricated answer; use `--provider-view stream` for that environment.

Native terminals do not expose the same reliable pre-work failure boundary as the
structured event mode. Nectra therefore does not blindly replay failed native
terminal work. A structured model-access rejection can fall back when hooks observed
the initial prompt and no tool work. Otherwise, denied models are excluded for future
messages and the uncertain turn is not replayed. Recognized Claude failure hooks can mark a provider for login or
cooldown on subsequent messages. Other native errors remain visible for manual
recovery. Use stream view when automatic pre-work provider failover is the priority.

## Development verification

Debug builds include a local fixture that makes no model calls:

```powershell
.\target\debug\nectra.exe __terminal-demo
```

It animates, asks for `y`, captures a fake completed answer, and tests return to
Nectra. Automated tests also nest the entire pane inside a real PTY, resize it,
cancel work, exit normally, and verify that its Windows child processes terminate.
Debug fixture commands are absent from release builds.

Windows uses ConPTY with an independent cursor and standard VT input. The vendored
portable-pty patch and its rationale are in `vendor/portable-pty/NECTRA-PATCH.md`.
Its license is included in [portable-pty-LICENSE.md](portable-pty-LICENSE.md).

Leading model requests such as `use opus and plan a shed`, `please use sonnet: review
this`, or `ask opus ...` select that configured model for the current turn only.
They override a saved chat pin for that turn; `/model opus` remains the command
for a persistent pin. Unavailable requested models trigger an explained fallback
when safe to retry. Ordinary mentions and quoted text do not
act as model requests. These directives apply at Nectra's main chat prompt.

Claude and Gemini display the actual current user message as their initial prompt;
shared history and handoff instructions are supplied through hooks. Long messages
show a 2,000-character preview while the hook supplies the full text. Codex still
uses its context-file launch instruction because its adapter uses a different
handoff mechanism.
