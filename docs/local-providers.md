# Ollama configuration

Ollama is implemented as a direct HTTP chat connection. It does not require an
Ollama CLI on the computer running Nectra, but needs a reachable server with
installed local models. Nectra does not download models.

Run `nectra provider add ollama` for the wizard, or use this unattended form,
replacing `MODEL:TAG` with an installed model's full name:

```sh
nectra ollama --url http://localhost:11434 --models MODEL:TAG --policy local-only
```

Setup checks /api/tags, verifies selected models through /api/show, and saves
only after successful checks. It discovers context capacity and initially uses
at most 8192 tokens per model. The same command changes the address, models,
or policy later. Other provider selections and model files are preserved.

| Policy | Behavior |
| --- | --- |
| local-only | Excludes cloud CLI providers, including explicit cloud pins |
| prefer-local | Uses local models first, then eligible selected cloud providers |
| fallback-only | Uses cloud models first, then local models when cloud choices are exhausted |
| mixed | Ranks local and cloud profiles together |

Local-only is the default. Removing Ollama from provider selections deactivates
its routing policy. Model IDs appear as `ollama/MODEL:TAG` in /models; use
`/provider ollama` or `/model ollama/MODEL:TAG` to pin a choice.

Nectra sends completed conversation turns as user/assistant messages to /api/chat
and streams the reply. It reserves 1024 output tokens and budgets serialized input
conservatively in bytes, not exact tokenizer counts. Long history is trimmed with
an omission notice while the full saved thread remains intact. Oversized current
messages are rejected. Edit context_tokens in the config's [[ollama.models]] entries
to adjust capacity, within the model's reported maximum.

The [ollama] config supports base_url, policy, timeout_seconds (default 180,
range 1..3600), optional token_env, and models. Metadata requests time out after
five seconds; connection establishment after three seconds. Generation has a total
request deadline including model loading. Protected remote servers can use token_env
naming a bearer-token environment variable; remote token use requires HTTPS.
No token is saved in TOML. To bootstrap a protected server, provide the Ollama
section in your config, then run `nectra setup --providers ollama`. The wizard
handles unauthenticated servers. System proxies and redirects are disabled.

Cloud-backed Ollama models are rejected; local metadata is rechecked before
generation. The configured server remains a trusted endpoint: Nectra cannot prove
how a separately administered server implements inference or handles data.
Busy/unreachable responses enter a 30-second cooldown and may fall back before
output, within the chosen policy. Partial replies are not replayed or committed
as completed turns. An exhausted session stays open.

This connection is chat-only: it does not execute tools, inspect files, or edit
the workspace. It can discuss and generate code but cannot perform file and shell
actions supplied by the native agent CLIs.

Validated with mock HTTP servers and synthetic streams. No live Ollama installation
or real model generation was available for validation.

References: [Ollama API](https://docs.ollama.com/api/introduction),
[model discovery](https://docs.ollama.com/api/tags),
[chat](https://docs.ollama.com/api/chat), and
[official schema](https://github.com/ollama/ollama/blob/main/docs/openapi.yaml).
