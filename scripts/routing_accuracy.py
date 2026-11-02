#!/usr/bin/env python3
"""Build the routing oracle, fit registry scores, and measure routing accuracy.

The script is deliberately stdlib-only.  It consumes the calibration worktree's
JSON artifacts, writes reviewable artifacts under eval/routing, and can invoke
the real `nectra route` command for the accuracy pass.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import statistics
import subprocess
import sys
import tempfile
import tomllib
from pathlib import Path

CAPS = ["coding", "reasoning", "long_context", "writing", "tool_use"]
ALL_CAPS = CAPS + ["speed", "cost_efficiency"]
TYPE_ROLE = {"code_generation": "coding", "debugging": "coding", "refactoring": "coding",
             "code_review": "general", "planning": "planning", "writing": "general",
             "research": "general", "long_context": "general", "tool_use": "simple"}


def load_prices(repo):
    data = read_json(Path(repo) / "eval/chat-savings/prices.json")
    aliases = data.get("_aliases", {})
    prices = {model: value for model, value in data.items() if isinstance(value, dict) and not model.startswith("_")}
    for alias, canonical in aliases.items():
        if canonical in prices:
            prices[alias] = prices[canonical]
    return prices


def api_cost(model, usage, prices):
    """Price reported tokens at one shared API-equivalent list-price table."""
    p = prices.get(model)
    if not p or not usage:
        return None
    million = 1_000_000
    total_write = usage.get("cache_write", 0) or 0
    write_5m = usage.get("cache_write_5m", 0) or 0
    write_1h = max(total_write - write_5m, 0)
    return round((usage.get("input", 0) * p["input"] + usage.get("cache_read", 0) * p["cache_read"] +
                  write_5m * p.get("cache_write_5m", p.get("cache_write", 0)) +
                  write_1h * p.get("cache_write", 0) + usage.get("output", 0) * p["output"]) / million, 6)


def read_json(path):
    return json.loads(Path(path).read_text(encoding="utf-8"))


def tasks(root):
    out = {}
    for d in sorted(Path(root).iterdir()):
        p = d / "task.json"
        if p.is_file():
            t = read_json(p)
            t["_dir"] = str(d)
            out[t["id"]] = t
    return out


def artifacts(results):
    runs, grades = [], {}
    for rd in Path(results).iterdir():
        if not rd.is_dir():
            continue
        for p in (rd / "runs").glob("*/*.json"):
            try:
                r = read_json(p)
            except (OSError, ValueError):
                continue
            if r.get("status") in ("ok", "failed"):
                runs.append(r)
        for p in (rd / "grades").glob("*.json"):
            try:
                g = read_json(p)
            except (OSError, ValueError):
                continue
            for o in g.get("outputs", []):
                grades[(o.get("model"), g.get("task"), o.get("rep", 0))] = o.get("score")
    return runs, grades


def quality(run, grades):
    parts = []
    obj = run.get("objective")
    if obj is not None:
        parts.append(float(obj.get("score", 0)))
    grade = grades.get((run.get("model"), run.get("task"), run.get("rep", 0)))
    if grade is not None:
        parts.append(float(grade))
    return statistics.mean(parts) if parts else None


def aggregate(root, results, prices):
    ts, grades = tasks(root), None
    runs, grades = artifacts(results)
    by = {}
    for r in runs:
        if r.get("task") not in ts:
            continue
        q = quality(r, grades)
        if q is not None:
            by.setdefault((r["task"], r["model"]), []).append((q, r))
    quality_by = {k: statistics.mean(x[0] for x in v) for k, v in by.items()}
    cost_by_task_model = {}
    for r in runs:
        c = api_cost(r.get("model"), r.get("usage"), prices)
        if c is not None:
            cost_by_task_model.setdefault((r["task"], r["model"]), []).append(c)
    costs = {k: statistics.median(v) for k, v in cost_by_task_model.items()}
    return ts, runs, quality_by, costs


def split(task_id):
    # Stable split, independent of filesystem ordering and future runs.
    return int(hashlib.sha256(task_id.encode()).hexdigest()[:8], 16) % 10 < 7


def build_oracle(ts, quality_by, costs, margin, model_ids):
    rows = []
    for tid, t in ts.items():
        models = {m: q for (task, m), q in quality_by.items() if task == tid}
        if not models:
            continue
        best_q = max(models.values())
        acceptable = sorted(m for m, q in models.items() if q >= best_q - margin)
        if not acceptable:
            continue
        # A model without measured cost is not allowed to win the cost oracle.
        priced = [m for m in acceptable if (tid, m) in costs]
        oracle = min(priced, key=lambda m: (costs[(tid, m)], m)) if priced else min(acceptable)
        all_models = {m: {"measured": m in models, "quality": round(models[m], 4) if m in models else None,
                          "cost_usd": round(costs[(tid, m)], 6) if (tid, m) in costs else None}
                      for m in model_ids}
        rows.append({"prompt": t["prompt"], "task": tid, "task_type": t["type"],
                     "tier": t["tier"], "oracle_model": oracle,
                     "acceptable_set": acceptable, "best_model": sorted(m for m, q in models.items() if q == best_q),
                     "quality": {m: round(q, 4) for m, q in sorted(models.items())},
                     "cost_usd_median": {m: round(costs[(tid, m)], 6) for (task, m) in costs if task == tid},
                     "models": all_models,
                     "cost_source": "usage tokens × eval/chat-savings/prices.json API-equivalent list prices",
                     "split": "train" if split(tid) else "heldout"})
    return rows


def fit_scores(ts, runs, grades, train_only=True):
    q = {}
    for r in runs:
        if train_only and not split(r.get("task", "")):
            continue
        v = quality(r, grades)
        if v is not None:
            q.setdefault((r["model"], r["task"]), []).append(v)
    q = {k: statistics.mean(v) for k, v in q.items()}
    scores = {}
    for model in sorted({m for m, _ in q}):
        scores[model] = {}
        for c in CAPS:
            vals = []
            for (m, tid), value in q.items():
                if m == model and ts.get(tid, {}).get("capabilities", {}).get(c, 0) > 0:
                    vals.append((ts[tid]["capabilities"][c], value))
            if len(vals) >= 3:
                scores[model][c] = round(sum(w * v for w, v in vals) / sum(w for w, _ in vals), 3)
        for field in ("wall_s", "cost_usd"):
            vals = [float(r[field]) for r in runs if r.get("model") == model and r.get(field) not in (None, 0)
                    and (not train_only or split(r.get("task", "")))]
            if vals:
                scores[model]["speed" if field == "wall_s" else "cost_efficiency"] = vals
    # Relative speed/cost is calculated against the best measured model, then
    # mapped to the same log scale used by calibrate.py.
    for field, key in (("speed", "speed"), ("cost_usd", "cost_efficiency")):
        vals = {m: v for m, s in scores.items() if (v := s.get(key)) and isinstance(v, list)}
        if not vals:
            continue
        med = {m: statistics.median(v) for m, v in vals.items()}
        best = min(med.values())
        for m, value in med.items():
            scores[m][key] = round(max(0.0, min(1.0, 1 - __import__("math").log10(max(value / best, 1.0)) / 2)), 3)
    for s in scores.values():
        for c in ALL_CAPS:
            if isinstance(s.get(c), list):
                del s[c]
    return scores


def fit_roles(ts, runs, grades):
    values = {}
    for r in runs:
        if not split(r.get("task", "")):
            continue
        q = quality(r, grades)
        t = ts.get(r.get("task"))
        if q is not None and t:
            values.setdefault((r["model"], TYPE_ROLE.get(t["type"], "general")), []).append(q)
    roles = {}
    for (model, role), vals in values.items():
        mean = statistics.mean(vals)
        if len(vals) >= 2 and (model not in roles or mean > roles[model][1]):
            roles[model] = (role, mean)
    return {m: role for m, (role, _) in roles.items()}


def rewrite_registry(source, target, scores, roles=None):
    text = Path(source).read_text(encoding="utf-8")
    out, current = [], None
    for line in text.splitlines(keepends=True):
        m = re.match(r'\s*id\s*=\s*"([^"]+)"', line)
        if m:
            current = m.group(1)
        if roles and current in roles and re.match(r'\s*prefers\s*=\s*\[', line):
            line = f'prefers = ["{roles[current]}"]\n'
        m = re.match(r'(\s*)(coding|reasoning|long_context|writing|tool_use|speed|cost_efficiency)(\s*=\s*)[0-9.]+(\s*)$', line.rstrip("\n"))
        if m and current in scores and m.group(2) in scores[current]:
            line = f"{m.group(1)}{m.group(2)}{m.group(3)}{scores[current][m.group(2)]}{m.group(4)}\n"
        out.append(line)
    Path(target).write_text("# Generated from calibration data; default models.toml is unchanged.\n" + "".join(out), encoding="utf-8")


def config_for(registry, providers, settings='[classifier]\nbackend = "local"\n'):
    fd, name = tempfile.mkstemp(prefix="nectra-routing-", suffix=".toml")
    os.close(fd)
    Path(name).write_text("providers = [" + ", ".join(json.dumps(p) for p in providers) + "]\n" +
                         f"registry = {json.dumps(str(Path(registry).resolve()))}\n\n{settings}\n[claude]\ncommand = \"claude\"\n\n[codex]\ncommand = \"codex\"\n\n[gemini]\ncommand = \"gemini\"\n", encoding="utf-8")
    return name


def route(binary, config, prompt):
    p = subprocess.run([str(binary), "--config", config, "--stdin", "route"], input=prompt,
                       text=True, encoding="utf-8", capture_output=True, timeout=600)  # Laya loads its model per call
    if p.returncode:
        raise RuntimeError((p.stderr or p.stdout)[-1000:])
    return json.loads(p.stdout)


def grade_pick(item, chosen):
    chosen_info = item.get("models", {}).get(chosen, {"measured": False})
    measured = bool(chosen_info.get("measured"))
    acceptable = chosen in item["acceptable_set"] if measured else None
    # Exact = the pick is the oracle's (cheapest acceptable) model.
    # Under = measured quality below the acceptable threshold. Over =
    # measured, acceptable, but more expensive than the cheapest
    # acceptable measured model. Unmeasured picks are unknown, never
    # penalised as either class.
    under = measured and not acceptable
    oracle_cost = item.get("models", {}).get(item["oracle_model"], {}).get("cost_usd")
    over = bool(measured and acceptable and chosen_info.get("cost_usd") is not None and
                oracle_cost is not None and chosen_info["cost_usd"] > oracle_cost)
    return {"chosen_measured": measured, "exact": chosen == item["oracle_model"] if measured else None,
            "acceptable": acceptable, "under": under, "over": over}


def _selftest():
    item = {"oracle_model": "haiku", "acceptable_set": ["haiku", "sonnet"], "best_model": ["sonnet"],
            "models": {"haiku": {"measured": True, "cost_usd": 0.01}, "sonnet": {"measured": True, "cost_usd": 0.05},
                       "opus": {"measured": True, "cost_usd": 0.2}}}
    g = grade_pick(item, "sonnet")  # acceptable but not the oracle pick
    assert g["acceptable"] and not g["exact"] and g["over"] and not g["under"], g
    g = grade_pick(item, "haiku")
    assert g["exact"] and g["acceptable"] and not g["over"], g
    g = grade_pick(item, "opus")
    assert not g["exact"] and g["under"] and not g["over"], g
    g = grade_pick(item, "gpt")  # unmeasured: unknown, not penalised
    assert g["exact"] is None and g["acceptable"] is None and not g["under"] and not g["over"], g
    lo, hi = wilson(43, 48)
    assert 0.77 < lo < 0.79 and 0.94 < hi < 0.96, (lo, hi)
    assert wilson(0, 0) == [None, None]
    items = [{**item, "task": "t"}]
    c = compare([("a", fixed_rows(items, "sonnet"))], baselines=("haiku",))
    assert [p["policy"] for p in c["policies"]] == ["a", "always-haiku"], c
    assert c["policies"][1]["exact_rate"] == 1.0 and c["policies"][0]["exact_rate"] == 0.0, c
    print("selftest ok")


def evaluate(args, oracle):
    registry = Path(args.registry).resolve()
    models = tomllib.loads(registry.read_text(encoding="utf-8"))["model"]
    providers = sorted({m["provider"] for m in models if m.get("enabled", True)})
    if args.providers:  # interim runs: restrict to providers the oracle actually measured
        providers = [p for p in providers if p in args.providers.split(",")]
    extra = {"settings": Path(args.settings).read_text(encoding="utf-8")} if args.settings else {}
    configs = {None: config_for(registry, providers, **extra)}
    for provider in providers:
        configs[provider] = config_for(registry, [p for p in providers if p != provider], **extra)
    rows = []
    try:
        for item in oracle:
            if args.split != "all" and item["split"] != args.split:
                continue
            decision = route(args.binary, configs[None], item["prompt"])
            # A Laya timeout silently routes on keywords; that would score the wrong router.
            if 'backend = "laya"' in extra.get("settings", "") and decision["reason"].startswith("keywords"):
                raise RuntimeError(f"{item['task']}: Laya fell back to keywords: {decision['reason'][:200]}")
            chosen = decision["model"]
            fallback = {}
            for provider in providers:
                try:
                    fallback[provider] = route(args.binary, configs[provider], item["prompt"])["model"]
                except Exception as e:
                    fallback[provider] = {"error": str(e)}
            rows.append({**item, "chosen_model": chosen, "chosen_provider": decision["provider"],
                         "fallback": fallback, **grade_pick(item, chosen)})
    finally:
        for c in configs.values():
            Path(c).unlink(missing_ok=True)
    return rows


def wilson(k, n, z=1.96):
    """95% Wilson score interval for k successes in n trials."""
    if not n:
        return [None, None]
    p = k / n
    d = 1 + z * z / n
    c = (p + z * z / (2 * n)) / d
    h = z * ((p * (1 - p) / n + z * z / (4 * n * n)) ** 0.5) / d
    return [round(max(0.0, c - h), 4), round(min(1.0, c + h), 4)]


def fixed_rows(items, model):
    """Grade an always-`model` policy against the same oracle items."""
    return [{**it, "chosen_model": model, **grade_pick(it, model)} for it in items]


def summarize(name, rows):
    m = [r for r in rows if r["chosen_measured"]]
    ok = sum(bool(r["acceptable"]) for r in m)
    costs = [r["models"][r["chosen_model"]]["cost_usd"] for r in m if r["models"][r["chosen_model"]].get("cost_usd") is not None]
    return {"policy": name, "n": len(rows), "measured_n": len(m), "accuracy": round(ok / len(m), 4) if m else None,
            "accuracy_ci95": wilson(ok, len(m)),
            # over-routing needs the pick's price; an unpriced model (fable) is n/a, not 0%
            "over_rate": round(sum(bool(r["over"]) for r in m) / len(m), 4) if m and costs else None,
            "exact_rate": round(sum(bool(r["exact"]) for r in m) / len(m), 4) if m else None,
            "priced_n": len(costs), "mean_cost_usd": round(statistics.mean(costs), 6) if costs else None}


def compare(named_rows, baselines=("sonnet", "fable")):
    """Side-by-side policies on the same tasks. named_rows: [(name, rows)] from eval reports."""
    items = named_rows[0][1]
    pols = [summarize(n, r) for n, r in named_rows] + [summarize(f"always-{b}", fixed_rows(items, b)) for b in baselines]
    verdicts = []
    for p in pols[:len(named_rows)]:
        for b in pols[len(named_rows):]:
            if p["accuracy"] is None or b["accuracy"] is None:
                continue
            overlap = p["accuracy_ci95"][0] <= b["accuracy_ci95"][1] and b["accuracy_ci95"][0] <= p["accuracy_ci95"][1]
            known = p["mean_cost_usd"] is not None and b["mean_cost_usd"] is not None
            delta = f"{(p['accuracy'] - b['accuracy']) * 100:+.1f} pts"
            if not overlap:
                v = f"beats on accuracy ({delta})" if p["accuracy"] > b["accuracy"] else f"worse on accuracy ({delta})"
            elif not known:
                v = f"accuracy not significantly different ({delta}, CIs overlap); cost unknown, baseline unpriced"
            elif p["mean_cost_usd"] < b["mean_cost_usd"]:
                v = f"matches accuracy ({delta}, CIs overlap) at lower cost"
            else:
                v = f"no accuracy gain ({delta}, CIs overlap) and not cheaper"
            verdicts.append({"policy": p["policy"], "baseline": b["policy"], "verdict": v})
    return {"policies": pols, "verdicts": verdicts,
            "note": "Accuracy = in-acceptable-set over tasks where the pick was measured; CI is Wilson 95%. "
                    "Cost = mean API-equivalent list-price cost of the picked model per task, over priced picks only."}


def compare_markdown(c):
    lines = ["| policy | n measured | accuracy | 95% CI | over-routing | exact | mean cost/task |", "|---|---:|---:|---|---:|---:|---:|"]
    pct = lambda x: "n/a" if x is None else f"{x * 100:.1f}%"
    for p in c["policies"]:
        ci = "n/a" if p["accuracy_ci95"][0] is None else f"{p['accuracy_ci95'][0] * 100:.1f}–{p['accuracy_ci95'][1] * 100:.1f}%"
        cost = "n/a (unpriced)" if p["mean_cost_usd"] is None else f"${p['mean_cost_usd']:.4f}"
        lines.append(f"| {p['policy']} | {p['measured_n']}/{p['n']} | {pct(p['accuracy'])} | {ci} | {pct(p['over_rate'])} | {pct(p['exact_rate'])} | {cost} |")
    lines += ["", "| policy | vs baseline | verdict |", "|---|---|---|"] + [f"| {v['policy']} | {v['baseline']} | {v['verdict']} |" for v in c["verdicts"]]
    return "\n".join(lines) + "\n\n" + c["note"] + "\n"


def report(rows, cost):
    measured_rows = [r for r in rows if r["chosen_measured"]]
    n = len(measured_rows) or 1
    def rate(k): return round(sum(bool(r[k]) for r in measured_rows) / n, 4)
    by_provider = {}
    for r in rows:
        by_provider.setdefault(r["chosen_provider"], []).append(r)
    types = {}
    for r in rows:
        types.setdefault(r["task_type"], []).append(r)
    examples = [{"task": r["task"], "task_type": r["task_type"], "tier": r["tier"],
                 "models": r.get("models", {}), "acceptable_set": r["acceptable_set"],
                 "best_model": r["best_model"], "oracle_model": r["oracle_model"],
                 "router_picked": r["chosen_model"], "router_picked_measured": r["chosen_measured"]}
                for r in rows[:5]]
    def breakdown(groups):
        result = {}
        for key, values in sorted(groups.items()):
            measured = [r for r in values if r["chosen_measured"]]
            d = {"n": len(values), "measured_n": len(measured), "coverage": round(len(measured) / len(values), 4)}
            if measured:
                d.update({"acceptable": round(sum(bool(r["acceptable"]) for r in measured) / len(measured), 4),
                          "under": round(sum(bool(r["under"]) for r in measured) / len(measured), 4),
                          "over": round(sum(bool(r["over"]) for r in measured) / len(measured), 4)})
            result[key] = d
        return result
    over_costs = [r["models"][r["chosen_model"]]["cost_usd"] for r in rows if r["over"] and r["models"].get(r["chosen_model"], {}).get("cost_usd") is not None]
    return {"n": len(rows), "measured_route_n": len(measured_rows), "unmeasured_route_n": len(rows) - len(measured_rows),
            "coverage_rate": round(len(measured_rows) / (len(rows) or 1), 4), "exact_match_rate": rate("exact"), "in_acceptable_set_rate": rate("acceptable"),
            "in_acceptable_set_ci95": wilson(sum(bool(r["acceptable"]) for r in measured_rows), len(measured_rows)),
            "under_routing_rate": rate("under"), "over_routing_rate": rate("over"),
            "over_routing_cost_usd_median": round(statistics.median(over_costs), 6) if over_costs else 0,
            "by_provider": breakdown(by_provider), "by_task_type": breakdown(types),
            "coverage_note": "Rates exclude router picks not measured on that task; those picks are reported as unknown, not under/over.",
            "examples": examples, "rows": rows}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("command", choices=["build", "fit", "eval", "all", "selftest", "compare"])
    ap.add_argument("--reports", nargs="*", default=[], help="compare: name=report_dir pairs, e.g. current=eval/routing/interim/current")
    ap.add_argument("--calibration-root", default=None)
    ap.add_argument("--results", default=None)
    ap.add_argument("--tasks", default=None)
    ap.add_argument("--registry", default="config/models.toml")
    ap.add_argument("--binary", default="target/release/nectra.exe")
    ap.add_argument("--margin", type=float, default=0.05)
    ap.add_argument("--out", default="eval/routing")
    ap.add_argument("--providers", default=None, help="comma list; limit eval to these providers")
    ap.add_argument("--settings", default=None,
                    help="eval: TOML with [classifier]/[routing] for the router (e.g. config/laya-cheap.toml); default keywords")
    ap.add_argument("--split",choices=["all", "train", "heldout"], default="all")
    args = ap.parse_args()
    if args.command == "selftest":
        return _selftest()
    if args.command == "compare":
        named = [(n, read_json(Path(d) / "report.json")["rows"]) for n, d in (x.split("=", 1) for x in args.reports)]
        c = compare(named)
        out = Path(args.out); out.mkdir(parents=True, exist_ok=True)
        (out / "comparison.json").write_text(json.dumps(c, indent=2) + "\n", encoding="utf-8")
        (out / "comparison.md").write_text(compare_markdown(c), encoding="utf-8")
        print(compare_markdown(c))
        return
    repo = Path(__file__).resolve().parents[1]
    cal = Path(args.calibration_root or repo.parent / "nectra-calibration")
    task_root = Path(args.tasks or cal / "eval/calibration/tasks")
    result_root = Path(args.results or cal / "eval/calibration/results")
    out = Path(args.out); out.mkdir(parents=True, exist_ok=True)
    if args.command == "eval" and (Path(args.out) / "oracle.json").is_file():
        oracle = read_json(Path(args.out) / "oracle.json")["tasks"]
        # Evaluation only needs the checked-in oracle; calibration inputs can
        # remain on the worker that owns them.
        all_costs = {}
        for item in oracle:
            for model, value in item.get("cost_usd_median", {}).items():
                all_costs.setdefault(model, []).append(value)
        cost = {model: statistics.median(values) for model, values in all_costs.items()}
        rows = evaluate(args, oracle)
        (Path(args.out) / "report.json").write_text(json.dumps(report(rows, cost), indent=2) + "\n", encoding="utf-8")
        print(f"evaluated {len(rows)} oracle tasks; output: {Path(args.out).resolve()}")
        return
    prices = load_prices(repo)
    ts, runs, quality_by, costs = aggregate(task_root, result_root, prices)
    registry_models = [m["id"] for m in tomllib.loads(Path(args.registry).read_text(encoding="utf-8"))["model"]]
    oracle = build_oracle(ts, quality_by, costs, args.margin, registry_models)
    if args.command in ("build", "all"):
        (out / "oracle.json").write_text(json.dumps({"margin": args.margin, "source": str(cal), "tasks": oracle}, indent=2) + "\n", encoding="utf-8")
    if args.command in ("fit", "all"):
        grades = artifacts(result_root)[1]
        scores = fit_scores(ts, runs, grades)
        roles = fit_roles(ts, runs, grades)
        calibrated = repo / "config" / "models.calibrated.toml"
        rewrite_registry(args.registry, calibrated, scores, roles)
        heldout = fit_scores(ts, runs, grades, train_only=False)
        (out / "fit.json").write_text(json.dumps({"scores": scores, "roles": roles, "heldout_measurements": heldout,
                                                    "train_tasks": sum(split(t) for t in ts), "heldout_tasks": sum(not split(t) for t in ts)}, indent=2) + "\n", encoding="utf-8")
    if args.command in ("eval", "all"):
        rows = evaluate(args, oracle)
        (out / "report.json").write_text(json.dumps(report(rows, costs), indent=2) + "\n", encoding="utf-8")
    print(f"oracle tasks: {len(oracle)}; measured models: {len({m for _, m in quality_by})}; output: {out.resolve()}")


if __name__ == "__main__":
    main()
