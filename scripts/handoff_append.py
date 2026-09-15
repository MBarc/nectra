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
import contextlib
import datetime
import io
import os
import re
import tempfile
import textwrap
import time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
HANDOFF = os.path.join(ROOT, "AGENT_HANDOFF.md")
LOCK = os.path.join(ROOT, ".collab", "handoff.lock")

if os.name == "nt":
    import msvcrt

    def _lock(handle):
        msvcrt.locking(handle.fileno(), msvcrt.LK_LOCK, 1)

    def _unlock(handle):
        handle.seek(0)
        msvcrt.locking(handle.fileno(), msvcrt.LK_UNLCK, 1)
else:
    import fcntl

    def _lock(handle):
        fcntl.flock(handle.fileno(), fcntl.LOCK_EX)

    def _unlock(handle):
        fcntl.flock(handle.fileno(), fcntl.LOCK_UN)


@contextlib.contextmanager
def exclusive(timeout=20.0):
    """Hold a real OS lock while read-modify-write runs.

    Both agents write this file concurrently. A PID lockfile can go stale and
    strand the other agent, so this takes an advisory lock the OS releases on
    process exit -- the same choice the Nectra journal makes.
    """
    os.makedirs(os.path.dirname(LOCK), exist_ok=True)
    handle = open(LOCK, "a+b")
    handle.seek(0)
    deadline = time.time() + timeout
    while True:
        try:
            _lock(handle)
            break
        except OSError:
            if time.time() >= deadline:
                handle.close()
                raise SystemExit(
                    "could not lock {0} within {1:.0f}s; another writer is stuck".format(
                        LOCK, timeout
                    )
                )
            time.sleep(0.1)
    try:
        yield
    finally:
        try:
            _unlock(handle)
        except OSError:
            pass
        handle.close()


def write_atomic(path, text):
    """Replace the file in one step so a reader never sees a truncated handoff."""
    directory = os.path.dirname(path) or "."
    handle, temp = tempfile.mkstemp(dir=directory, prefix=".handoff-", suffix=".tmp")
    try:
        with io.open(handle, "w", encoding="utf-8", newline="\n") as out:
            out.write(text)
            out.flush()
            os.fsync(out.fileno())
        os.replace(temp, path)
    except BaseException:
        with contextlib.suppress(OSError):
            os.unlink(temp)
        raise

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

    with exclusive():
        _append(args.section, entry)

    print("appended to {0}:".format(SECTIONS[args.section]))
    print(entry)


def _append(section, entry):
    """Read, insert, and replace. Must run under `exclusive()`."""
    with io.open(HANDOFF, encoding="utf-8") as handle:
        text = handle.read()

    heading = SECTIONS[section]
    heading_match = re.search(r"^" + re.escape(heading) + r"$", text, re.MULTILINE)
    if heading_match is None:
        # New section: place it before Shared checks when that exists, else append.
        block = "{0}\n\n{1}\n\n".format(heading, entry)
        anchor = SECTIONS["checks"]
        if anchor in text and anchor != heading:
            text = text.replace(anchor, block + anchor, 1)
        else:
            text = text.rstrip("\n") + "\n\n" + block
    else:
        head, rest = text[:heading_match.start()], text[heading_match.end():]
        match = re.search(r"\n## ", rest)
        if match:
            body, tail = rest[: match.start()], rest[match.start():]
        else:
            body, tail = rest, ""
        body = body.rstrip("\n") + "\n" + entry + "\n"
        text = head + heading + body + tail

    write_atomic(HANDOFF, text)


if __name__ == "__main__":
    raise SystemExit(main())
