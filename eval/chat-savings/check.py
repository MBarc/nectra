"""Did the chat get the work done? Prints a JSON verdict for one run's repo.

Usage: python check.py REPO
"""
import json
import pathlib
import subprocess
import sys


def run(repo, *args):
    return subprocess.run(
        [sys.executable, *args], cwd=repo, capture_output=True, text=True, timeout=120
    )


def main(repo):
    repo = pathlib.Path(repo)
    tests = run(repo, "-m", "unittest", "-v")
    ran = tests.stderr.count(" ... ")
    cli = run(repo, "-m", "invoice.cli", "items.json", "--tax", "10", "--json")
    try:
        json_ok = json.loads(cli.stdout)["total"] == 16.5
    except (ValueError, KeyError, TypeError):
        json_ok = False
    sources = "".join(
        p.read_text(encoding="utf-8") for p in repo.rglob("*.py") if "__pycache__" not in p.parts
    )
    checks = {
        "tests_pass": tests.returncode == 0,
        "json_test_added": ran >= 4,
        "json_flag_works": json_ok,
        "renamed": "def calculate_total(" in sources and "calc(" not in sources.replace("calculate_total(", ""),
        "readme_documents_json": "--json" in (repo / "README.md").read_text(encoding="utf-8"),
    }
    checks["score"] = sum(checks.values())
    print(json.dumps(checks))


if __name__ == "__main__":
    main(sys.argv[1])
