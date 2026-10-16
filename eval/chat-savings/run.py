"""Drive one scripted chat through Nectra and record what it cost and achieved.

    python run.py OUT_DIR LABEL [--model ID] [--jev] [--script FILE] [--nectra PATH]

Each run gets a fresh copy of fixture/ as its own git repo, a private config
(the bundled registry; the keyword classifier unless --jev) and its own
session file, so `nectra usage --session` counts this chat alone. Tools may
edit the copy: Claude runs with acceptEdits and Python-only Bash, Codex with
the workspace-write sandbox. result.json is always written, with `valid`
saying whether the run measured what it claims to.
"""
import argparse
import json
import os
import pathlib
import shutil
import subprocess
import sys
import time

HERE = pathlib.Path(__file__).resolve().parent
ROOT = HERE.parent.parent
DEADLINE = 60 * 60

CONFIG = """registry = "models.toml"
providers = ["anthropic", "openai"]

[claude]
command = "claude"
extra_args = ["--permission-mode", "acceptEdits", "--allowedTools", "Bash(python:*)"]

[codex]
command = "codex"
extra_args = ["--sandbox", "workspace-write"]

[routing]
confidence_margin = 0.035
default_mode = "balanced"

[classifier]
backend = "{backend}"
"""


def output(*cmd):
    try:
        exe = shutil.which(cmd[0]) or cmd[0]
        return subprocess.run([exe, *cmd[1:]], capture_output=True, text=True,
                              timeout=60).stdout.strip()
    except (OSError, subprocess.TimeoutExpired) as e:
        return f"unavailable: {e}"


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("out")
    ap.add_argument("label")
    ap.add_argument("--model")
    ap.add_argument("--mode", choices=["fast", "balanced", "best"])
    ap.add_argument("--jev", action="store_true", help="classify with Jev (needs TYPESAFE_API_KEY)")
    ap.add_argument("--workload", choices=["light", "hard", "expert"], default="light")
    ap.add_argument("--script", help="override the workload's script")
    ap.add_argument("--nectra", default=str(ROOT / "target/release/nectra.exe"))
    a = ap.parse_args()
    suffix = "" if a.workload == "light" else f"-{a.workload}"
    a.script = a.script or str(HERE / f"script{suffix}.txt")
    checker = HERE / ("check.py" if a.workload == "light" else f"check_{a.workload}.py")
    run = pathlib.Path(a.out).resolve() / a.label
    if run.exists():
        sys.exit(f"{run} exists; pick a new label")
    env = dict(os.environ)
    if not a.jev:
        env.pop("TYPESAFE_API_KEY", None)
    elif not env.get("TYPESAFE_API_KEY"):
        sys.exit("--jev needs TYPESAFE_API_KEY")
    repo, cfg = run / "repo", run / "config"
    shutil.copytree(HERE / "fixture", repo)
    git = ["git", "-c", "user.name=bench", "-c", "user.email=bench@example.invalid"]
    subprocess.run(["git", "init", "-q"], cwd=repo, check=True)
    subprocess.run(["git", "add", "-A"], cwd=repo, check=True)
    subprocess.run([*git, "commit", "-qm", "fixture"], cwd=repo, check=True)
    cfg.mkdir()
    (cfg / "config.toml").write_text(
        CONFIG.format(backend="auto" if a.jev else "local"), encoding="utf-8")
    shutil.copy(ROOT / "config/models.toml", cfg / "models.toml")
    nectra = [a.nectra, "--config", str(cfg / "config.toml"), "--provider-view", "stream"]
    if a.model:
        nectra += ["--model", a.model]
    if a.mode:
        nectra += ["--mode", a.mode]
    script = [l for l in pathlib.Path(a.script).read_text(encoding="utf-8").splitlines()
              if l.strip() and not l.startswith("/")]
    session = run / "session.jsonl"
    cooldowns = pathlib.Path(os.environ["LOCALAPPDATA"]) / "nectra/cooldowns.json"
    started = time.monotonic()
    with open(a.script, "rb") as stdin, open(run / "transcript.txt", "wb") as out:
        try:
            code = subprocess.run([*nectra, "chat", "--session", str(session)], cwd=repo,
                                  stdin=stdin, stdout=out, stderr=subprocess.STDOUT,
                                  env=env, timeout=DEADLINE).returncode
        except subprocess.TimeoutExpired:
            code = "timeout"
    seconds = round(time.monotonic() - started)
    events = [json.loads(l) for l in session.read_text(encoding="utf-8").splitlines()] \
        if session.exists() else []
    attempts = [e["model"] for e in events if e["type"] == "user"]
    answered = sum(e["type"] == "assistant" for e in events)
    failed = sum(e["type"] == "failed" for e in events)
    try:
        usage = json.loads(output(a.nectra, "usage", "--session", str(session), "--json"))
    except ValueError:
        usage = {}
    try:
        check = json.loads(output(sys.executable, str(checker), str(repo)))
    except ValueError:
        check = {"error": "checker produced no verdict", "core": False, "score": 0}
    transcript = (run / "transcript.txt").read_text(encoding="utf-8", errors="replace")
    problems = []
    if code != 0:
        problems.append(f"chat exited {code}")
    if answered != len(script):
        problems.append(f"{answered} of {len(script)} messages answered")
    if failed:
        problems.append(f"{failed} failed attempts")
    if a.model and any(m != a.model for m in attempts):
        problems.append("pin not honoured")
    if "rate limited" in transcript:
        problems.append("rate limited")
    if not usage or any(u.get("turns_without_data") for u in usage.values()):
        problems.append("some turns have no token data")
    result = {
        "label": a.label, "workload": a.workload, "pinned": a.model, "mode": a.mode, "jev": a.jev, "seconds": seconds,
        "valid": not problems, "problems": problems,
        "routes": attempts, "check": check, "usage": usage,
        "versions": {"nectra": output(a.nectra, "--version"),
                     "claude": output("claude", "--version"),
                     "codex": output("codex", "--version")},
        "cooldowns_after": cooldowns.read_text(encoding="utf-8") if cooldowns.exists() else None,
    }
    (run / "result.json").write_text(json.dumps(result, indent=2), encoding="utf-8")
    print(json.dumps(result, indent=2))


if __name__ == "__main__":
    main()
