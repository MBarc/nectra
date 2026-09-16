#!/usr/bin/env python3
"""Block until the peer agent posts to AGENT_HANDOFF.md, then print what is new.

Run this in a blocking/background terminal tool. It returns on the first new
peer entry, so the wake-up and the read are the same event -- an agent does not
have to poll. Re-arm it after replying.

    python scripts/watch_handoff.py Claude 55

The first launch establishes a baseline. Later launches use a persistent cursor
so messages arriving between invocations are delivered on restart. Run only one
watcher per agent. Printing a message records delivery, not acknowledgment: write
an ACK in the handoff after acting on it. This process cannot wake an ended agent
turn; keep the agent turn running and re-arm after timeout while awaiting a peer.
"""

import argparse
import hashlib
import json
import os
import re
import sys
import time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
HANDOFF = os.path.join(ROOT, "AGENT_HANDOFF.md")
PEERS = {"Claude": "Codex", "Codex": "Claude"}


def entry_key(entry):
    return hashlib.sha256(entry.encode("utf-8")).hexdigest()


def load_seen(path, entries):
    try:
        with open(path, encoding="utf-8") as handle:
            return set(json.load(handle))
    except FileNotFoundError:
        # First launch establishes a baseline; subsequent launches retain unread
        # messages that arrived during the gap between watcher invocations.
        seen = {entry_key(e) for e in entries}
        save_seen(path, seen)
        return seen


def save_seen(path, seen):
    from handoff_append import write_atomic

    os.makedirs(os.path.dirname(path), exist_ok=True)
    write_atomic(path, json.dumps(sorted(seen)))


def peer_entries(peer):
    """Current entries written by `peer`, plus any user directive relayed to it."""
    try:
        with open(HANDOFF, encoding="utf-8") as handle:
            text = handle.read()
    except OSError:
        return []

    if "## Messages and handoffs" not in text:
        return []
    section = text.split("## Messages and handoffs", 1)[1]
    section = re.split(r"\n## ", section, 1)[0]

    # A bullet starts at a line beginning with "- "; wrapped lines belong to it.
    bullets = re.split(r"\n(?=- )", section)
    pattern = re.compile(r"^-\s+(?:\d{4}-\d{2}-\d{2}\s*/\s*)?(%s|User[\w-]*)\s*/" % peer)
    return [b.strip() for b in bullets if pattern.match(b.strip())]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("agent", choices=sorted(PEERS), help="which agent is watching")
    parser.add_argument(
        "seconds", nargs="?", type=int, default=55, help="timeout in seconds (default 55)"
    )
    parser.add_argument("--cursor", help="override persistent delivered-entry cursor path")
    args = parser.parse_args()

    if not 1 <= args.seconds <= 3600:
        sys.exit("timeout must be between 1 and 3600 seconds")

    peer = PEERS[args.agent]
    cursor = args.cursor or os.path.join(ROOT, ".collab", "watch-%s.json" % args.agent.lower())
    seen = load_seen(cursor, peer_entries(peer))
    print("watching for new %s entries for %ds" % (peer, args.seconds), flush=True)

    deadline = time.time() + args.seconds
    while time.time() < deadline:
        added = [e for e in peer_entries(peer) if entry_key(e) not in seen]
        if added:
            print("\n\n".join(added), flush=True)
            save_seen(cursor, seen | {entry_key(e) for e in added})
            return 0
        time.sleep(1.0)

    print("no new %s entries in this interval" % peer, flush=True)
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
