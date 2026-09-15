# Shared options to consider next

Implemented common controls include provider/model selection, routing mode, saved
sessions, launch-scoped permission bypass, terminal color, and provider login.
Ollama additionally has server configuration, local-routing policy, model context
capacity, and a generation timeout. Other settings below are proposals, not flags
that currently exist.

| Proposed control | Meaning and implementation boundary |
| --- | --- |
| Request timeout | Common deadline and child/HTTP cancellation; distinguish model loading from inactivity |
| Retry limit | Bound failover attempts and cooldown policy; never replay a turn after tool activity |
| Context budget | Keep the user's latest message and budget history to each model's capacity |
| Output budget | Normalize only where the backend supports it; otherwise report unsupported |
| Working directory and allowed paths | Control where work occurs; native providers differ in sandbox semantics |
| Tool profile | Read-only, edit, or unrestricted; advertise supported behavior per backend |
| Reasoning effort | Per-provider mapping with explicit unsupported values; never silently pretend equivalence |
| Session cloud policy | Local-only versus explicitly selected cloud fallback, independently of speed/quality mode |

Avoid forwarding arbitrary shared flags to every CLI. Define a capability table
and fail clearly when a requested control is unsupported. Cancellation needs
particular care for descendant processes, and a unified retry count must include
provider-internal retries when they can be controlled or observed.

Reauthentication is implemented through `nectra provider login anthropic|openai|google`
and `/login PROVIDER` in chat. The provider CLI owns credentials and its interactive
sign-in UI. Nectra neither captures passwords/tokens nor invokes logout automatically.
Recognized authentication failures exclude the provider for the running chat;
the login workflow restores eligibility, and a new message verifies access.
