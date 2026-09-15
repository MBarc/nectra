#!/usr/bin/env python3
"""Send a message to the live Codex session and record it for the monitor.

One step instead of two, so the verbatim payload never drifts from the summary
that lands in AGENT_HANDOFF.md.

    python scripts/send_to_codex.py --tag SPLIT --text "..."

`--tag` and `--date` are what the monitor uses to pair this payload with the
matching AGENT_HANDOFF.md entry, so keep them identical to the entry you write.
"""

import argparse
import datetime
import json
import os
import subprocess
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
MESSAGES = os.path.join(ROOT, ".collab", "messages.jsonl")
THREAD_FILE = os.path.join(ROOT, ".collab", "codex-thread")


def resolve_thread(explicit):
    if explicit:
        return explicit
    env = os.environ.get("CODEX_THREAD")
    if env:
        return env
    try:
        with open(THREAD_FILE, encoding="utf-8") as handle:
            value = handle.read().strip()
        if value:
            return value
    except OSError:
        pass
    sys.exit(
        "no thread id: pass --thread, set CODEX_THREAD, or write one to "
        ".collab/codex-thread"
    )


def log(record):
    os.makedirs(os.path.dirname(MESSAGES), exist_ok=True)
    with open(MESSAGES, "a", encoding="utf-8") as handle:
        handle.write(json.dumps(record) + "\n")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--text", required=True, help="message body")
    parser.add_argument("--tag", required=True, help="tag, matching the handoff entry")
    parser.add_argument(
        "--id",
        dest="msg_id",
        help="message id; the monitor pairs the payload with the handoff entry by this",
    )
    parser.add_argument("--thread", help="Codex session UUID or exact session name")
    parser.add_argument("--date", help="YYYY-MM-DD; defaults to today")
    parser.add_argument(
        "--log-only",
        action="store_true",
        help="record the payload without queueing it (for backfill)",
    )
    args = parser.parse_args()

    thread = resolve_thread(args.thread)
    date = args.date or datetime.date.today().isoformat()
    record = {
        "ts": datetime.datetime.now().isoformat(timespec="seconds"),
        "date": date,
        "from": "Claude",
        "to": "Codex",
        "tag": args.tag,
        "id": args.msg_id,
        "via": "codex queue",
        "thread": thread,
        "text": args.text,
    }

    if args.log_only:
        record["queued"] = None
        log(record)
        print("logged only (not queued)")
        return 0

    result = subprocess.run(
        ["codex", "queue", "--thread", thread, "--message", args.text],
        capture_output=True,
        text=True,
        shell=os.name == "nt",
    )
    output = (result.stdout or "").strip() or (result.stderr or "").strip()
    record["queued"] = output
    record["exit_code"] = result.returncode
    log(record)

    print(output)
    if result.returncode != 0:
        print("queue failed; payload was still logged", file=sys.stderr)
    return result.returncode


if __name__ == "__main__":
    raise SystemExit(main())
