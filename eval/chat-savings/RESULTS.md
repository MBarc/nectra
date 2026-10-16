# Chat savings: results (2026-10-16)

**Summary.** On three coding workloads, the cheapest coder (gpt-5.6-luna) passed every
check at 3–4% of Opus's cost. The old balanced default cost 66–136% of Opus. The new
balanced default sends ordinary coding to the cheap coder first, and it cost 4–18% of
Opus with no lost checks.

## What was measured

Each run is one scripted chat through `nectra chat` (stream view) on a fresh copy of
[`fixture/`](fixture), a small Python invoice project with a tax bug.
[`run.py`](run.py) drives the chat. A checker then scores the final repo with its own
probes, independent of the tests the agent wrote. `nectra usage --session` reads exact
token counts from the providers' own logs, and [`report.py`](report.py) prices them.

| Workload | Script | Checks |
|---|---|---|
| Light | [`script.txt`](script.txt): overview, fix the bug, add `--json`, README, rename, two thank-yous | 7 ([`check.py`](check.py)) |
| Hard | [`script-hard.txt`](script-hard.txt): fix the bug, Decimal half-up rounding, per-category tax with proportional discount, CSV input with error exit | 11 ([`check_hard.py`](check_hard.py)) |
| Expert | [`script-expert.txt`](script-expert.txt): globally optimal coupon assignment with lexicographic ties, up to 80×80 | 17 ([`check_expert.py`](check_expert.py)), including 5 random dense cases compared against a reference solver. Greedy scores 8. |

Each checker was validated twice. The untouched fixture scores near zero. A hand-written
reference scores full marks.

Cost is **API-equivalent USD** from [`prices.json`](prices.json), so subscription and API
users compare on one scale. On a subscription the same ratios show up as quota used.
Claude Code's cache writes were confirmed to be 1-hour TTL in its own logs. The prices
come from third-party sources, and Jev's own fee (about $0.00003 a message) is not included.

## Results

Median cost per chat. The percentage is relative to Opus pinned on the same workload,
and N/M means N of M runs scored full marks.

| Configuration | Light | Hard | Expert |
|---|---|---|---|
| opus pinned | $0.706 (100%) 3/3 | $0.771 (100%) 2/2 | $0.546 (100%) 2/2 |
| gpt-5.6-sol pinned | $0.635 (90%) 3/3 | $1.013 (131%) 2/2 | $0.427 (78%) 2/2 |
| gpt-5.6-luna pinned | $0.020 (3%) 3/3 | $0.030 (4%) 2/2 | $0.014 (3%) 2/2 |
| fast mode | $0.031 (4%) 3/3 | $0.026 (3%) 2/2 | $0.015 (3%) 2/2 |
| balanced, old default | $0.687 (97%) 3/3 | $1.046 (136%) 2/2 | $0.361 (66%) 2/2 |
| balanced, old, with Jev | $0.557 (79%) 3/3 | $0.485 (63%) 2/2 | $0.456 (84%) 2/2 |
| **balanced, new default** | **$0.129 (18%) 2/2** | **$0.028 (4%) 2/2** | **$0.021 (4%) 2/2** |
| balanced, new, with Jev | $0.379 (54%) 1/1 | $0.024 (3%) 1/1 | $0.015 (3%) 1/1 |

51 runs, all valid: every message answered, no failed attempts, pins honoured, no
rate limits, token data present for every turn.

## What it shows

- **Which model writes the code decides the bill.** Moving thank-yous and overviews to
  cheap models saves little, because on a resumed session those turns are already cheap
  cache reads. The old default sent all coding to gpt-5.6-sol, so it cost about what
  Opus did.
- **Switching to a Claude model costs a startup fee.** A cold Claude Code session writes
  22–27k tokens of 1-hour cache, about $0.10 on Sonnet. Most of the new default's light
  cost is the one Sonnet overview turn. Codex sessions have no cache-write charge.
- **Sol is not cheaper than Opus at these prices.** Its cache reads cost $0.50 per million
  against Opus's $0.20. At the promotional Sol rate one source quotes ($4 / $0.40 / $20),
  the two are roughly level.
- **Jev did not help the new default on the light workload.** It sent the README and
  rename turns to Sonnet and paid for a cold session.

## What it does not show

- **It does not show that a cheap model is always enough.** All three workloads use one
  small fixture with precise specs. Large repositories, vague requests and long debugging
  sessions are untested, so call this "no lost checks", not "no quality loss".
- **The run counts are small**: 2–3 per configuration. That's enough to see a 25× cost
  gap, not to rank configurations that are close together.
- **It is an end-to-end comparison of Nectra configurations**, not of bare CLIs.

## What changed because of it

Balanced mode now tries the `simple-coding` model first for ordinary coding
(`policy::cheap_first`). Demanding work still escalates, best mode keeps the strong
coder, and a registry where no model claims `simple-coding` is unchanged. If the cheap
coder isn't good enough for some work, use `/mode best` for that chat, or `--mode best`.

## Reproduce

```sh
cargo build --release --locked
python eval/chat-savings/run.py OUT b1-opus --model opus
python eval/chat-savings/run.py OUT b1-auto --workload hard
python eval/chat-savings/run.py OUT b1-jev --workload expert --jev   # needs TYPESAFE_API_KEY
python eval/chat-savings/report.py OUT
```

The design was reviewed by gpt-6-astra twice, before the runs and after the first 36.
Its fixes are in: behavioural checkers, validity verdicts, failed-attempt accounting and
the expert workload.
