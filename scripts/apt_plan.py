#!/usr/bin/env python3
"""Chooses the debs apt-publish.yml publishes.

    gh api --paginate repos/MBarc/nectra/releases | python3 scripts/apt_plan.py

Prints one line per deb: SUITE TAG ASSET.
- Only v* releases that are not drafts, not [yanked] (title or notes) and have debs.
- stable: the nectra debs of the last 3 releases; alpha: of the last 3 prereleases.
- Every other deb (nectra-laya, nectra-archive-keyring) is rebuilt by every
  release, so it is taken from the oldest release that has it: the bytes
  published for a version never change. It goes to stable if a stable release
  carries it.
"""
import json
import sys

KEEP = 3


def releases(text):
    """`gh api --paginate` prints one JSON array per page, back to back."""
    decoder, i, out = json.JSONDecoder(), 0, []
    while True:
        while i < len(text) and text[i].isspace():
            i += 1
        if i == len(text):
            return out
        page, i = decoder.raw_decode(text, i)
        out.extend(page if isinstance(page, list) else [page])


def debs(release):
    return [a["name"] for a in release["assets"] if a["name"].endswith(".deb")]


def plan(rels):
    rels = sorted(
        (r for r in rels
         if r["tag_name"].startswith("v") and not r["draft"] and debs(r)
         and "[yanked]" not in (r.get("name") or "") + " " + (r.get("body") or "")),
        key=lambda r: r["created_at"],
        reverse=True,
    )
    chosen = {}
    for suite, pre in (("stable", False), ("alpha", True)):
        for r in [r for r in rels if r["prerelease"] == pre][:KEEP]:
            for name in debs(r):
                if name in chosen:
                    continue
                source = r if name.startswith("nectra_") else next(
                    o for o in reversed(rels) if name in debs(o))
                chosen[name] = (suite, source["tag_name"])
    return sorted((suite, tag, name) for name, (suite, tag) in chosen.items())


if __name__ == "__main__":
    for line in plan(releases(sys.stdin.read())):
        print(*line)
