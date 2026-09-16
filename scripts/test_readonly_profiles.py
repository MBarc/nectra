"""Probe a Codex Windows sandbox with disposable files, without a model call.

Pass --codex with the native executable path. A pass requires successful reads,
explicit permission denial for every mutation, and an unchanged directory tree.
Startup/configuration errors are failures, never evidence of write denial.
"""
import argparse
import contextlib
import hashlib
import json
import os
from pathlib import Path
import subprocess
import shutil
import sys
import uuid


PROBE = r'''
import errno, json
from pathlib import Path
root = Path.cwd()
result = {"read": (root / "existing.txt").read_text() == "original", "operations": {}}
operations = {
    "create": lambda: (root / "created.txt").write_text("changed"),
    "overwrite": lambda: (root / "existing.txt").write_text("changed"),
    "delete": lambda: (root / "delete.txt").unlink(),
    "rename": lambda: (root / "rename.txt").rename(root / "renamed.txt"),
    "mkdir": lambda: (root / "created-dir").mkdir(),
}
for name, action in operations.items():
    try:
        action()
        result["operations"][name] = "allowed"
    except OSError as error:
        result["operations"][name] = "denied" if error.errno in (errno.EACCES, errno.EPERM) else repr(error)
print(json.dumps(result))
'''


def populate(root):
    for name in ["existing.txt", "delete.txt", "rename.txt"]:
        (root / name).write_text("original", encoding="utf-8")


def snapshot(root):
    return {str(p.relative_to(root)): hashlib.sha256(p.read_bytes()).hexdigest()
            if p.is_file() else "directory" for p in root.rglob("*")}


def run_probe(command, cwd, env):
    result = subprocess.run(command, cwd=cwd, env=env, capture_output=True,
                            text=True, timeout=30)
    if result.returncode:
        raise RuntimeError(f"probe failed to run ({result.returncode}): {result.stderr}")
    return json.loads(result.stdout.strip())


@contextlib.contextmanager
def fixture_directory(base):
    # TemporaryDirectory uses a private Windows ACL. Inherit normal workspace
    # read permissions so that the sandbox can read the probe before denying writes.
    root = base / ("readonly-probe-" + uuid.uuid4().hex)
    root.mkdir()
    try:
        yield root
    finally:
        if root.resolve().parent != base.resolve():
            raise RuntimeError("refusing cleanup outside fixture parent")
        shutil.rmtree(root)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--codex", required=True, type=Path)
    args = parser.parse_args()
    if os.name != "nt":
        parser.error("this fixture currently verifies the Windows sandbox only")
    executable = args.codex.resolve(strict=True)
    base = Path(__file__).resolve().parent.parent / ".collab"
    base.mkdir(exist_ok=True)
    with fixture_directory(base) as directory:
        root = Path(directory)
        assert root.resolve().parent == base.resolve()
        control, protected = [root / name for name in ["control", "protected"]]
        for path in [control, protected]:
            path.mkdir()
        populate(control)
        populate(protected)
        # Reuse the installed Windows sandbox identity/setup. The explicit
        # built-in permission profile overrides ordinary workspace permissions.
        env = dict(os.environ)
        probe = [sys.executable, "-I", "-c", PROBE]
        baseline = run_probe(probe, control, env)
        if not baseline["read"] or set(baseline["operations"].values()) != {"allowed"}:
            raise RuntimeError(f"unrestricted control failed: {baseline}")
        before = snapshot(protected)
        command = [str(executable), "sandbox", "--permission-profile", ":read-only",
                   "-C", str(protected), "--", *probe]
        restricted = run_probe(command, protected, env)
        passed = (restricted["read"] and
                  restricted["operations"].keys() == baseline["operations"].keys() and
                  set(restricted["operations"].values()) == {"denied"} and
                  snapshot(protected) == before)
        print(json.dumps({"passed": passed, "control": baseline, "sandbox": restricted}, indent=2))
        return 0 if passed else 1


if __name__ == "__main__":
    raise SystemExit(main())
