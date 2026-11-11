#!/usr/bin/env python3
"""Stub `codex` for the gate dry run: never contacts any service.

Accepts the flags nectra passes (`exec --model M [--config k=v] [--json] -- -`), reads the grading prompt from stdin and
answers with a valid `--json` event stream whose agent message is the grade JSON calibrate.py expects (every output gets
full points on every rubric item, derived from the prompt's own `[N pts]` lines and `Labels:` line).
"""
import json, re, sys

args = sys.argv[1:]
if "--version" in args:
    print("codex-cli 0.166.0 (stub)"); sys.exit(0)
if not args or args[0] != "exec":
    print("stub codex: only `exec` is supported", file=sys.stderr); sys.exit(2)
if "-" not in args:
    print("stub codex: expected '-' (prompt on stdin) after the flags", file=sys.stderr); sys.exit(2)
prompt = sys.stdin.read()
pts = [int(x) for x in re.findall(r"^\d+\. \[(\d+) pts\]", prompt, re.M)]
labels = re.search(r"Labels: ([A-Z, ]+)\.", prompt)
if not pts or not labels:
    print("stub codex: prompt is not a grading prompt", file=sys.stderr); sys.exit(3)
reply = {l.strip(): {"points": pts, "note": "STUB GRADE (dry run)"} for l in labels.group(1).split(",")}
for ev in ({"type": "thread.started", "thread_id": "stub"}, {"type": "turn.started"},
           {"type": "item.completed", "item": {"id": "i0", "type": "agent_message", "text": json.dumps(reply)}},
           {"type": "turn.completed", "usage": {"input_tokens": 1, "cached_input_tokens": 0, "output_tokens": 1}}):
    print(json.dumps(ev), flush=True)
