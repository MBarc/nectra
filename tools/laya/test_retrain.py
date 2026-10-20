"""python tools/laya/test_retrain.py -- checks the parts of retrain that can break silently."""
import importlib.machinery
import importlib.util
import json
from pathlib import Path

HERE = Path(__file__).parent
loader = importlib.machinery.SourceFileLoader("retrain", str(HERE / "retrain"))
spec = importlib.util.spec_from_loader("retrain", loader)
retrain = importlib.util.module_from_spec(spec)
loader.exec_module(retrain)

criteria = json.loads((retrain.LAYA / "criteria.json").read_text(encoding="utf-8"))
data = [
    dict(prompt=f"p{i}", followup=f"f{i}", task=t, difficulty=i % 3, role=r)
    for i, (t, r) in enumerate([("debugging", "coding"), ("writing", "general")] * 6)
]
items = retrain.to_laya_items(data, criteria, claimed={"coding", "planning", "general"})
singles = [x for x in items if isinstance(x["state"], str)]
pairs = [x for x in items if isinstance(x["state"], dict)]
assert len(singles) == 12 and len(pairs) == 4, (len(singles), len(pairs))
# Every example asks the task; difficulty and role ride on every other one.
full = [x for x in singles if "role" in x["questions"]]
assert len(full) == 6 and all(set(x["questions"]) == {"task", "difficulty", "role"} for x in full)
assert all(set(x["questions"]) == set(x["expected"]) == {"task"} for x in singles if x not in full)
q = singles[0]["questions"]
# The exact questions src/laya.rs asks: bare labels, criteria order.
assert q["task"]["instructions"] == "What task is this?"
assert list(q["task"]["criteria"]) == [k for k, _ in criteria["tasks"]]
assert set(q["task"]["criteria"].values()) == {""}
assert q["difficulty"]["criteria"] == ["easy", "moderate", "hard"]
role_lists = {tuple(x["questions"]["role"]["criteria"]) for x in full}
assert ("coding", "planning", "general") in role_lists and len(role_lists) == 2, role_lists
assert all(x["expected"]["role"] in x["questions"]["role"]["criteria"] for x in full)
follow = [x for x in pairs if x["expected"]["new_task"] is False]
new = [x for x in pairs if x["expected"]["new_task"] is True]
assert len(follow) == len(new) == 2
assert all(set(x["questions"]) == set(x["expected"]) == {"task", "new_task"} for x in pairs)
for x in follow:  # a follow-up keeps the previous message's task
    prev = next(d for d in data if d["prompt"] == x["state"]["previous_message"])
    assert x["expected"]["task"] == prev["task"]
for x in new:  # a new message is labelled as itself
    cur = next(d for d in data if d["prompt"] == x["state"]["current_message"])
    assert x["expected"]["task"] == cur["task"] != next(
        d for d in data if d["prompt"] == x["state"]["previous_message"])["task"]

guarded = [set(retrain.norm("my docker container exits with code 137 right after startup").split())]
assert retrain.near_duplicate("My Docker container exits with code 137, right after startup!", guarded)
assert not retrain.near_duplicate("my docker container is slow", guarded)
print("ok")
