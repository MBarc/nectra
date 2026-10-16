"""Verdict for the expert workload (script-expert.txt): python check_expert.py REPO

Probes optimize_coupons with fixed cases (from the design review, confirmed by
the reference solver below) and random dense cases compared against that
reference, which greedy assignment fails. Each call has a 10 s limit.
"""
import json
import pathlib
import random
import subprocess
import sys

def hungarian_max(w):
    # rectangular n x k (n<=k) maximisation via min-cost on negated weights (classic O(n^2 k))
    n, k = len(w), len(w[0])
    INF = float('inf')
    u = [0]*(n+1); v = [0]*(k+1); p = [0]*(k+1); way = [0]*(k+1)
    for i in range(1, n+1):
        p[0] = i; j0 = 0; minv = [None]*(k+1); used = [False]*(k+1)
        while True:
            used[j0] = True; i0 = p[j0]; delta = None; j1 = None
            for j in range(1, k+1):
                if not used[j]:
                    cur = -w[i0-1][j-1] - u[i0] - v[j]
                    if minv[j] is None or cur < minv[j]:
                        minv[j] = cur; way[j] = j0
                    if delta is None or minv[j] < delta:
                        delta = minv[j]; j1 = j
            for j in range(k+1):
                if used[j]:
                    u[p[j]] += delta; v[j] -= delta
                else:
                    minv[j] -= delta
            j0 = j1
            if p[j0] == 0: break
        while True:
            j1 = way[j0]; p[j0] = p[j1]; j0 = j1
            if j0 == 0: break
    res = [None]*n
    for j in range(1, k+1):
        if p[j]: res[p[j]-1] = j-1
    return res

def solve(lines, coupons):
    n, m = len(lines), len(coupons)
    if n == 0: return (0, [])
    B = m + 1
    tie = lambda i, j: (m - j) * B ** (n - 1 - i)
    K = B ** n * 2
    NEG = -(10**40) * K
    w = []
    for i in range(n):
        row = []
        for j, (el, bps, cap) in enumerate(coupons):
            s = min(lines[i] * bps // 10000, cap) if i in el else 0
            row.append(s * K + tie(i, j) if s > 0 else NEG)
        row += [tie(i, m)] * n  # unused slots
        w.append(row)
    a = hungarian_max(w)
    asg = [j if j < m else None for j in a]
    total = sum(min(lines[i] * coupons[j][1] // 10000, coupons[j][2]) for i, j in enumerate(asg) if j is not None)
    return (total, asg)

C = lambda e, b, c: (e, b, c)
cases = [
 ([], [], (0, [])),
 ([100], [], (0, [None])),
 ([100,99], [C([0,1],10000,100), C([0],9900,100)], (198,[1,0])),
 ([100,100], [C([0,1],10000,100)], (100,[0,None])),
 ([100,100], [C([0,1],10000,100)]*2, (200,[0,1])),
 ([101,99], [C([0,1],3333,100), C([0],10000,20)], (52,[1,0])),
 ([9007199254740993], [C([0],10000,10**18)], (9007199254740993,[0])),
 ([0,1], [C([0,1],1,100)], (0,[None,None])),
 ([1]*80, [C(list(range(80-j)),10000,1) for j in range(80)], (80, list(range(79,-1,-1)))),
 ([1]*80, [C(list(range(80)),10000,1)]*80, (80, list(range(80)))),
]


def random_cases():
    rnd = random.Random(11)
    out = []
    for n, m in [(12, 12), (25, 30), (40, 40), (80, 80), (80, 60)]:
        lines = [rnd.randint(0, 10**6) for _ in range(n)]
        coupons = [(rnd.sample(range(n), rnd.randint(1, n)), rnd.choice([0, 500, 1000, 2500, 3333, 5000, 10000]),
                    rnd.randint(0, 10**5)) for _ in range(m)]
        out.append((lines, coupons, solve(lines, coupons)))
    return out


PROBE = r"""
import copy, json, sys, time
from invoice.calc import optimize_coupons
lines, coupons = json.loads(sys.stdin.read())
coupons = [tuple(c) for c in coupons]
before = copy.deepcopy((lines, coupons))
t = time.monotonic()
got = optimize_coupons(lines, coupons)
print(json.dumps({"got": [got[0], list(got[1])], "secs": time.monotonic() - t,
                  "mutated": before != (lines, coupons)}))
"""


def probe(repo, lines, coupons):
    try:
        r = subprocess.run([sys.executable, "-c", PROBE], cwd=repo, input=json.dumps([lines, coupons]),
                           capture_output=True, text=True, timeout=30)
        d = json.loads(r.stdout)
        return d["got"], d["secs"], d["mutated"]
    except Exception:
        return None, None, None


def check(repo):
    c = {}
    fixed = [probe(repo, l, cp) + (list(e),) for l, cp, e in [(l, cp, [e[0], e[1]]) for l, cp, e in cases]]
    c["fixed_cases"] = sum(g == e and s is not None and s < 10 for g, s, _, e in fixed)
    c["no_mutation"] = all(m is False for _, _, m, _ in fixed)
    rand = [probe(repo, l, cp) + ([e[0], e[1]],) for l, cp, e in random_cases()]
    c["random_cases"] = sum(g == e and s is not None and s < 10 for g, s, _, e in rand)
    tests = subprocess.run([sys.executable, "-m", "unittest", "-v"], cwd=repo, capture_output=True,
                           text=True, timeout=300)
    # The fixture's old tax tests may still fail; judge only the coupon tests asked for.
    coupon = [l for l in tests.stderr.splitlines() if " ... " in l and "coupon" in l.lower()]
    c["tests_pass"] = len(coupon) >= 2 and all(l.rstrip().endswith("ok") for l in coupon)
    c["core"] = c["fixed_cases"] == len(cases) and c["random_cases"] == 5
    c["score"] = c["fixed_cases"] + c["random_cases"] + c["no_mutation"] + c["tests_pass"]
    return c


def main(repo):
    try:
        result = check(pathlib.Path(repo))
    except Exception as e:  # a broken repo is a result, not a crash
        result = {"error": f"{type(e).__name__}: {e}", "core": False, "score": 0}
    print(json.dumps(result))


if __name__ == "__main__":
    main(sys.argv[1])
