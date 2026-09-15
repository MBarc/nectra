# Design review: v0.3

## Outcome

Preserve the small local router architecture. Separate classification, policy,
configuration, execution and CLI handling in a reusable Rust library. Keep
routing offline and make the selected model and decision basis inspectable.

## Shared conversation

`chat` adds a main conversation independent of any individual provider session.
Each message is routed again, after excluding profiles with unavailable native
executables. Short generic follow-ups borrow the previous user task as a routing
hint. Explicit task messages are classified on their own.

The session owns a durable JSONL journal and a history of completed exchanges.
Inputs are saved before execution; only a successful final response enters
context. A failed or interrupted turn stays in the journal but does not become
an assistant answer. Recognized rate-limit rejections before streamed activity may
retry another eligible model. Ollama availability errors and recognized authentication
errors can also fall back before output. Claude/Codex provider sessions are ephemeral;
Gemini CLI may retain its own logs, but no native session is resumed between turns;
every turn receives the bounded shared transcript in the current working directory.

Claude stream-json result events and Codex JSONL completion events define turn
completion. Partial display text, tool output and transient errors are separate
from the final saved answer. Concurrent stdin writing and stdout reading avoid
pipe deadlocks. Event lines are limited to 8 MiB.

The journal uses an operating-system file lock, not a stale PID lockfile. Resume
validates the original working directory and refuses corrupt complete records;
only an unfinished final line can be discarded. History is bounded per request,
with omissions reported, while the full journal remains on disk.

## Findings addressed

| Starter defect | Change |
| --- | --- |
| Leading-dot float literals invalid in Rust and TOML | Corrected literals; compile and embedded-config tests |
| Default mode ignored by CLI default | Optional CLI override; typed config mode |
| Arbitrary repository config implicitly executed | Explicit/user/embedded precedence only |
| Embedded registry could load an unrelated file | Always pair embedded config with embedded registry |
| Substring matches such as planet/plan and contest/test | Normalized word and phrase boundaries |
| Writing tests classified as prose | Test vocabulary and stable category tie precedence |
| Fabricated probability-like confidence | Candidate scores, nullable score gap and ambiguity flag |
| Model ties depended on registry order | Stable model-ID tie-break |
| Config typos and duplicate IDs accepted | Strict schema and validation |
| Prompt text passed as positional process input | Piped stdin; native argument arrays |
| Windows npm Codex shim incompatible with native spawning | Discover native npm package payload |
| Doctor ignored custom executable settings | Shared executable resolver and bounded version checks |
| Child failure status collapsed into generic error | Preserve normal child exit codes |
| Registry errors blocked config inspection | Load registry only where required |
| Barely any regression coverage | CLI, config, routing and child-process tests plus offline corpus |
| Installer promised unsupported prebuilt targets | Align platform selection and release matrix |
| Downloads not checked before installation | SHA-256 sidecars and installer verification |

## Deliberate boundaries

No server, GUI, API key store, daemon or parallel
agent orchestration. Ranking does not query the network. Standalone `route`
ranks all enabled profiles; chat narrows those to installed runtimes before
ranking. Account access is not inferred from executable availability. Retrying
automatically could duplicate work or charges.

A low score margin describes disagreement between policy profiles, not uncertainty
about the prompt classification. Keep the historic confidence_margin key to avoid
unnecessary config migration, but do not label its output a probability.

The provider enum constrains routing, not downstream network behavior. Restricting
an agent's endpoints and tool traffic requires enforcement in that agent or its
execution environment. Nectra must not claim to provide such enforcement.

## Further work

1. Gather a larger held-out corpus of real mixed tasks and negation cases. The
   bundled 25 examples are regression fixtures, not an independent benchmark.
2. Calibrate model capability profiles using task outcomes, measured latency
   and actual cost; version the corpus and policy together.
3. Add opt-in repository metadata extraction with explicit size/privacy bounds.
4. Extend live-provider compatibility testing and optional context summarization;
   native hidden state and raw tool traces are not transferred between providers.
5. Add signed release provenance once a canonical repository and release owner
   are established.

The release workflow and installers can be prepared locally, but publishing,
platform-specific CI runs and live account/model compatibility remain separate
validation steps.
