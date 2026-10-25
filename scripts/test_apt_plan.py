"""python scripts/test_apt_plan.py -- apt_plan.py picks the right release for every deb."""
import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
import apt_plan as p  # noqa: E402


def rel(tag, day, pre=False, debs=(), draft=False, body=""):
    return {"tag_name": tag, "created_at": f"2026-11-{day:02}T00:00:00Z", "prerelease": pre,
            "draft": draft, "name": tag, "body": body,
            "assets": [{"name": d} for d in debs] + [{"name": "nectra-x86_64-unknown-linux-gnu.tar.gz"}]}


laya1, laya2, key = "nectra-laya_1_amd64.deb", "nectra-laya_2_amd64.deb", "nectra-archive-keyring_1_all.deb"
rels = [
    rel("v0.4.5", 1),  # no debs: never counted
    rel("v0.5.0-alpha.1", 2, True, ["nectra_0.5.0~alpha.1-1_amd64.deb", laya1, key]),
    rel("v0.5.0-alpha.2", 3, True, ["nectra_0.5.0~alpha.2-1_amd64.deb", laya1, key]),
    rel("v0.5.0", 4, False, ["nectra_0.5.0-1_amd64.deb", laya1, key]),
    rel("v0.5.1", 5, False, ["nectra_0.5.1-1_amd64.deb", laya2, key], body="bad build [yanked]"),
    rel("v0.5.2", 6, False, ["nectra_0.5.2-1_amd64.deb", laya2, key]),
    rel("v0.6.0-alpha.1", 7, True, ["nectra_0.6.0~alpha.1-1_amd64.deb", laya2, key]),
    rel("v0.6.0-alpha.2", 8, True, ["nectra_0.6.0~alpha.2-1_amd64.deb", laya2, key], draft=True),
    rel("laya-model-2", 9, True, ["nectra_9_amd64.deb"]),  # not a v* tag
]
# gh api --paginate: one array per page.
text = json.dumps(rels[:4]) + "\n" + json.dumps(rels[4:])
got = p.plan(p.releases(text))
assert got == [
    ("alpha", "v0.5.0-alpha.1", "nectra_0.5.0~alpha.1-1_amd64.deb"),
    ("alpha", "v0.5.0-alpha.2", "nectra_0.5.0~alpha.2-1_amd64.deb"),
    ("alpha", "v0.6.0-alpha.1", "nectra_0.6.0~alpha.1-1_amd64.deb"),
    # Support debs come from the oldest release that has them, yanked ones aside.
    ("stable", "v0.5.0", "nectra_0.5.0-1_amd64.deb"),
    ("stable", "v0.5.0-alpha.1", key),
    ("stable", "v0.5.0-alpha.1", laya1),
    ("stable", "v0.5.2", "nectra-laya_2_amd64.deb"),
    ("stable", "v0.5.2", "nectra_0.5.2-1_amd64.deb"),
], got

# Only the last 3 of each kind.
many = [rel(f"v1.{i}.0", i + 1, False, [f"nectra_1.{i}.0-1_amd64.deb", key]) for i in range(5)]
names = [n for _, _, n in p.plan(many)]
assert names == [key, "nectra_1.2.0-1_amd64.deb", "nectra_1.3.0-1_amd64.deb", "nectra_1.4.0-1_amd64.deb"], names
assert p.plan(many)[0][1] == "v1.0.0"
print("ok")
