#!/usr/bin/env python3
"""Acceptance demo for `nectra run --events jsonl` against real providers.

Makes a throwaway git repo with a one-line bug, then:
  1. a routed headless run (fast mode) that fixes it,
  2. a --resume follow-up in the same provider session,
  3. a run stopped by --max-cost-usd, which must end in a typed failure.
Prints every JSONL event and checks each result event against schema 1
(docs/headless.md). Then checks the usage independently: against the
provider's own session log (Claude Code's ~/.claude/projects/<slug>/<id>.jsonl,
one entry per unique assistant message id; Codex's rollout), and the cost
against Claude Code's own total_cost_usd. Costs a few cents at most.

    python scripts/headless_demo.py [--nectra PATH] [--provider P] [--keep]
"""
import argparse, json, os, pathlib, shutil, subprocess, sys, tempfile

ROOT = pathlib.Path(__file__).resolve().parent.parent
EXE = "nectra.exe" if os.name == "nt" else "nectra"
USAGE = {"input", "output", "cache_read", "cache_write", "cache_write_5m"}
FLAGS = []
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
    need("usage_complete", bool)
    need("cost_usd", float, int, type(None))
    need("provider_cost_usd", float, int, type(None))
    need("warnings", list)
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
        if set(m.get("usage", {})) != USAGE or not {"model", "routed_as", "usage_complete"} <= set(m):
            problems.append(f"bad models entry {m}")
    # The total is the sum of the per-model entries.
    for k in USAGE:
        if sum(m["usage"][k] for m in r.get("models", [])) != r.get("usage", {}).get(k):
            problems.append(f"usage.{k} is not the sum over models")
    if r.get("is_error") != (r.get("error_class") is not None):
        problems.append("is_error disagrees with error_class")
    return problems


def claude_log(session):
    """Usage per unique assistant message id in Claude Code's own session log.
    Claude Code writes a message's usage on each of its content blocks; the
    counters only grow, so each field's largest value is the message's."""
    home = pathlib.Path(os.environ.get("CLAUDE_CONFIG_DIR") or pathlib.Path.home() / ".claude")
    logs = list((home / "projects").glob(f"*/{session}.jsonl"))
    assert len(logs) == 1, f"Claude Code log for {session}: {logs}"
    messages = {}
    for line in logs[0].read_text(encoding="utf-8").splitlines():
        e = json.loads(line)
        u = e.get("message", {}).get("usage")
        if e.get("type") != "assistant" or not u:
            continue
        split = u.get("cache_creation") or {}
        now = {"input": u.get("input_tokens", 0), "cache_read": u.get("cache_read_input_tokens", 0),
               "cache_write": u.get("cache_creation_input_tokens", 0),
               "cache_write_5m": split.get("ephemeral_5m_input_tokens", 0),
               "output": u.get("output_tokens", 0)}
        old = messages.setdefault(e["message"]["id"], now)
        for k in now:
            old[k] = max(old[k], now[k])
    return messages


def codex_log(thread):
    """The thread's running total after the last call in Codex's own rollout."""
    home = pathlib.Path(os.environ.get("CODEX_HOME") or pathlib.Path.home() / ".codex")
    logs = list((home / "sessions").rglob(f"rollout-*-{thread}.jsonl"))
    assert len(logs) == 1, f"Codex log for {thread}: {logs}"
    total = {}
    for line in logs[0].read_text(encoding="utf-8").splitlines():
        p = json.loads(line).get("payload", {})
        if p.get("type") == "token_count" and p.get("info"):
            t = p["info"]["total_token_usage"]
            total = {"input": t["input_tokens"] - t.get("cached_input_tokens", 0),
                     "cache_read": t.get("cached_input_tokens", 0), "cache_write": 0,
                     "cache_write_5m": 0, "output": t["output_tokens"]}
    return total


def provider_usage(state, r, before):
    """This task's usage by the provider's own log, given the log's state
    before it (None for a new session). Returns (usage, state now)."""
    record = json.loads((state / "runs" / f"{r['session_id']}.json").read_text())
    if record["runtime"] == "claude":
        messages = claude_log(record["provider_session_id"])
        new = [m for i, m in messages.items() if i not in (before or {})]
        return {k: sum(m[k] for m in new) for k in USAGE}, messages
    total = codex_log(record["provider_session_id"])
    return {k: total[k] - (before or {}).get(k, 0) for k in USAGE}, total


def cross_check(state, r, before, label):
    """Nectra's usage must equal the provider's log exactly."""
    theirs, now = provider_usage(state, r, before)
    print(f"--- {label}: Nectra {r['usage']}")
    print(f"    provider log {theirs}")
    assert r["usage"] == theirs, "usage differs from the provider's own log"
    assert r["usage_complete"], "a finished task's usage is complete"
    return now


def run(nectra, repo, state, args, label):
    cmd = [str(nectra), *FLAGS, "--mode", "fast", "--dangerously-skip-permissions", "run",
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
    ap.add_argument("--provider", help="route within one provider (anthropic, openai)")
    a = ap.parse_args()
    if a.provider:
        FLAGS.extend(["--provider", a.provider])
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
        log = cross_check(state, r, None, "independent check, run 1")
        if r["provider_cost_usd"] is not None:
            print(f"    cost: Nectra {r['cost_usd']}, Claude Code {r['provider_cost_usd']}")
            assert abs(r["cost_usd"] - r["provider_cost_usd"]) < 1e-6, "cost differs from Claude Code's"

        events, r2 = run(a.nectra, repo, state, ["--resume", session,
            "Now add a one-line docstring to greet. Reply with one short sentence."],
            "2. --resume follow-up")
        assert not r2["is_error"], r2["error"]
        assert events[0]["resumed"] is True and r2["session_id"] == session
        assert r2["model"] == r["model"], "a resumed task stays on its model"
        assert '"""' in (repo / "greet.py").read_text() or "'''" in (repo / "greet.py").read_text()
        cross_check(state, r2, log, "independent check, --resume")
        if r2["provider_cost_usd"] is not None:
            # Claude Code's figure on a resumed session covers the whole session.
            both = r["cost_usd"] + r2["cost_usd"]
            print(f"    session cost: Nectra {both}, Claude Code {r2['provider_cost_usd']}")
            assert abs(both - r2["provider_cost_usd"]) < 1e-6, "session cost differs from Claude Code's"

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
