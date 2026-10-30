"""python scripts/test_laya_gate.py -- the gate fails on task accuracy, keyword fallbacks, tier accuracy and demanding routing."""
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
from laya_gate import failures  # noqa: E402

base = {"accuracy": 0.65,"demanding_model": {"tp": 13, "fp": 3, "fn": 7}}


def reports(acc=0.98, fallbacks=0, tier_fallbacks=0, tier_acc=0.65, tp=13, fp=3):
    task = {"accuracy": acc, "keyword_fallbacks": fallbacks}
    tier = {"keyword_fallbacks": tier_fallbacks, "tier": {"accuracy": tier_acc, "demanding_model": {"tp": tp, "fp": fp, "fn": 20 - tp}}}
    return task, tier


def check(**kw):
    return failures(*reports(**kw), 0.977, base, 0)


assert check() == []
for kw, word in [({"acc": 0.97}, "task accuracy"), ({"fallbacks": 1}, "task set"), ({"tier_fallbacks": 2}, "tier set"),
                 ({"tier_acc": 0.6}, "tier accuracy"), ({"tp": 12}, "hard specs"), ({"fp": 4}, "false escalations")]:
    bad = check(**kw)
    assert len(bad) == 1 and word in bad[0], (kw, bad)
# --max-extra-fp allows that many more false escalations.
assert failures(*reports(fp=4), 0.977, base, 1) == []
# A tier set without gold tiers can't vouch for routing.
assert failures({"accuracy": 0.98, "keyword_fallbacks": 0}, {"keyword_fallbacks": 0}, 0.977, base, 0) == ["the tier set has no gold tiers"]
print("ok")
