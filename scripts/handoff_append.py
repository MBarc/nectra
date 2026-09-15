#!/usr/bin/env python3
"""Append a well-formed entry to a section of AGENT_HANDOFF.md.

Keeps every entry in the shape both agents and the monitor parse:

    - YYYY-MM-DD / <agent> / <TAG> [id]: <body wrapped at 88 columns>

Examples:

    python scripts/handoff_append.py --agent Claude --tag STATUS --text "..."
    python scripts/handoff_append.py --agent Claude --tag BUG --section findings \
        --text "router.rs:88 falls through on an empty profile list"
    python scripts/handoff_append.py --agent User-to-Claude --tag DIRECTIVE \
        --text "wants a two-provider debate mode"

Inserting rather than rewriting keeps the other agent's entries intact; this file
is coordination, not a lock, so read the latest copy before appending.
"""

import argparse
import datetime
import io
import os
import re
import textwrap

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
HANDOFF = os.path.join(ROOT, "AGENT_HANDOFF.md")

SECTIONS = {
    "messages": "## Messages and handoffs",
    "findings": "## Audit findings",
    "checks": "## Shared checks",
    "ownership": "## Task ownership",
}


def render(agent, tag, msg_id, body, date):
    head = "- {0} / {1} / {2}".format(date, agent, tag)
    if msg_id:
        head += " " + msg_id
    text = "{0}: {1}".format(head, " ".join(body.split()))
    wrapped = textwrap.wrap(text, width=88, subsequent_indent="  ", break_long_words=False)
    return "\n".join(wrapped)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--agent", required=True, help="Claude, Codex, or User-to-<agent>")
    parser.add_argument("--tag", required=True, help="uppercase tag, e.g. STATUS or BUG")
    parser.add_argument("--text", required=True, help="entry body")
    parser.add_argument("--id", dest="msg_id", help="optional message id")
    parser.add_argument("--date", help="YYYY-MM-DD; defaults to today")
    parser.add_argument(
        "--section", default="messages", choices=sorted(SECTIONS), help="target section"
    )
    args = parser.parse_args()

    date = args.date or datetime.date.today().isoformat()
    entry = render(args.agent, args.tag.upper(), args.msg_id, args.text, date)

    with io.open(HANDOFF, encoding="utf-8") as handle:
        text = handle.read()

    heading = SECTIONS[args.section]
    if heading not in text:
        # New section: place it before Shared checks when that exists, else append.
        block = "{0}\n\n{1}\n\n".format(heading, entry)
        anchor = SECTIONS["checks"]
        if anchor in text and anchor != heading:
            text = text.replace(anchor, block + anchor, 1)
        else:
            text = text.rstrip("\n") + "\n\n" + block
    else:
        head, rest = text.split(heading, 1)
        match = re.search(r"\n## ", rest)
        if match:
            body, tail = rest[: match.start()], rest[match.start():]
        else:
            body, tail = rest, ""
        body = body.rstrip("\n") + "\n" + entry + "\n"
        text = head + heading + body + tail

    with io.open(HANDOFF, "w", encoding="utf-8", newline="\n") as handle:
        handle.write(text)

    print("appended to {0}:".format(heading))
    print(entry)


if __name__ == "__main__":
    raise SystemExit(main())
