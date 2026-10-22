# A shared Laya service per machine

Status: design only, nothing is implemented. Written 2026-10-22 from the measurements in
`docs/measurements/2026-10-22-laya/REPORT.md` in the PonsOS repo and the ONNX Runtime
experiments in [LAYA-MEMORY.md](LAYA-MEMORY.md).

## Problem

Every Nectra process that routes with Laya loads its own copy of the model. On a 2 GiB, 2 vCPU PonsOS agent VM:

| | Measured |
|---|---|
| Idle machine, nothing loaded | about 190 MiB used, 1,719 MiB available |
| One process with Laya loaded | 1,029 MiB peak RSS (about 850 MiB of it private anonymous memory, see LAYA-MEMORY.md) |
| Load | about 16 s per process (hash 667 MiB of weights, build the ONNX session, one warm-up message) |
| One message once loaded | about 2 s |

`nectra chat` now loads once per session in the background (`laya::warm`), so a chat pays the load once. But a chat and an
agent task (`nectra run`, a worker unit) are separate processes, and two copies do not fit:
190 + 2 x 1,030 = 2,250 MiB against 1,907 MiB total. The kernel OOM-kills one of them, or the provider CLI it was about to
start. One-shot `nectra route` calls pay the full 16 s every time.

## Proposal

One long-lived `nectra laya-serve` process per machine owns the model. Nectra processes ask it over a unix socket and fall
back to keywords exactly as they do today when it is not there.

- **Socket:** `$XDG_RUNTIME_DIR/nectra/laya.sock` for a user service (mode 0600, directory 0700). A system-wide agent VM with
  one worker user uses the same path under that user. A machine-wide socket in `/run/nectra/` would need an access policy
  (any local user could spend the CPU and read prompts) that nothing needs yet.
- **Protocol:** one request per line of JSON, one response line: `{"prompt","previous","roles":[...]}` in,
  `{"task","confidence","difficulty","new_task","role"}` or `{"error":"..."}` out. It is `jev::Answer` serialised, so the
  client side is a few lines next to `laya::ask` and the existing `Answer` type does not change. A `version` field in each
  message lets a newer client refuse an older server instead of misreading it.
- **Client:** `laya::ask` tries, in order, an in-process engine (if this process was told to own one, e.g. in tests and in
  `nectra eval`), then the socket. `laya_timeout_ms` bounds the whole request including queueing behind another caller, because
  that is what the caller experiences. The route line distinguishes the causes: "keywords · Laya service not running",
  "keywords · Laya still loading", "keywords · Laya timed out", "keywords · Laya failed to load: ...".
- **Server:** loads exactly as `laya::load` does today, including the pinned-hash check, so a swapped model is refused
  before it serves anyone. It answers requests one at a time (ONNX Runtime already uses both cores for one message; two
  concurrent messages would not finish sooner). While loading it accepts connections and answers `{"error":"loading"}`
  immediately, so clients never wait on the 16 s.

### Starting and stopping it

Preferred: **systemd socket activation** (`nectra-laya.socket` plus `nectra-laya.service`). The first connection starts the
service; clients that connect during the load get "loading" and use keywords, which is the behaviour `nectra chat` has now.
The service **exits after N minutes without a request** (default 10) and gives the 1 GiB back; the next message starts it
again and routes with keywords while it loads. No systemd (macOS, Windows, containers): the first Nectra process that finds
no socket may start the server itself, detached, and the same idle exit applies. A lock file next to the socket stops two
processes starting two servers.

Idle exit matters more than it looks: on a 2 GiB machine the point is that Laya costs nothing while nobody is chatting.

## RAM math for a 2 GiB agent VM

Figures: 1,907 MiB total, about 190 MiB idle OS, about 1,030 MiB for a Laya process (850 MiB private after load, 1,030
MiB peak), clients about 12 MiB each (a keyword-only `nectra route` measured 12 MiB). Claude Code's own size was not
measured; treat 400-600 MiB as an assumption to check.

| Setup | Resident | Left for the provider CLI |
|---|---:|---:|
| Today: chat + one agent task, each loads Laya | 190 + 2 x 1,030 = 2,250 | none (does not fit) |
| Today: one process with Laya | 190 + 1,030 = 1,220 | about 690 |
| Shared service, loaded, 2 clients | 190 + 1,030 + 2 x 12 = 1,244 | about 660 |
| Shared service, idle past the timeout | 190 | about 1,700 |
| No Laya (keywords) | 190 | about 1,700 |

The shared service makes concurrent routing *possible*, but it does not make it *cheap*: while it is loaded, about 660 MiB
remains for a provider CLI and the rest of the machine. On a 2 GiB VM that is workable for one agent at a time and not for
two; the honest recommendation is unchanged: Laya from 4 GiB up, keywords below, and the service for machines that run more
than one Nectra process. Anything that shrinks the resident model (LAYA-MEMORY.md) shrinks every row above.

## Trade-offs

**For**
- One copy per machine regardless of how many chats and tasks run: 1 GiB, not 1 GiB each.
- Cold start paid once, then every later process (including one-shot `nectra route`) answers in about 2 s instead of 16 s.
- The model's integrity check runs in one place.

**Against**
- A new long-lived process, a socket, a protocol and a lifecycle to maintain, for a classifier that already has a working
  fallback. It is only worth it for machines that run more than one Nectra process at a time.
- Messages are serialised: two chats routing at once each wait up to a message behind the other (about 2 s on 2 vCPUs),
  which with `laya_timeout_ms = 1000` means the second one falls back to keywords. Raising the timeout, or queueing
  with a deadline, is a tuning decision to make from measurements, not a guess now.
- It holds prompts from every client in one process. The 0600 socket keeps that to one user; a machine-wide service would
  need real authorisation.
- A crashed or wedged server must never hang a chat: every client call has the deadline above and a stale socket is
  treated as "not running".
- The idle-exit timer is a trade between RAM held and 16 s cold starts. 10 minutes is a guess to tune.

## Alternatives considered

1. **Do nothing; keywords on small machines.** Zero code and zero RAM. Right below 2 GiB; gives up Laya's routing quality.
2. **Smaller resident model** (LAYA-MEMORY.md). Helps every setup, including this one. The weights are private anonymous memory, so
   what the file mapping would give (pages the kernel can drop) is not available through the ONNX Runtime options the `laya`
   crate exposes.
3. **Fork per request / zygote.** A loaded parent forked per request would share the weights copy-on-write, but ONNX Runtime
   threads do not survive `fork`, so this is not safe.
4. **Run the classifier on another machine.** The existing option is Jev (hosted); a LAN Laya server is the same protocol over TCP
   and brings the authorisation problem above. Not for now.

## Rollout, if it is built

1. `nectra laya-serve` and the client path behind `[classifier] laya_service = "auto" | "off"`, default off, tests with a
   fake in-process server.
2. Socket-activation unit files shipped with the PonsOS package, with the idle exit.
3. Measure on the 2 GiB VM: two concurrent chats, queueing delay, and the real headroom for Claude Code.
4. Turn the default on only where the measured headroom is there.
