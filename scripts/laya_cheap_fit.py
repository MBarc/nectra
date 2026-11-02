#!/usr/bin/env python3
"""Fit the laya-cheap candidate: tier thresholds, stakes_weight and which model
claims each role, toward the cheapest-acceptable oracle. Train data only.

  laya_cheap_fit.py oracle                  train oracle: non-held-out calibration results, Claude models
  laya_cheap_fit.py probe train|heldout     Laya once per prompt under forced Simple/Normal/Demanding tiers
  laya_cheap_fit.py fit                     grid over role map x demanding_above x stakes_weight, train only
  laya_cheap_fit.py score MAP DA W          simulated held-out score of one setting (MAP = simple,general,planning,demanding models)

The probe needs a release `nectra` built from master and Laya's pinned model
files under $XDG_DATA_HOME/nectra/laya. assert_clean() fails if any rh- id or
held-out prompt text reaches the oracle, the probe of train or the fit.
"""
import argparse, itertools, json, os, re, subprocess, sys, tempfile
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

REPO = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(REPO / "scripts"))
import routing_accuracy as ra  # noqa: E402

CAL = REPO.parent / "nectra-calibration"
OUT = REPO / "eval/routing/laya-cheap"
HELDOUT_ORACLE = REPO / "eval/routing/interim/current/oracle.json"
HELDOUT_FILES = [REPO / "eval/routing/new-heldout-prompts.json", REPO / "eval/routing/new-heldout-prompts-2.json"]
ALLOWED = lambda d: d.startswith(("coverage-", "codex-v1", "crossgrade")) or d in ("default", "hard-pilot")
CLAUDE = ["haiku", "sonnet", "opus", "fable"]
ROLES = ["simple", "general", "planning", "demanding"]
SHIPPED = {"simple": "haiku", "general": "sonnet", "planning": "opus", "demanding": "fable"}


def norm(s):
    return " ".join(s.split()).lower()


def assert_clean(rows):
    """Fail if any held-out task id or prompt text reaches training or tuning."""
    ids, texts = set(), set()
    for f in HELDOUT_FILES:
        for t in json.loads(f.read_text(encoding="utf-8-sig"))["tasks"]:
            ids.add(t["id"]); texts.add(norm(t["prompt"]))
    assert len(ids) == 200, len(ids)
    for r in rows:
        tid, p = r["task"], norm(r["prompt"])
        assert not tid.startswith("rh-") and tid not in ids, f"held-out id in training: {tid}"
        assert not any(h in p or p in h for h in texts), f"held-out prompt text in training: {tid}"


def train_items():
    items = json.loads((OUT / "train-oracle.json").read_text(encoding="utf-8"))["tasks"]
    assert_clean(items)
    return items


def build_oracle(results):
    with tempfile.TemporaryDirectory() as tmp:
        for d in sorted(os.listdir(results)):
            if ALLOWED(d):
                os.symlink(Path(results) / d, Path(tmp) / d)
        ts, _, q, costs = ra.aggregate(CAL / "eval/calibration/tasks", tmp, ra.load_prices(REPO))
    ts = {k: v for k, v in ts.items() if not k.startswith("rh-")}
    q = {k: v for k, v in q.items() if k[1] in CLAUDE}
    costs = {k: v for k, v in costs.items() if k[1] in CLAUDE}
    rows = ra.build_oracle(ts, q, costs, 0.05, CLAUDE)
    assert_clean(rows)
    OUT.mkdir(parents=True, exist_ok=True)
    (OUT / "train-oracle.json").write_text(json.dumps({"margin": 0.05, "tasks": rows}, indent=1) + "\n", encoding="utf-8")
    print(len(rows), "train tasks; oracle", {m: sum(r["oracle_model"] == m for r in rows) for m in CLAUDE})


WHY = re.compile(r"laya classified prompt as (\S+) \(confidence [\d.]+, difficulty ([\d.]+) \+ [\d.]+ x stakes ([\d.]+) = [\d.]+ -> (\w+)(?:, role (\S+))?\)")


def probe(which, binary, registry):
    items = train_items() if which == "train" else json.loads(HELDOUT_ORACLE.read_text(encoding="utf-8"))["tasks"]
    work = Path(tempfile.mkdtemp(prefix="laya-cheap-"))  # empty cwd: no prompt-named files to inline

    def config(sb, da):
        p = work / f"{sb}-{da}.toml"
        # The tiny weight only makes the reason print the stakes score.
        p.write_text(f'providers = ["anthropic"]\nregistry = {json.dumps(str(Path(registry).resolve()))}\n'
                     f'[classifier]\nbackend = "laya"\nlaya_timeout_ms = 600000\nsimple_below = {sb}\ndemanding_above = {da}\n'
                     '[routing]\nstakes_weight = 0.0001\n[claude]\ncommand = "claude"\n[codex]\ncommand = "codex"\n')
        return str(p)

    cfgs = {"S": config(1.0, 1.0), "N": config(0.0, 1.0), "D": config(0.0, 0.0)}

    def one(it):
        out = {"task": it["task"]}
        for tier, c in cfgs.items():
            p = subprocess.run(["nice", "-n", "19", binary, "--config", c, "--stdin", "route"], input=it["prompt"],
                               text=True, capture_output=True, timeout=900, cwd=work)
            if p.returncode:
                raise RuntimeError(p.stderr[-500:])
            d = json.loads(p.stdout)
            m = WHY.search(d["reason"])
            assert m, d["reason"]
            out[tier] = d["model"]
            out.update(type=m[1], raw=float(m[2]), stakes=float(m[3]), role=m[5])
        return out

    with ThreadPoolExecutor(2) as ex:
        res = list(ex.map(one, items))
    (OUT / f"probe-{which}.json").write_text(json.dumps(res, indent=1) + "\n", encoding="utf-8")
    print(which, len(res), "probed")


def final_role(p, da, w):
    if p["role"] == "planning":
        return "planning"  # the router keeps planning even when demanding
    # Escalates on the tier, Laya's own role, or the router's demanding words
    # (a fable pick in the forced-Normal probe).
    if min(1.0, p["raw"] + w * p["stakes"]) > da or p["role"] == "demanding" or p["N"] == "fable":
        return "demanding"
    return p["role"]


def rows_for(items, probes, mapping, da, w):
    by = {p["task"]: p for p in probes}
    return [{**it, "chosen_model": (m := mapping[final_role(by[it["task"]], da, w)]),
             "chosen_provider": "anthropic", **ra.grade_pick(it, m)} for it in items]


def fit():
    items = train_items()
    probes = json.loads((OUT / "probe-train.json").read_text(encoding="utf-8"))
    assert {p["task"] for p in probes} == {i["task"] for i in items}
    # The simulation must reproduce the real picks: Simple and Normal tiers pick
    # alike (Laya's role decides), Demanding escalates. raw prints at 2 dp, so
    # 0.00 can be a positive raw that the forced-Demanding probe escalated.
    assert all(p["S"] == p["N"] == SHIPPED[final_role(p, 1.0, 0)] and
               (p["D"] == SHIPPED[final_role(p, 0.0, 1e-4)] or p["raw"] + p["stakes"] == 0) for p in probes)
    keys = ("accuracy", "accuracy_ci95", "over_rate", "exact_rate", "mean_cost_usd")
    base = ra.summarize("shipped", rows_for(items, probes, SHIPPED, 0.6, 0.3))
    print("shipped", {k: base[k] for k in keys})
    for m in CLAUDE:
        s = ra.summarize("", ra.fixed_rows(items, m))
        print(f"always-{m}", {k: s[k] for k in keys})
    res = []
    for combo in itertools.product(CLAUDE, repeat=4):
        mp = dict(zip(ROLES, combo))
        for da in [0.5, 0.6, 0.7, 0.8, 0.9, 1.0]:
            for w in [0.0, 0.15, 0.3, 0.5]:
                res.append((combo, da, w, ra.summarize("", rows_for(items, probes, mp, da, w))))
    # Accuracy/cost frontier; among equal points, the thresholds closest to what ships.
    res.sort(key=lambda r: (r[3]["mean_cost_usd"], abs(r[1] - 0.6) + abs(r[2] - 0.3)))
    top = -1.0
    print("frontier (simple, general, planning, demanding), demanding_above, stakes_weight:")
    for r in res:
        if r[3]["accuracy"] > top:
            top = r[3]["accuracy"]
            print(" ", r[0], r[1], r[2], {k: r[3][k] for k in keys})
    by = {p["task"]: p for p in probes}
    print("Laya role of each train task haiku fails:",
          sorted((by[i["task"]]["role"], i["oracle_model"]) for i in items if "haiku" not in i["acceptable_set"]))


def score(spec, da, w):
    items = json.loads(HELDOUT_ORACLE.read_text(encoding="utf-8"))["tasks"]
    probes = json.loads((OUT / "probe-heldout.json").read_text(encoding="utf-8"))
    print(ra.summarize(spec, rows_for(items, probes, dict(zip(ROLES, spec.split(","))), float(da), float(w))))


if __name__ == "__main__":
    ap = argparse.ArgumentParser()
    ap.add_argument("command", choices=["oracle", "probe", "fit", "score"])
    ap.add_argument("args", nargs="*")
    ap.add_argument("--results", default=str(CAL / "eval/calibration/results"))
    ap.add_argument("--binary", default="target/release/nectra")
    ap.add_argument("--registry", default=str(REPO / "config/models.toml"))
    a = ap.parse_args()
    {"oracle": lambda: build_oracle(a.results), "probe": lambda: probe(a.args[0], a.binary, a.registry),
     "fit": fit, "score": lambda: score(*a.args)}[a.command]()
