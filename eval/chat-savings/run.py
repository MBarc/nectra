"""Drive one scripted chat through Nectra and record what it cost and achieved.

    python run.py OUT_DIR LABEL [--model ID] [--nectra PATH]

Each run gets a fresh copy of fixture/ as its own git repo, a private config
(the bundled registry, no Jev key unless TYPESAFE_API_KEY is set) and its own
session file, so `nectra usage --session` counts this chat alone. Tools are
allowed to edit the copy but nothing outside it: Claude runs with acceptEdits
and Python-only Bash, Codex with the workspace-write sandbox.
"""
import argparse
import json
import pathlib
import shutil
import subprocess
import sys
import time

HERE = pathlib.Path(__file__).resolve().parent
ROOT = HERE.parent.parent

CONFIG = """registry = "models.toml"
providers = ["anthropic", "openai"]

[claude]
command = "claude"
extra_args = ["--permission-mode", "acceptEdits", "--allowedTools", "Bash(python:*)"]

[codex]
command = "codex"
extra_args = ["-c", "sandbox_mode=\\"workspace-write\\""]

[routing]
default_mode = "balanced"
"""


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("out")
    ap.add_argument("label")
    ap.add_argument("--model")
    ap.add_argument("--nectra", default=str(ROOT / "target/release/nectra.exe"))
    a = ap.parse_args()
    run = pathlib.Path(a.out).resolve() / a.label
    if run.exists():
        sys.exit(f"{run} exists; pick a new label")
    repo, cfg = run / "repo", run / "config"
    shutil.copytree(HERE / "fixture", repo)
    git = ["git", "-c", "user.name=bench", "-c", "user.email=bench@example.invalid"]
    subprocess.run(["git", "init", "-q"], cwd=repo, check=True)
    subprocess.run(["git", "add", "-A"], cwd=repo, check=True)
    subprocess.run([*git, "commit", "-qm", "fixture"], cwd=repo, check=True)
    cfg.mkdir()
    (cfg / "config.toml").write_text(CONFIG, encoding="utf-8")
    shutil.copy(ROOT / "config/models.toml", cfg / "models.toml")
    nectra = [a.nectra, "--config", str(cfg / "config.toml"), "--provider-view", "stream"]
    if a.model:
        nectra += ["--model", a.model]
    session = run / "session.jsonl"
    started = time.monotonic()
    with open(HERE / "script.txt", "rb") as script, open(run / "transcript.txt", "wb") as out:
        code = subprocess.run(
            [*nectra, "chat", "--session", str(session)],
            cwd=repo, stdin=script, stdout=out, stderr=subprocess.STDOUT,
        ).returncode
    seconds = time.monotonic() - started
    usage = json.loads(subprocess.run(
        [a.nectra, "usage", "--session", str(session), "--json"],
        capture_output=True, text=True, check=True,
    ).stdout)
    check = json.loads(subprocess.run(
        [sys.executable, str(HERE / "check.py"), str(repo)],
        capture_output=True, text=True, check=True,
    ).stdout)
    turns = [json.loads(l) for l in session.read_text(encoding="utf-8").splitlines()]
    routes = [t["model"] for t in turns if t["type"] == "user"]
    result = {"label": a.label, "pinned": a.model, "exit": code, "seconds": round(seconds),
              "routes": routes, "check": check, "usage": usage}
    (run / "result.json").write_text(json.dumps(result, indent=2), encoding="utf-8")
    print(json.dumps(result, indent=2))


if __name__ == "__main__":
    main()
