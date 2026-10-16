"""Did the chat get the work done? Prints a JSON verdict for one run's repo.

Usage: python check.py REPO

Behaviour is checked independently of the tests the agent wrote, which it
could have weakened. Every check reports separately; `core` is the three that
matter most (bug fixed, --json works, suite green).
"""
import ast
import json
import pathlib
import subprocess
import sys

PROBE = r"""
import json, invoice.calc as m
f = getattr(m, "calculate_total", None) or getattr(m, "calc")
items = [{"name": "pen", "price": 2.5, "qty": 4}, {"name": "pad", "price": 5.0, "qty": 1}]
print(json.dumps([f(items, 0), f(items, 10), f(items, 10, discount=0.2),
                  f([{"name": "x", "price": 100, "qty": 1}], 7.25)]))
"""


def py(repo, *args):
    try:
        return subprocess.run([sys.executable, *args], cwd=repo, capture_output=True,
                              text=True, timeout=120)
    except subprocess.TimeoutExpired:
        return subprocess.CompletedProcess(args, 124, "", "timeout")


def check(repo):
    c = {}
    probe = py(repo, "-c", PROBE)
    try:
        c["tax_fixed"] = json.loads(probe.stdout) == [15.0, 16.5, 13.2, 107.25]
    except ValueError:
        c["tax_fixed"] = False
    text = py(repo, "-m", "invoice.cli", "items.json", "--tax", "10")
    c["text_output_kept"] = text.returncode == 0 and "16.50" in text.stdout
    out = py(repo, "-m", "invoice.cli", "items.json", "--tax", "10", "--json")
    try:
        c["json_flag_works"] = out.returncode == 0 and json.loads(out.stdout) == {"total": 16.5}
    except ValueError:
        c["json_flag_works"] = False
    tests = py(repo, "-m", "unittest", "-v")
    c["tests_pass"] = tests.returncode == 0 and tests.stderr.count(" ... ok") >= 3
    test_src = "".join(p.read_text(encoding="utf-8") for p in (repo / "tests").glob("*.py"))
    c["json_test_added"] = tests.stderr.count(" ... ok") >= 4 and "json" in test_src.lower()
    names, defs = set(), set()
    for p in repo.rglob("*.py"):
        if "__pycache__" in p.parts:
            continue
        for node in ast.walk(ast.parse(p.read_text(encoding="utf-8"))):
            if isinstance(node, ast.FunctionDef):
                defs.add(node.name)
            elif isinstance(node, ast.Name):
                names.add(node.id)
            elif isinstance(node, ast.Attribute):
                names.add(node.attr)
            elif isinstance(node, ast.alias):
                names.add(node.name)
    c["renamed"] = "calculate_total" in defs and "calc" not in defs | names
    c["readme_documents_json"] = "--json" in (repo / "README.md").read_text(encoding="utf-8")
    c["core"] = c["tax_fixed"] and c["json_flag_works"] and c["tests_pass"]
    c["score"] = sum(v for k, v in c.items() if k not in ("core",))
    return c


def main(repo):
    try:
        result = check(pathlib.Path(repo))
    except Exception as e:  # a broken repo is a result, not a crash
        result = {"error": f"{type(e).__name__}: {e}", "core": False, "score": 0}
    print(json.dumps(result))


if __name__ == "__main__":
    main(sys.argv[1])
