#!/usr/bin/env python3
"""Acceptance demo for `nectra run --events jsonl` against real providers.

Makes a throwaway git repo with a one-line bug, then:
  1. a routed headless run (fast mode) that fixes it,
  2. a --resume follow-up in the same provider session,
  3. a run stopped by --max-cost-usd, which must end in a typed failure.
Prints every JSONL event and checks each result event against schema 1
(docs/headless.md). Costs a few cents at most on the cheapest models.

    python scripts/headless_demo.py [--nectra PATH] [--keep]
"""
import argparse, json, os, pathlib, shutil, subprocess, sys, tempfile

ROOT = pathlib.Path(__file__).resolve().parent.parent
EXE = "nectra.exe" if os.name == "nt" else "nectra"
USAGE = {"input", "output", "cache_read", "cache_write"}
CLASSES = {None, "model_unavailable", "auth", "rate_limited", "harness_error", "no_model",
           "agent_error", "cost_limit", "deadline", "cancelled"}


def check_result(r):
    """Every field docs/headless.md promises, with its type."""
    problems = []
    def need(key, *types):
        if key not in r:
            problems.append(f"missing {key}")
        elif not isinstance(r[key], types):
            problems.append(f"{key} is {type(r[key]).__name__}")
    need("is_error", bool)
    need("error_class", str, type(None))
    need("error", str, type(None))
    need("text", str, type(None))
    need("session_id", str, type(None))
    need("model", str, type(None))
    need("usage", dict)
    need("cost_usd", float, int, type(None))
    need("models", list)
    need("attempts", list)
    need("route", dict, type(None))
    need("duration_ms", int)
    need("exit_code", int)
    if r.get("error_class") not in CLASSES:
        problems.append(f"unknown error_class {r.get('error_class')}")
    if set(r.get("usage", {})) != USAGE:
        problems.append(f"usage keys {sorted(r.get('usage', {}))}")
    for m in r.get("models", []):
        if set(m.get("usage", {})) != USAGE or "model" not in m or "routed_as" not in m:
            problems.append(f"bad models entry {m}")
    # The total is the sum of the per-model entries.
    for k in USAGE:
        if sum(m["usage"][k] for m in r.get("models", [])) != r.get("usage", {}).get(k):
            problems.append(f"usage.{k} is not the sum over models")
    if r.get("is_error") != (r.get("error_class") is not None):
        problems.append("is_error disagrees with error_class")
    return problems


def run(nectra, repo, state, args, label):
    cmd = [str(nectra), "--mode", "fast", "--dangerously-skip-permissions", "run",
           "--events", "jsonl", "--state-dir", str(state), *args]
    print(f"\n=== {label}\n$ nectra {' '.join(cmd[1:])}", flush=True)
    p = subprocess.run(cmd, cwd=repo, capture_output=True, text=True, encoding="utf-8")
    events = []
    for line in p.stdout.splitlines():
        print(line)
        events.append(json.loads(line))
    if p.stderr.strip():
        print("--- stderr\n" + p.stderr.strip())
    assert events, "no events"
    assert all(e.get("v") == 1 for e in events), "every event carries v=1"
    assert [e["type"] for e in events].count("result") == 1 and events[-1]["type"] == "result"
    r = events[-1]
    problems = check_result(r)
    assert not problems, problems
    assert r["exit_code"] == p.returncode, (r["exit_code"], p.returncode)
    print(f"--- exit {p.returncode}; schema ok; model {r['model']}; usage {r['usage']}; "
          f"cost_usd {r['cost_usd']}; session {r['session_id']}")
    return events, r


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--nectra", default=ROOT / "target" / "debug" / EXE)
    ap.add_argument("--keep", action="store_true", help="keep the temp repo")
    a = ap.parse_args()
    tmp = pathlib.Path(tempfile.mkdtemp(prefix="nectra-headless-"))
    repo, state = tmp / "repo", tmp / "state"
    repo.mkdir()
    (repo / "greet.py").write_text('def greet(name):\n    return "Helo, " + name\n')
    git = lambda *x: subprocess.run(["git", *x], cwd=repo, check=True, capture_output=True)
    git("init", "-q")
    git("add", ".")
    git("-c", "user.name=demo", "-c", "user.email=demo@example.invalid", "commit", "-qm", "init")
    try:
        events, r = run(a.nectra, repo, state, [
            "Fix the spelling mistake in the greeting string in greet.py. Edit only that line. "
            "Reply with one short sentence."], "1. routed headless run")
        assert not r["is_error"], r["error"]
        assert events[0]["type"] == "route"
        assert "Hello" in (repo / "greet.py").read_text(), "the fix landed"
        session = r["session_id"]
        assert session, "a resumable session id"

        events, r2 = run(a.nectra, repo, state, ["--resume", session,
            "Now add a one-line docstring to greet. Reply with one short sentence."],
            "2. --resume follow-up")
        assert not r2["is_error"], r2["error"]
        assert events[0]["resumed"] is True and r2["session_id"] == session
        assert r2["model"] == r["model"], "a resumed task stays on its model"
        assert '"""' in (repo / "greet.py").read_text() or "'''" in (repo / "greet.py").read_text()

        events, r3 = run(a.nectra, repo, state, ["--max-cost-usd", "0.0001",
            "Write a docstring for every function in greet.py."], "3. cost guard stop")
        assert r3["error_class"] == "cost_limit" and r3["exit_code"] == 4, r3
        print("\n--- final greet.py\n" + (repo / "greet.py").read_text())
        print("ALL CHECKS PASSED")
    finally:
        if a.keep:
            print(f"kept {tmp}")
        else:
            shutil.rmtree(tmp, ignore_errors=True)


if __name__ == "__main__":
    sys.exit(main())
