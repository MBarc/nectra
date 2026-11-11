#!/usr/bin/env python3
"""Inventory of the Laya ship-gate inputs in the calibration tree (read only, stdlib).

usage: inventory.py [--root ~/nectra-calibration] [--out GATE-STATUS.md]
"""
import argparse, collections, json, sys, time
from pathlib import Path

HELD = "routing-new-heldout"


def load(p):
    try:
        return json.loads(Path(p).read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return None


def set_info(d):
    runs = collections.defaultdict(lambda: collections.Counter())
    for m in sorted((d / "runs").glob("*")):
        for p in m.glob("*.json"):
            r = load(p) or {}
            runs[m.name][r.get("status", "?")] += 1
    graded = collections.defaultdict(set)  # grader -> tasks
    graded_out = collections.defaultdict(collections.Counter)  # grader -> model -> outputs
    for g in (d / "grades" / "panel").glob("*"):
        for p in g.glob("*.json"):
            graded[g.name].add(p.stem)
            for o in (load(p) or {}).get("outputs", []):
                graded_out[g.name][o["model"]] += 1
    legacy = [p.stem for p in (d / "grades").glob("*.json")]
    tasks = set()
    for m in (d / "runs").glob("*"):
        tasks |= {p.name.split(".r")[0] for p in m.glob("*.json")}
    return runs, graded, graded_out, legacy, tasks


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--root", default=str(Path.home() / "nectra-calibration"))
    ap.add_argument("--out", default=None)
    a = ap.parse_args()
    root = Path(a.root)
    res = root / "eval/calibration/results"
    L = [f"# Laya gate status (generated {time.strftime('%Y-%m-%d %H:%M:%SZ', time.gmtime())}; read-only inventory)", ""]
    h = res / HELD
    runs, graded, gout, legacy, tasks = set_info(h)
    rh = sorted(t for t in tasks if t.startswith("rh-"))
    L += [f"## Gate set `{HELD}`: {len(rh)} rh-* prompts", "",
          "Runs per model (status ok/failed; a task needs a run for every role model to be graded as a blind batch):", "",
          "| model | ok | failed | total | missing vs 200 |", "|---|--:|--:|--:|--:|"]
    for m, c in runs.items():
        t = sum(c.values())
        L.append(f"| {m} | {c['ok']} | {c['failed']} | {t} | {len(rh) - t} |")
    L += ["", "Grades per grader (one grade file = one task, all models blind-ranked in one batch):", "",
          "| grader | tasks graded | missing | outputs graded |", "|---|--:|--:|--:|"]
    for g in ("fable", "gpt-6-astra"):
        n = len(graded.get(g, ()))
        L.append(f"| {g} | {n} | {len(rh) - n} | {sum(gout[g].values())} |")
    miss_f = sorted(set(rh) - graded.get("fable", set()))
    miss_a = sorted(set(rh) - graded.get("gpt-6-astra", set()))
    both = sorted(graded.get("fable", set()) & graded.get("gpt-6-astra", set()))
    L += ["", f"* tasks with BOTH graders: {len(both)} ({', '.join(both) or 'none'})",
          f"* Fable-missing ({len(miss_f)}): {', '.join(miss_f) or 'none'}",
          f"* Astra-missing: {len(miss_a)} of {len(rh)} (rerun `gate-relaunch.sh` at 14:55Z)", "",
          "Per grader x model outputs graded:", "", "| grader | " + " | ".join(runs) + " |", "|---|" + "--:|" * len(runs)]
    for g in ("fable", "gpt-6-astra"):
        L.append(f"| {g} | " + " | ".join(str(gout[g][m]) for m in runs) + " |")
    L += ["", "Note: astra-half.sh's step 1 measures gpt-5.6-sol, gpt-5.6-luna, gpt-6-astra (all 200 done); gpt-6.1-sol and gpt-6-luna "
          "are NOT measured on the 200 (step 4, low priority; not needed for the gate).", "",
          "## Other sets (coverage / panel)", "",
          "| set | models (ok runs) | tasks | fable graded | astra graded | astra missing |", "|---|---|--:|--:|--:|--:|"]
    for d in sorted(res.iterdir()):
        if not d.is_dir() or d.name == HELD:
            continue
        r, g, go, lg, ts = set_info(d)
        ms = ", ".join(f"{m}:{c['ok']}" for m, c in r.items())
        fa = len(g.get("fable", ()))
        aa = len(g.get("gpt-6-astra", ()))
        panel_in_scope = d.name in ("coverage-non-google", "coverage-routing-new", "coverage-google", "codex-v1-fixed", "codex-v1-hard-fixed")
        miss = f"{len(ts) - aa}" if panel_in_scope else "n/a (out of astra-half scope)"
        L.append(f"| {d.name} | {ms or '-'} | {len(ts)} | {fa}{' (+%d legacy single-grader)' % len(lg) if lg else ''} | {aa} | {miss} |")
    text = "\n".join(L) + "\n"
    if a.out:
        Path(a.out).write_text(text, encoding="utf-8")
    print(text)


if __name__ == "__main__":
    sys.exit(main())
