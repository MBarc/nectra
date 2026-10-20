# Laya task-labelling rubric

One label per prompt: the kind of work the message asks an AI assistant to do.
The ten labels and their one-line definitions are Nectra's own (the `task`
criteria in `src/jev.rs`, copied in `criteria.json`). This rubric settles the
cases those lines leave open. It is what labelers and adjudicators use for
`fresh-test.json` and for checking generated training data.

## Labels

| Label | Use when the deliverable is... |
|---|---|
| `code_generation` | New code: a feature, script, function, endpoint, test, flag, config file, query, regex, port to another language. |
| `debugging` | Making something that is broken work: an error, crash, wrong output, failing test or build, flaky behaviour, or finding the cause of an incident. |
| `refactoring` | Existing code changed **without changing what it does**: restructure, rename, split, deduplicate, simplify, speed up, modernise syntax, upgrade an API call with equal behaviour. |
| `code_review` | A judgement on existing code, a diff, a PR, a config or a security design: review, audit, critique, "is this OK", "anything wrong with this". No failure has been reported. |
| `architecture` | Deciding how a system is structured: components, services, data models, schemas, protocols, technology trade-offs for a system being designed. |
| `research` | Facts from outside the conversation: what exists, what people say, product comparisons, prices, current state of a law/library/field, whether something has been done before. |
| `writing` | Prose for people: emails, bios, posts, stories, letters, proposals, docs, READMEs, release notes, rewording or translating text. |
| `planning` | An ordered set of steps, milestones, schedule or approach for doing or building something (software or physical). |
| `data_analysis` | Answers computed from data the user has: CSVs, logs, metrics, spreadsheets, statistics, charts, rankings, aggregates, trends. |
| `general` | Chit-chat, thanks, greetings, trivial facts or arithmetic, explanations of a concept, questions about the assistant itself. |

## Tie-breaks (apply in order)

1. **Deliverable over topic.** Label what the user wants back, not the subject.
   "Write a blog post about Kubernetes" is `writing`; "explain Kubernetes pods"
   is `general`; "set up a Kubernetes deployment yaml" is `code_generation`.
2. **Something is broken -> `debugging`.** A reported failure, error, wrong
   number or crash wins over review, research or analysis, including "look into
   what caused the outage" and "why are these logs full of 500s".
3. **New behaviour -> `code_generation`; same behaviour -> `refactoring`.**
   Adding a feature while touching existing code is `code_generation`.
   Performance work on working code ("make this loop faster") is `refactoring`.
4. **Judgement without a failure -> `code_review`.** "Is this safe", "would you
   merge this", "check my threat model / migration / Dockerfile".
5. **Design vs. sequence.** What the parts are and how they connect ->
   `architecture`. What to do first, next and when -> `planning`. Physical
   builds ("build a bookshelf", "plan a shed") are `planning`, never
   `code_generation`.
6. **Outside facts vs. own data.** Looking things up -> `research`. Computing
   from data the user supplies or points at -> `data_analysis`.
7. **Prose about software is still prose.** A design doc, README, changelog or
   proposal is `writing` unless the message asks to make the design decisions
   themselves (then `architecture`).
8. **`general` is the floor.** Use it only when no other label fits: greetings,
   thanks, quick facts, arithmetic, conversions, "explain X", "what model are
   you". A concept explanation stays `general` even when technical.
9. **Explicit model requests are ignored.** "use opus and plan a shed" is
   labelled as "plan a shed".

## Process for the fresh test set

- Prompts were written for this set only, by three writers with different
  briefs (see `fresh-test.README.md`), and are never used for training or model
  selection. The training generator filters out near-duplicates of them.
- Each prompt was labelled blind by two independent labelers using this rubric.
- Every disagreement was adjudicated against the rubric and the decision noted
  in `fresh-test-adjudication.json`. Prompts that stayed genuinely ambiguous
  under the rubric were dropped, not forced.
