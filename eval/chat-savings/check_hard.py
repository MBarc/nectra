"""Verdict for the hard workload (script-hard.txt): python check_hard.py REPO

Independent of the agent's tests. Each spec in the script gets its own probe.
"""
import json
import pathlib
import subprocess
import sys
import tempfile

PROBE = r"""
import json, invoice.calc as m
f = getattr(m, "calculate_total", None) or getattr(m, "calc")
I = lambda p, q=1, c=None: dict(name="x", price=p, qty=q, **({"category": c} if c else {}))
base = [I(2.5, 4), I(5.0)]
out = {
 "base": [f(base, 0), f(base, 10), f(base, 10, discount=0.2)],
 "round": [f([I(2.675)], 0), f([I(1.005)], 0)],
 "float": isinstance(f(base, 10), float),
 "food": f([I(100, 1, "food"), I(100)], 10),
 "exempt": f([I(100, 1, "exempt"), I(100)], 10),
 "share": f([I(100, 1, "food"), I(100)], 10, discount=0.5),
 "share_unequal": f([I(100, 1, "food"), I(300)], 10, discount=0.5),
}
print(json.dumps(out))
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
        p = json.loads(probe.stdout)
    except ValueError:
        p = {}
    c["tax_fixed"] = p.get("base") == [15.0, 16.5, 13.2]
    c["half_up_rounding"] = p.get("round") == [2.68, 1.01]
    c["returns_float"] = p.get("float") is True
    c["food_half_rate"] = p.get("food") == 215.0
    c["exempt_untaxed"] = p.get("exempt") == 210.0
    # Unequal lines tell proportional sharing (217.5) from an equal split (220.0).
    c["discount_shared"] = p.get("share") == 107.5 and p.get("share_unequal") == 217.5
    with tempfile.TemporaryDirectory() as d:
        good = pathlib.Path(d, "good.csv")
        good.write_text("name,price,qty\npen,2.5,4\npad,5.0,1\n", encoding="utf-8")
        cat = pathlib.Path(d, "cat.csv")
        cat.write_text("name,price,qty,category\nbread,100,1,food\ntv,100,1,\n", encoding="utf-8")
        bad = pathlib.Path(d, "bad.csv")
        bad.write_text("name,price,qty\npen,2.5,0\n", encoding="utf-8")
        r = py(repo, "-m", "invoice.cli", str(good), "--tax", "10", "--csv")
        c["csv_reads"] = r.returncode == 0 and "16.50" in r.stdout
        r = py(repo, "-m", "invoice.cli", str(cat), "--tax", "10", "--csv")
        c["csv_category"] = r.returncode == 0 and "215.00" in r.stdout
        neg = pathlib.Path(d, "neg.csv")
        neg.write_text("name,price,qty\npen,2.5,4\npad,5.0,-1\n", encoding="utf-8")
        rejected = [py(repo, "-m", "invoice.cli", str(f), "--tax", "10", "--csv") for f in (bad, neg)]
        c["csv_rejects_bad_qty"] = all(
            r.returncode == 2 and "Traceback" not in r.stderr and len(r.stderr.strip().splitlines()) == 1
            for r in rejected)
    r = py(repo, "-m", "invoice.cli", "items.json", "--tax", "10")
    c["json_input_kept"] = r.returncode == 0 and "16.50" in r.stdout
    tests = py(repo, "-m", "unittest", "-v")
    c["tests_pass"] = tests.returncode == 0 and tests.stderr.count(" ... ok") >= 6
    c["core"] = all(c[k] for k in ("tax_fixed", "half_up_rounding", "food_half_rate",
                                   "csv_reads", "tests_pass"))
    c["score"] = sum(v for k, v in c.items() if k != "core")
    return c


def main(repo):
    try:
        result = check(pathlib.Path(repo))
    except Exception as e:  # a broken repo is a result, not a crash
        result = {"error": f"{type(e).__name__}: {e}", "core": False, "score": 0}
    print(json.dumps(result))


if __name__ == "__main__":
    main(sys.argv[1])
