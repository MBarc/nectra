#!/usr/bin/env python3
"""Ship-gate routing compare on the 200-prompt held-out set (stdlib; never calls Codex/Astra).

Builds the cost oracle from routing-new-heldout under one GRADING VARIANT, routes every prompt with each policy through the
real `nectra route`, and reports in-acceptable-set accuracy with Wilson 95% CIs per tier and overall, plus fixed always-X
baselines and the gate verdict (hard tier = tier 3 must not regress vs keywords and always-sonnet).

variants (--variant):
  fable        Fable grades only (INTERIM until Astra lands)
  panel        mean of Fable and Astra grades per output (the final number)
  drop-self    panel, but gpt-6-astra's own outputs are dropped from the oracle (self-graded items removed)
  swap-fable   panel, but gpt-6-astra's outputs use the Fable grade instead of Astra's
Only tasks where every measured model has a grade under the variant are used (so fable-only = the Fable-graded subset).

policies (--policy NAME=SPEC, repeatable):
  SPEC = backend:KEYWORDS(=local)|LAYA|AUTO[:registry.toml]   master registry, that classifier backend
  SPEC = config:/path/to/nectra.toml             an existing nectra config (e.g. config/laya-cheap.toml; its registry key is used,
                                                 models unmeasured on the held-out set are NOT disabled, they count as unmeasured)
"""
import argparse, hashlib, json, math, os, re, statistics, subprocess, sys, tempfile, tomllib
from pathlib import Path

ROOT = Path.home() / "nectra-calibration"
HELD = ROOT / "eval/calibration/results/routing-new-heldout"
MEASURED = ["fable", "haiku", "opus", "sonnet", "gpt-5.6-luna", "gpt-5.6-sol", "gpt-6-astra"]
CLAUDE = ["fable", "haiku", "opus", "sonnet"]   # the only models Fable has graded so far


def models_for(variant):
    """Models in the oracle (and enabled for the policies) under a variant."""
    return {"fable": CLAUDE, "drop-self": [m for m in MEASURED if m != "gpt-6-astra"]}.get(variant, MEASURED)


def rj(p):
    return json.loads(Path(p).read_text(encoding="utf-8"))


def wilson(k, n, z=1.96):
    if not n:
        return (None, None)
    p = k / n; d = 1 + z * z / n
    c = (p + z * z / (2 * n)) / d
    h = z * math.sqrt(p * (1 - p) / n + z * z / (4 * n * n)) / d
    return (max(0.0, c - h), min(1.0, c + h))


def load_grades():
    g = {}  # (grader, task, model, rep) -> score
    for gd in (HELD / "grades/panel").glob("*"):
        for p in gd.glob("*.json"):
            d = rj(p)
            for o in d["outputs"]:
                g[(gd.name, d["task"], o["model"], o.get("rep", 0))] = o["score"]
    return g


def build_oracle(variant, margin, repo):
    tasks = {}
    for p in (ROOT / "eval/calibration/tasks").glob("rh-*/task.json"):
        t = rj(p); tasks[t["id"]] = t
    grades = load_grades()
    prices = rj(repo / "eval/chat-savings/prices.json")
    for a, c in prices.get("_aliases", {}).items():
        prices.setdefault(a, prices.get(c))
    runs = {}
    for m in MEASURED:
        for p in (HELD / "runs" / m).glob("*.json"):
            r = rj(p)
            if r.get("status") in ("ok", "failed"):
                runs.setdefault((r["task"], m), []).append(r)

    def grade_for(task, model, rep):
        f, a = grades.get(("fable", task, model, rep)), grades.get(("gpt-6-astra", task, model, rep))
        if variant == "fable":
            return f
        if variant == "panel":
            v = [x for x in (f, a) if x is not None]
            return statistics.mean(v) if len(v) == 2 else None   # panel needs both
        if variant == "drop-self":
            return None if model == "gpt-6-astra" else grade_for_panel(f, a)
        if variant == "swap-fable":
            return f if model == "gpt-6-astra" else grade_for_panel(f, a)

    def grade_for_panel(f, a):
        return statistics.mean([f, a]) if f is not None and a is not None else None

    def cost(model, u):
        p = prices.get(model)
        if not p or not u:
            return None
        w = u.get("cache_write", 0) or 0; w5 = u.get("cache_write_5m", 0) or 0
        return (u.get("input", 0) * p["input"] + u.get("cache_read", 0) * p["cache_read"] + w5 * p.get("cache_write_5m", p.get("cache_write", 0))
                + max(w - w5, 0) * p.get("cache_write", 0) + u.get("output", 0) * p["output"]) / 1e6

    oracle = []
    for tid, t in sorted(tasks.items()):
        q, c = {}, {}
        for m in models_for(variant):
            rs = runs.get((tid, m), [])
            if not rs:
                continue
            qs = []
            for r in rs:
                g = grade_for(tid, m, r.get("rep", 0))
                if g is None:
                    qs = "ungraded"; break
                parts = ([float(r["objective"]["score"])] if r.get("objective") else []) + [g]
                qs.append(statistics.mean(parts))
            if qs == "ungraded":
                q = None; break   # task not fully graded under this variant
            q[m] = statistics.mean(qs)
            cs = [x for x in (cost(m, r.get("usage")) for r in rs) if x is not None]
            if cs:
                c[m] = statistics.median(cs)
        if not q:
            continue
        best = max(q.values())
        acc = sorted(m for m, v in q.items() if v >= best - margin)
        priced = [m for m in acc if m in c]
        orc = min(priced, key=lambda m: (c[m], m)) if priced else min(acc)
        oracle.append({"task": tid, "prompt": t["prompt"], "tier": t["tier"], "type": t["type"], "quality": q, "cost": c,
                       "acceptable": acc, "oracle": orc})
    return oracle


def cfg_for(spec, repo, tmp, models):
    kind, _, rest = spec.partition(":")
    if kind == "config":
        return rest
    extra = ""
    if kind == "laya":   # laya:SETTINGS.toml|-:REGISTRY|-:MODEL_DIR  (settings = the branch's laya-cheap.toml: [classifier]/[routing] tables)
        sett, reg, mdir = (rest.split(":") + ["-", "-", "-"])[:3]
        backend, reg = "laya", ("" if reg == "-" else reg)
        body = Path(sett).read_text(encoding="utf-8") if sett != "-" else '[classifier]\nbackend = "laya"\nlaya_timeout_ms = 600000\n'
        if mdir != "-":
            body = body.replace("[classifier]\n", f'[classifier]\nmodel_dir = "{mdir}"\n', 1)
        extra = body
    else:
        backend, _, reg = rest.partition(":")
    registry = Path(reg or repo / "config/models.toml")
    text = registry.read_text(encoding="utf-8")
    ids = [m["id"] for m in tomllib.loads(text)["model"]]
    # keep only models measured on the held-out set eligible, so every pick is gradable
    out = tmp / f"reg-{hashlib.md5(spec.encode()).hexdigest()[:6]}.toml"
    parts = re.split(r"(?m)^(?=\[\[model\]\])", text)
    new = []
    for part in parts:
        m = re.search(r'(?m)^id = "([^"]+)"', part)
        if m and m.group(1) not in models:
            part = re.sub(r"(?m)^enabled = true", "enabled = false", part)
        new.append(part)
    out.write_text("".join(new), encoding="utf-8")
    cfg = tmp / f"cfg-{out.stem}.toml"
    head = extra or f'[classifier]\nbackend = "{ {"keywords": "local"}.get(backend.lower(), backend.lower()) }"\n'
    cfg.write_text(f'providers = ["anthropic", "openai"]\nregistry = "{out}"\n\n{head}\n[claude]\ncommand = "claude"\n\n[codex]\ncommand = "codex"\n', encoding="utf-8")
    return str(cfg)


def route(binary, cfg, prompt):
    p = subprocess.run([binary, "--config", cfg, "--stdin", "route"], input=prompt, text=True, capture_output=True, timeout=60)
    if p.returncode:
        raise RuntimeError((p.stderr or p.stdout)[-400:])
    d = json.loads(p.stdout)
    if "Laya failed to load" in d.get("reason", ""):
        raise SystemExit("policy invalid: Laya failed to load (" + d["reason"].split("·")[1].split(";")[0].strip() + "); it would silently equal keywords")
    return d["model"]


def summarize(oracle, picks):
    """A pick of a model with no held-out run on that task is a MISS and stays in the denominator (never dropped)."""
    rows = []
    for it in oracle:
        m = picks[it["task"]]
        rows.append((it["tier"], m in it["quality"], m in it["acceptable"], it["cost"].get(m), it["task"]))

    def stat(sel):
        ok = sum(x[2] for x in sel)           # acceptable implies measured
        unm = sum(not x[1] for x in sel)
        cs = [x[3] for x in sel if x[1] and x[3] is not None]
        return {"n": len(sel), "unmeasured": unm, "ok": ok, "acc": ok / len(sel) if sel else None, "ci": wilson(ok, len(sel)),
                "cost": statistics.mean(cs) if cs else None, "hits": {x[4]: bool(x[2]) for x in sel}}
    out = {"all": stat(rows)}
    for t in (1, 2, 3):
        out[f"tier{t}"] = stat([x for x in rows if x[0] == t])
    return out


def mcnemar_exact(cand_hits, base_hits):
    """Paired exact McNemar on the same tasks. Returns (b, c, p two-sided): b = cand hit/base miss, c = cand miss/base hit."""
    b = sum(cand_hits[t] and not base_hits[t] for t in cand_hits)
    c = sum(base_hits[t] and not cand_hits[t] for t in cand_hits)
    n = b + c
    if n == 0:
        return b, c, 1.0
    k = min(b, c)
    tail = sum(math.comb(n, i) for i in range(k + 1)) / 2 ** n
    return b, c, min(1.0, 2 * tail)


def verdict(p, q):
    """p, q: tier stats of candidate and baseline. REGRESSION if the CIs separate OR the paired test is significant at 95%."""
    b, c, pv = mcnemar_exact(p["hits"], q["hits"])
    if p["ci"][1] < q["ci"][0]:
        v = "REGRESSION (CI)"
    elif c > b and pv < 0.05:
        v = "REGRESSION (paired)"
    else:
        v = "point-lower" if p["acc"] < q["acc"] else "no regression"
    return v, b, c, pv


def selftest():
    n = 46
    base = {f"t{i}": i < 30 for i in range(n)}                      # 30/46 = 65.2%
    cand = dict(base)
    drop = [k for k, v in base.items() if v][:9]                    # 9 fewer hits = 19.6 pts
    for k in drop:
        cand[k] = False
    mk = lambda h: {"hits": h, "acc": sum(h.values()) / len(h), "ci": wilson(sum(h.values()), len(h))}
    v, b, c, pv = verdict(mk(cand), mk(base))
    assert (b, c) == (0, 9) and pv < 0.05 and v == "REGRESSION (paired)", (v, b, c, pv)   # CIs overlap, paired catches it
    assert mk(cand)["ci"][1] >= mk(base)["ci"][0]
    assert verdict(mk(base), mk(base))[0] == "no regression"
    # unmeasured pick counts as a miss in the denominator
    orc = [{"task": "a", "tier": 3, "quality": {"x": 1.0}, "acceptable": ["x"], "cost": {}},
           {"task": "b", "tier": 3, "quality": {"x": 1.0}, "acceptable": ["x"], "cost": {}}]
    s = summarize(orc, {"a": "x", "b": "ghost"})["tier3"]
    assert s["n"] == 2 and s["ok"] == 1 and s["unmeasured"] == 1 and s["acc"] == 0.5, s
    print("selftest ok")


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--variant", choices=["fable", "panel", "drop-self", "swap-fable"], default="fable")
    ap.add_argument("--policy", action="append", default=[], help="NAME=SPEC")
    ap.add_argument("--baselines", default="sonnet,opus,fable", help="always-X policies")
    ap.add_argument("--margin", type=float, default=0.05)
    ap.add_argument("--repo", default=str(Path(__file__).resolve().parents[2]))
    ap.add_argument("--binary", default=str(Path.home() / "nectra-eval/target/release/nectra"))
    ap.add_argument("--label", default="")
    ap.add_argument("--selftest", action="store_true")
    ap.add_argument("--json-out", default=None)
    a = ap.parse_args()
    if a.selftest:
        return selftest()
    repo = Path(a.repo)
    oracle = build_oracle(a.variant, a.margin, repo)
    tmp = Path(tempfile.mkdtemp(prefix="gate-compare-"))
    results = {}
    try:
        for spec in a.policy:
            name, _, s = spec.partition("=")
            cfg = cfg_for(s, repo, tmp, models_for(a.variant))
            picks = {it["task"]: route(a.binary, cfg, it["prompt"]) for it in oracle}
            results[name] = (summarize(oracle, picks), picks)
        for b in a.baselines.split(","):
            if b:
                results[f"always-{b}"] = (summarize(oracle, {it["task"]: b for it in oracle}), None)
    finally:
        import shutil; shutil.rmtree(tmp, ignore_errors=True)
    tiers = ["all", "tier1", "tier2", "tier3"]
    pct = lambda x: "n/a" if x is None else f"{x * 100:.1f}%"
    print(f"## Gate compare {a.label}— variant `{a.variant}`, oracle margin {a.margin}, {len(oracle)} of 200 tasks usable "
          f"(tier counts: " + ", ".join(f"T{t}={sum(1 for i in oracle if i['tier'] == t)}" for t in (1, 2, 3)) + ")\n")
    print("| policy | " + " | ".join(f"{t} acc (95% Wilson CI)" for t in tiers) + " | mean $/task |")
    print("|---|" + "---|" * (len(tiers) + 1))
    for name, (s, _) in results.items():
        cells = []
        for t in tiers:
            x = s[t]
            cells.append("n/a" if x["acc"] is None else f"{pct(x['acc'])} ({x['ok']}/{x['n']}{'; %d unmeasured=miss' % x['unmeasured'] if x['unmeasured'] else ''}; {x['ci'][0]*100:.0f}–{x['ci'][1]*100:.0f}%)")
        c = s["all"]["cost"]
        print(f"| {name} | " + " | ".join(cells) + f" | {'n/a' if c is None else f'${c:.4f}'} |")
    cand = [n for n in (x.partition('=')[0] for x in a.policy)]
    print("\nHard-tier (tier 3) check, candidate vs baseline. REGRESSION = CIs separate (candidate upper < baseline lower) OR exact "
          "McNemar on paired tasks significant at 95% with the candidate worse; b/c = tasks only the candidate / only the baseline got right:\n")
    for n in cand:
        for b_ in [k for k in results if k.startswith("always-")] + [k for k in cand if k != n]:
            p, q = results[n][0]["tier3"], results[b_][0]["tier3"]
            if p["acc"] is None or q["acc"] is None:
                continue
            v, b, c, pv = verdict(p, q)
            print(f"* {n} vs {b_}: {p['acc']*100:.1f}% vs {q['acc']*100:.1f}% ({(p['acc']-q['acc'])*100:+.1f} pts, n={p['n']}) b={b} c={c} p={pv:.3f} -> {v}")
    if a.json_out:
        Path(a.json_out).write_text(json.dumps({"variant": a.variant, "n": len(oracle), "results": {k: {t: {kk: vv for kk, vv in st.items() if kk != "hits"} for t, st in v[0].items()} for k, v in results.items()}}, indent=1))


if __name__ == "__main__":
    main()
