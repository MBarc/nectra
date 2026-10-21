"""python tools/laya/heldout.py fresh-test-3 -- builds a new held-out test set.

Writers see only RUBRIC.md and the labels. Near-duplicates (token Jaccard >= 0.8) of
the training data (prompts and follow-ups), every earlier eval set and each other
are dropped. Two blind labelers (Opus, Sonnet) label every prompt; an Opus
adjudicator settles disagreements by the rubric, or drops the prompt. Claude
models only. Writes eval/laya/<name>.json and <name>-adjudication.json; never
runs a model on the result.
"""
import concurrent.futures
import importlib.machinery
import importlib.util
import json
import random
import sys
from pathlib import Path

HERE = Path(__file__).parent
loader = importlib.machinery.SourceFileLoader("retrain", str(HERE / "retrain"))
spec = importlib.util.spec_from_loader("retrain", loader)
retrain = importlib.util.module_from_spec(spec)
loader.exec_module(retrain)
LAYA, log, norm, near_duplicate, ask_llm = retrain.LAYA, retrain.log, retrain.norm, retrain.near_duplicate, retrain.ask_llm

WRITER = "claude -p --model sonnet"
LABELERS = ["claude -p --model opus", "claude -p --model sonnet"]
ADJUDICATOR = "claude -p --model opus"
PER_CALL = 36
# The three briefs of fresh-test and fresh-test-2 (eval/laya/fresh-test.README.md).
BRIEFS = [
    "developers in a terminal: terse, lowercase, typos, pasted errors and short code snippets",
    "non-specialist and mixed users: small business, students, hobbyists, office workers; "
    "about one in seven written in non-native English",
    "deliberately hard boundary cases with misleading keywords from other labels, "
    "whose label is still decided by the rubric",
]


def write(labels, rubric, label, brief):
    return [
        x["prompt"]
        for x in ask_llm(WRITER, f"""You write test messages for a classifier that sorts messages sent to an AI
assistant (which can code and write) into one of these task labels:
{labels}

The labelling rubric, which decides every case:
{rubric}

Write {PER_CALL} distinct, realistic messages whose one clearly best label under the rubric is `{label}`.
Writers' brief: {brief}.
Vary subject, length and phrasing; do not start more than two with the same word.
Reply with only a JSON array: [{{"prompt": "..."}}]""")
        if isinstance(x, dict) and isinstance(x.get("prompt"), str) and x["prompt"].strip()
    ]


def label(model, labels, rubric, batch):
    items = "\n".join(json.dumps({"id": i, "prompt": p}, ensure_ascii=False) for i, p in batch)
    answer = ask_llm(model, f"""Label each message sent to an AI assistant with its task. Judge each one yourself.

Labels:
{labels}
Rubric (apply its tie-breaks in order):
{rubric}

Messages (JSON lines):
{items}

Reply with only a JSON array: [{{"id": ..., "label": "...", "unsure": true|false, "note": "short, only if unsure"}}]""")
    return {a["id"]: a for a in answer if isinstance(a, dict) and "id" in a}


def adjudicate(labels, rubric, x):
    answer = ask_llm(ADJUDICATOR, f"""Two labelers disagree on the task label of a message sent to an AI assistant.
Decide it by the rubric alone.

Labels:
{labels}
Rubric (apply its tie-breaks in order):
{rubric}

Message: {json.dumps(x["prompt"], ensure_ascii=False)}
Labeler 1: {x["labeler_1"]["label"]}   Labeler 2: {x["labeler_2"]["label"]}

If the rubric settles it, give that label. If it genuinely cannot, answer "drop".
Reply with only a JSON array of one object: [{{"decision": "<label or drop>", "reason": "one sentence"}}]""")
    return answer[0]


def main(name):
    criteria = json.loads((LAYA / "criteria.json").read_text(encoding="utf-8"))
    rubric = (LAYA / "RUBRIC.md").read_text(encoding="utf-8")
    tasks = [k for k, _ in criteria["tasks"]]
    labels = "\n".join(f"- {k}: {v}" for k, v in criteria["tasks"])
    if (LAYA / f"{name}.json").exists():
        sys.exit(f"{name}.json exists; a held-out set is written once")

    guarded = []
    for f in LAYA.glob("*.json"):
        if "adjudication" not in f.name and f.name != "criteria.json":
            guarded += [c["prompt"] for c in json.loads(f.read_text(encoding="utf-8"))]
    for f in ["holdout.json", "generalization.json", "tasks.json"]:
        if (LAYA.parent / f).exists():
            guarded += [c["prompt"] for c in json.loads((LAYA.parent / f).read_text(encoding="utf-8"))]
    for f in (HERE / "work").glob("data-*.json"):
        for x in json.loads(f.read_text(encoding="utf-8")):
            guarded += [x["prompt"]] + ([x["followup"]] if x.get("followup") else [])
    guarded = [set(norm(g).split()) for g in guarded]
    log(f"guarding against {len(guarded)} earlier prompts")

    jobs = [(t, b) for t in tasks for b in range(len(BRIEFS))]
    written = []
    with concurrent.futures.ThreadPoolExecutor(6) as pool:
        futures = {pool.submit(write, labels, rubric, t, BRIEFS[b]): (t, b) for t, b in jobs}
        for f in concurrent.futures.as_completed(futures):
            t, b = futures[f]
            written += [{"prompt": p, "writer_intent": t, "brief": "abc"[b]} for p in f.result()]
    kept, mine, dropped = [], [], 0
    for x in written:
        if near_duplicate(x["prompt"], guarded + mine):
            dropped += 1
            continue
        mine.append(set(norm(x["prompt"]).split()))
        kept.append(x)
    log(f"{len(written)} written, {dropped} near-duplicates dropped, {len(kept)} kept")
    random.Random(3).shuffle(kept)
    for i, x in enumerate(kept):
        x["id"] = f"u{i:04d}"

    batches = [[(x["id"], x["prompt"]) for x in kept[i : i + 60]] for i in range(0, len(kept), 60)]
    for n, model in enumerate(LABELERS, 1):
        got = {}
        with concurrent.futures.ThreadPoolExecutor(6) as pool:
            for g in pool.map(lambda b: label(model, labels, rubric, b), batches):
                got.update(g)
        for x in kept:
            a = got.get(x["id"], {})
            x[f"labeler_{n}"] = {"label": a.get("label"), "unsure": bool(a.get("unsure")), "note": a.get("note") or ""}
    final, record = [], []
    for x in kept:
        l1, l2 = x["labeler_1"]["label"], x["labeler_2"]["label"]
        if l1 == l2 and l1 in tasks:
            x["decision"], x["reason"] = l1, "both blind labelers agree"
        elif l1 in tasks and l2 in tasks:
            a = adjudicate(labels, rubric, x)
            x["decision"], x["reason"] = a.get("decision"), "adjudicated: " + str(a.get("reason", ""))
        else:
            x["decision"], x["reason"] = "drop", "a labeler gave no valid label"
        if x["decision"] not in tasks:
            x["decision"] = "drop"
        flagged = l1 != l2 or x["writer_intent"] != x["decision"] or x["labeler_1"]["unsure"] or x["labeler_2"]["unsure"]
        if flagged:
            record.append(x)
        if x["decision"] != "drop":
            final.append({"prompt": x["prompt"], "task": x["decision"]})
    (LAYA / f"{name}.json").write_text(json.dumps(final, indent=1, ensure_ascii=False) + "\n", encoding="utf-8")
    (LAYA / f"{name}-adjudication.json").write_text(json.dumps(record, indent=1, ensure_ascii=False) + "\n", encoding="utf-8")
    agree = sum(x["labeler_1"]["label"] == x["labeler_2"]["label"] for x in kept)
    log(f"{name}: {len(final)} prompts; labelers agreed on {agree}/{len(kept)}; "
        f"{len(record)} flagged, {len(kept) - len(final)} dropped")


if __name__ == "__main__":
    main(sys.argv[1] if len(sys.argv) > 1 else "fresh-test-3")
