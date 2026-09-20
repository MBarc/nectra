#!/usr/bin/env python3
"""Refuse a commit that stages files another agent owns.

Run as a git pre-commit hook. Two agents share this checkout, and `git add -A`
sweeps up whatever the other one happens to have mid-edit -- which lands their
unreviewed work inside an unrelated commit, under a message that does not
describe it. Saying "I will scope my commits" did not prevent that three times,
so this enforces it.

Ownership lives in .collab/ownership.json as {agent: [glob, ...]}. Set
COMMIT_AGENT to identify yourself; anything you stage matching another agent's
globs blocks the commit.

Deliberate cross-boundary commits are normal -- reviewing and committing a
peer's finished work is part of the workflow. Pass them explicitly:

    COMMIT_SCOPE_ALLOW="src/executor/*,tests/native_terminal.rs" git commit ...

which forces the paths to be named, and therefore described.
"""

import fnmatch
import json
import os
import subprocess
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
OWNERSHIP = os.path.join(ROOT, ".collab", "ownership.json")


def staged_paths():
    result = subprocess.run(
        # Deletions count: removing a peer's file is exactly the kind of
        # cross-boundary change this is meant to surface.
        ["git", "diff", "--cached", "--name-only"],
        capture_output=True,
        text=True,
        cwd=ROOT,
    )
    return [line.strip().replace("\\", "/") for line in result.stdout.splitlines() if line.strip()]


def matches(path, globs):
    return any(fnmatch.fnmatch(path, pattern) for pattern in globs)


def specificity(path, globs):
    """Length of the longest matching pattern, as a crude specificity score.

    An explicit claim must beat a peer's broader wildcard, so comparing the
    most specific match on each side is what decides ownership.
    """
    return max(
        (len(pattern) for pattern in globs if fnmatch.fnmatch(path, pattern)),
        default=-1,
    )


def main():
    me = os.environ.get("COMMIT_AGENT", "Claude")
    allow = [p.strip() for p in os.environ.get("COMMIT_SCOPE_ALLOW", "").split(",") if p.strip()]

    try:
        with open(OWNERSHIP, encoding="utf-8") as handle:
            ownership = json.load(handle)
    except (OSError, ValueError):
        return 0  # No ownership map: nothing to enforce.

    others = {
        agent: globs for agent, globs in ownership.items() if agent.lower() != me.lower()
    }
    if not others:
        return 0

    mine = ownership.get(me) or next(
        (globs for agent, globs in ownership.items() if agent.lower() == me.lower()), []
    )

    intruders = []
    for path in staged_paths():
        if matches(path, allow):
            continue
        # Resolve by specificity, so an explicit claim of your own beats a
        # peer's broader wildcard but not their equally explicit one.
        own = specificity(path, mine)
        for agent, globs in others.items():
            theirs = specificity(path, globs)
            if theirs >= 0 and theirs > own:
                intruders.append((path, agent))
                break

    if not intruders:
        return 0

    print("commit blocked: staged files owned by another agent\n", file=sys.stderr)
    for path, agent in intruders:
        print("  {0:<44} owned by {1}".format(path, agent), file=sys.stderr)
    print(
        "\nUnstage them, or name them so the commit message can describe them:\n"
        '  COMMIT_SCOPE_ALLOW="{0}" git commit ...'.format(
            ",".join(path for path, _ in intruders)
        ),
        file=sys.stderr,
    )
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
