#!/usr/bin/env python3
"""Classifier vs plain cascade, offline on the existing rh-v2 runs (no model calls).

  python3 classifier_vs_cascade.py --data ~/nectra-calibration/eval/calibration   # rebuild rhv2-snapshot.json, then run
  python3 classifier_vs_cascade.py                                                # run from the committed snapshot

Writes CLASSIFIER-VS-CASCADE.md next to this script. Every task has exactly one run per model (rep 0), so a
cascade's outcome on a task is exact: it passes if any attempted rung passed, and its cost is the sum of the
recorded cost of every attempted rung (failed attempts cost money). Escalation trigger simulated: the recorded
objective check failed (`objective.passed` false) -> try the next rung. Cost = the run's recorded `cost_usd`
(nectra's API-equivalent: usage x list prices in eval/chat-savings/prices.json, the registry's price table)."""
import argparse, collections, glob, json, random
from pathlib import Path

HERE = Path(__file__).parent
SNAP = HERE / "rhv2-snapshot.json"
MODELS = {"luna": "gpt-5.6-luna", "haiku": "haiku", "sonnet": "sonnet", "opus": "opus"}
TIER = {"1": "simple", "2": "normal", "3": "demanding"}
BOOT, SEED = 4000, 20261115


def build_snapshot(data):
    data = Path(data)
    runs = collections.defaultdict(dict)
    for m, d in MODELS.items():
        for f in glob.glob(str(data / "results" / f"rh-v2-{d}" / "runs" / d / "*.json")):
            r = json.load(open(f)); o = r.get("objective") or {}
            if r.get("rep", 0) != 0 or (o.get("outcome") == "input_missing" and not o.get("passed")):
                continue
            c = r.get("cost_usd") if r.get("cost_usd") is not None else r.get("provider_cost_usd")
            runs[r["task"]][m] = [bool(o.get("passed")), c or 0.0, r.get("status") == "failed", r.get("answer_chars") or 0]
    pred = json.load(open(data / "predictions.json"))
    table = json.load(open(data / "results" / "rh-v2-table.nectra.json"))["entries"]
    tasks = {}
    for t in glob.glob(str(data / "tasks-v2" / "rh-*" / "task.json")):
        j = json.load(open(t))
        if j["id"] in runs and all(m in runs[j["id"]] for m in MODELS):
            p = pred.get(j["id"], {})
            tasks[j["id"]] = {"prompt_chars": len(j.get("prompt", "")), "kind": j["type"], "tier": TIER[str(j["tier"])], "family": j["id"].rsplit("-", 1)[0],
                              "runs": runs[j["id"]], "pred_kind": p.get("kind"), "pred_tier": p.get("tier"),
                              "pred_difficulty": p.get("difficulty")}
    snap = {"tasks": tasks, "table": [[e["kind"], e["tier"], [r["model"] for r in e["ladder"]]] for e in table]}
    json.dump(snap, open(SNAP, "w"), indent=1, sort_keys=True)


def main():
    ap = argparse.ArgumentParser(); ap.add_argument("--data"); a = ap.parse_args()
    if a.data: build_snapshot(a.data)
    S = json.load(open(SNAP)); T = S["tasks"]; ids = sorted(T)
    short = {"gpt-5.6-luna": "luna", "gpt-5.6-sol": "sol", "gpt-6-astra": "astra"}
    table = {(k, t): [short.get(m, m) for m in l] for k, t, l in S["table"]}
    # Cost order = mean recorded cost per run (cheapest first).
    mean = {m: sum(T[i]["runs"][m][1] for i in ids) / len(ids) for m in MODELS}
    order = sorted(MODELS, key=lambda m: mean[m]); assert order == ["luna", "haiku", "sonnet", "opus"], order

    def cascade(i, ladder):
        spent, tried = 0.0, 0
        for m in ladder:
            ok, c = T[i]["runs"][m][:2]; spent += c; tried += 1
            if ok: return True, spent, tried, m
        return False, spent, tried, None

    def up(start): return order[order.index(start):]
    def ladder_laya_table(i):
        t = T[i]; l = table.get((t["pred_kind"], t["pred_tier"]))
        return [m for m in l if m in MODELS] if l and t["pred_tier"] else up("haiku")  # unmeasured/unclassified -> haiku-first
    START = {"simple": "luna", "normal": "haiku", "demanding": "sonnet"}
    def ladder_tier(i, key):
        tier = T[i][key]; return up(START[tier]) if tier in START else up("haiku")
    cheapest_pass = {}
    for i in ids:
        ps = [(T[i]["runs"][m][1], m) for m in order if T[i]["runs"][m][0]]
        cheapest_pass[i] = min(ps) if ps else None
    def oracle(i):
        cp = cheapest_pass[i]
        return (True, cp[0], 1, cp[1]) if cp else (False, min(T[i]["runs"][m][1] for m in MODELS), 1, None)

    policies = [
        ("laya + table as on master (certified, opus-only)", lambda i: cascade(i, ladder_laya_table(i))),
        ("laya tier-start (simple luna / normal haiku / demanding sonnet, predicted)", lambda i: cascade(i, ladder_tier(i, "pred_tier"))),
        ("tier-start with authored tier (upper bound for that rule)", lambda i: cascade(i, ladder_tier(i, "tier"))),
        ("haiku-first cascade", lambda i: cascade(i, up("haiku"))),
        ("luna-first cascade", lambda i: cascade(i, up("luna"))),
        ("luna-first, capped at 2 escalations (luna>haiku>sonnet)", lambda i: cascade(i, up("luna")[:3])),
        ("sonnet-first cascade", lambda i: cascade(i, up("sonnet"))),
        ("always-opus", lambda i: cascade(i, ["opus"])),
        ("oracle (cheapest model that passed)", oracle),
    ]
    res = {name: {i: fn(i) for i in ids} for name, fn in policies}

    fams = collections.defaultdict(list)
    for i in ids: fams[T[i]["family"]].append(i)
    rng = random.Random(SEED)
    def boot(stat, cluster):
        units = list(fams.values()) if cluster else [[i] for i in ids]
        out = []
        for _ in range(BOOT):
            s = [i for u in (rng.choice(units) for _ in units) for i in u]
            out.append(stat(s))
        out.sort(); return out[int(.025 * BOOT)], out[int(.975 * BOOT) - 1]
    def ci(stat):
        a, b = boot(stat, False); c, d = boot(stat, True)
        return a, b, c, d

    rows = []
    for name, _ in policies:
        r = res[name]
        pas = lambda s: sum(r[i][0] for i in s) / len(s)
        cst = lambda s: sum(r[i][1] for i in s) / len(s)
        n = len(ids)
        esc = sum(r[i][2] > 1 for i in ids) / n
        under = sum((not r[i][0]) and cheapest_pass[i] is not None for i in ids)      # a cheaper-or-equal pass existed
        nobody = sum(cheapest_pass[i] is None for i in ids)
        failcost_att = sum(r[i][1] - (r[i][1] if r[i][3] is None else T[i]["runs"][r[i][3]][1]) for i in ids if r[i][0])
        nobody_cost = sum(r[i][1] for i in ids if cheapest_pass[i] is None) / n
        over = sum((T[i]["runs"][r[i][3]][1] - cheapest_pass[i][0]) for i in ids if r[i][0])
        rows.append(dict(name=name, passn=sum(r[i][0] for i in ids), pass_ci=ci(pas), cost=cst(ids), cost_ci=ci(cst), esc=esc,
                         under=under, nobody=nobody, attempt_cost=failcost_att / n, nobody_cost=nobody_cost, over=over / n))

    def fmt(x): return f"{x:.4f}"
    L = []; w = L.append
    w("# Classifier vs cascade (offline, rh-v2, no model calls)\n")
    w(f"Script: `classifier_vs_cascade.py`; data: `rhv2-snapshot.json` (per-task pass/cost for four models, Laya's predicted kind/tier/difficulty from `nectra route`, the imported table). {len(ids)} tasks, one run per model (rep 0).\n")
    w("## Method and assumptions\n")
    w("- Escalation trigger: the recorded objective check failed -> next rung. A cascade's outcome is exact (every task has one recorded run per model, so no independence assumption); a cascade passes if any attempted rung passed.")
    w("- Cost = sum of the recorded `cost_usd` of **every attempted rung**, failed ones included. `cost_usd` is nectra's API-equivalent (usage x list prices, `eval/chat-savings/prices.json`, the registry's price table). Subscription capacity and retry latency are not in it. Mean cost per run: " + ", ".join(f"{m} ${mean[m]:.4f}" for m in order) + ". Cascades climb in that cost order.")
    w("- **Detection is free here**: the cascade sees the objective check result. In real use `Completed` is not success, so the real escalation signal is weaker and every cascade row is optimistic by that amount (the classifier-start rows less so, since they escalate less).")
    w("- 'laya + table as on master' uses the imported table (`rh-v2-table.nectra.json`): non-inferiority certified, which at ~22 tasks per kind leaves **opus only** for every measured kind; unmeasured kinds (architecture, data_analysis, general) and the 27 prompts Laya did not classify fall back to haiku-first. The tier-start rule is a fixed, unfitted rule (no in-sample table), so it is not tuned to these runs.")
    w("- Oracle = the cheapest model whose run passed (cost = that single run); on tasks nobody passed it counts a failure at the cheapest run's cost. It is a lower bound no policy can reach.")
    w("- **Clustering.** The rh-v2 data carries no project identifier (tasks are standalone fixtures). The only grouping available is the task-id family (rh-code, rh-debug, ... = the authored kind, 9 clusters of 21-23). CIs below are 95% bootstrap percentile intervals, 4000 resamples, seed fixed, shown both **task-level** and **clustered by id family**. Nine clusters is few, so the clustered intervals are rough and the task-level ones are probably too tight; read the pair as a range.")
    w("- **Noise data.** `noise-repeat-20261112` holds no complete tasks (every model 0/0), so no flip-rate correction is applied. At n=194 a 1-2 task difference is noise regardless.\n")
    w("## Policies\n")
    w("| policy | pass | pass 95% CI (task / family) | $/task | $/task 95% CI (task / family) | escalation rate |")
    w("|---|---:|---|---:|---|---:|")
    for r in rows:
        a, b, c, d = r["pass_ci"]; e, f, g, h = r["cost_ci"]
        w(f"| {r['name']} | {r['passn']}/{len(ids)} ({r['passn']/len(ids):.1%}) | {a:.3f}-{b:.3f} / {c:.3f}-{d:.3f} | {fmt(r['cost'])} | {e:.4f}-{f:.4f} / {g:.4f}-{h:.4f} | {r['esc']:.0%} |")
    w("\n## Where the money and the failures go\n")
    w("Under-routing = tasks the policy failed although some model passed them (cost: lost quality). Nobody-passed tasks are failures no routing could fix. Over-routing waste = $/task spent above the cheapest passing model, split into **failed attempts** (cascade steps that did not pass) and **overshoot** (the final passing model dearer than the cheapest passing one). A third cost is **spent on tasks no model passes**: a cascade climbs the whole ladder on them. No routing can fix those tasks, but a cap or a stop rule can stop paying for them.\n")
    w("| policy | under-routing fails | nobody passed | failed-attempt cost $/task | overshoot $/task | spent on nobody-passed tasks $/task |")
    w("|---|---:|---:|---:|---:|---:|")
    for r in rows:
        w(f"| {r['name']} | {r['under']} | {r['nobody']} | {fmt(r['attempt_cost'])} | {fmt(r['over'])} | {fmt(r['nobody_cost'])} |")
    w("\n## Per authored kind (pass / $ per task; n<20 flagged)\n")
    pol_short = [n for n, _ in policies]
    w("; ".join(f"P{k+1} = {p}" for k, p in enumerate(pol_short)) + "\n")
    w("| kind | n | " + " | ".join(f"P{k+1}" for k in range(len(pol_short))) + " |")
    w("|---|---:|" + "---:|" * len(pol_short))
    kinds = sorted({T[i]["kind"] for i in ids})
    for k in kinds:
        s = [i for i in ids if T[i]["kind"] == k]; n = len(s)
        cells = []
        for name in pol_short:
            r = res[name]; cells.append(f"{sum(r[i][0] for i in s)}/{n} ${sum(r[i][1] for i in s)/n:.3f}")
        w(f"| {k} | {n}{' ⚠ n<20' if n < 20 else ''} | " + " | ".join(cells) + " |")
    # --- Where could a classifier win?
    w("\n## Where a classifier could beat the cascade\n")
    hf = res["haiku-first cascade"]; lf = res["luna-first cascade"]
    def rung_stats(m):
        ps = [i for i in ids if T[i]["runs"][m][0]]; fs = [i for i in ids if not T[i]["runs"][m][0]]
        return len(ps), len(fs), sum(T[i]["runs"][m][1] for i in fs), mean[m]
    nxt = dict(zip(order, order[1:]))
    w("Skipping a cheap rung pays only on tasks that rung fails (saves that rung's cost) and loses on tasks it would have passed (pays the next rung's price). A classifier that flags 'will fail on rung R' with precision p breaks even at p* = (c_next - c_R) / c_next, using mean costs.\n")
    w("| rung R | passes | fails | cost of its failed attempts, $/task (the most skipping can save) | c_R | c_next | break-even precision p* | actual fail share |")
    w("|---|---:|---:|---:|---:|---:|---:|---:|")
    for m in ("luna", "haiku", "sonnet"):
        p, f, fc, c = rung_stats(m); cn = mean[nxt[m]]
        w(f"| {m} | {p} | {f} | {fc/len(ids):.4f} | {c:.4f} | {cn:.4f} | {(cn-c)/cn:.0%} | {f/len(ids):.0%} |")
    # AUC of Laya difficulty for 'haiku fails' and 'luna fails'
    def auc(score, label):
        pos = [score[i] for i in ids if label[i] and score.get(i) is not None]; neg = [score[i] for i in ids if not label[i] and score.get(i) is not None]
        if not pos or not neg: return None
        gt = sum((p > q) + 0.5 * (p == q) for p in pos for q in neg); return gt / (len(pos) * len(neg))
    diff = {i: T[i]["pred_difficulty"] for i in ids if T[i]["pred_difficulty"] is not None}
    authored = {i: int(next(k for k, v in TIER.items() if v == T[i]["tier"])) for i in ids}
    w("\nDoes Laya's signal predict where a rung fails? AUC (0.5 = no information) of the signal for 'the rung's run failed':\n")
    w("| signal | luna fails | haiku fails | sonnet fails | n |")
    w("|---|---:|---:|---:|---:|")
    def au(sig, m):
        v = auc(sig, {i: not T[i]["runs"][m][0] for i in ids}); return "n/a" if v is None else f"{v:.2f}"
    w(f"| Laya predicted difficulty | {au(diff,'luna')} | {au(diff,'haiku')} | {au(diff,'sonnet')} | {len(diff)} |")
    w(f"| authored tier (human label) | {au(authored,'luna')} | {au(authored,'haiku')} | {au(authored,'sonnet')} | {len(ids)} |")
    agree_k = sum(T[i]["pred_kind"] == T[i]["kind"] for i in ids); agree_t = sum(T[i]["pred_tier"] == T[i]["tier"] for i in ids)
    w(f"\nLaya vs authored labels: kind {agree_k}/{len(ids)}, tier {agree_t}/{len(ids)}; {len(ids) - len(diff)} prompts unclassified (keyword fallback).")

    # ---- Follow-up 2: what nectra's own escalation signal sees
    w("\n## Free failure detection: what nectra's real signal sees\n")
    w("What triggers an escalation in nectra today (`src/chat.rs`, `src/headless.rs`): a **failed turn** (the provider run errored: deadline, rate limit, tool error) steps the task up the ladder; the user can ask for a stronger model (`/model`, a demanding follow-up); and a headless caller passes `--escalate-from` (optionally with `--prior-verified`, a verifier verdict). **Nothing in nectra judges whether a completed answer is right** (`Completed` is not success). In these runs the only recorded signal of that kind is `status == failed` (`error_class`). The objective check is a grader the real system does not have.\n")
    w("Confusion matrix per rung: 'nectra would escalate' = the run's turn failed; truth = the objective check failed (the run did not pass).\n")
    w("| rung | runs | escalate & check failed | escalate & check passed | no escalate & check failed (missed) | no escalate & check passed | recall of check failures |")
    w("|---|---:|---:|---:|---:|---:|---:|")
    for m in order:
        tp = sum(1 for i in ids if T[i]["runs"][m][2] and not T[i]["runs"][m][0]); fp = sum(1 for i in ids if T[i]["runs"][m][2] and T[i]["runs"][m][0])
        fn = sum(1 for i in ids if not T[i]["runs"][m][2] and not T[i]["runs"][m][0]); tn = sum(1 for i in ids if not T[i]["runs"][m][2] and T[i]["runs"][m][0])
        w(f"| {m} | {len(ids)} | {tp} | {fp} | {fn} | {tn} | {tp/(tp+fn):.1%} |")
    w("\nSo on these runs the real signal fires on a handful of deadline failures and misses essentially every wrong answer. Cascades re-run with it (a failed turn always escalates; otherwise a wrong answer is accepted with probability 1-d, where d is the share of wrong answers some detector would catch; false alarms assumed zero; expected values are exact, not sampled). d=1 is the oracle check used above; 'real' is d=0 plus the failed-turn trigger.\n")
    def expected(i, ladder, d):
        pp, cost, alive = 0.0, 0.0, 1.0
        for m in ladder:
            ok, c, failed = T[i]["runs"][m][:3]; cost += alive * c
            if ok: pp += alive; alive = 0; break
            alive *= 1.0 if failed else d
        return pp, cost
    w("| cascade | detection of wrong answers | pass (expected) | pass 95% CI (task / family) | $/task | $/task 95% CI (task / family) |")
    w("|---|---|---:|---|---:|---|")
    for lname, start in (("luna-first", "luna"), ("haiku-first", "haiku")):
        for label, d in (("oracle check (d=1)", 1.0), ("d=0.75", .75), ("d=0.5", .5), ("d=0.25", .25), ("**real: failed turns only**", 0.0)):
            ev = {i: expected(i, up(start), d) for i in ids}
            pas = lambda s, ev=ev: sum(ev[i][0] for i in s) / len(s); cst = lambda s, ev=ev: sum(ev[i][1] for i in s) / len(s)
            a, b, c2, d2 = ci(pas); e, f, g, h = ci(cst)
            w(f"| {lname} | {label} | {pas(ids)*len(ids):.1f}/{len(ids)} ({pas(ids):.1%}) | {a:.3f}-{b:.3f} / {c2:.3f}-{d2:.3f} | {cst(ids):.4f} | {e:.4f}-{f:.4f} / {g:.4f}-{h:.4f} |")
    first = {m: sum(T[i]["runs"][m][0] for i in ids) for m in order}
    w(f"\nWith the real signal a cascade is just its first rung (luna alone {first['luna']}/{len(ids)}, haiku alone {first['haiku']}/{len(ids)}): the cascade's whole pass-rate advantage comes from a detector the system does not have. Any real deployment needs a verifier (tests, a grader model, the user), and its recall d sets the result between the d=0 and d=1 rows.\n")
    # verifier cost: grade the saved answers
    prices = json.load(open(HERE.parent / "chat-savings" / "prices.json"))
    ans_tok = sum(T[i]["runs"][m][3] for i in ids for m in MODELS) / 4; prm_tok = sum(T[i]["prompt_chars"] for i in ids) * len(MODELS) / 4
    n_runs = len(ids) * len(MODELS); in_tok = ans_tok + prm_tok + 400 * n_runs; out_tok = 150 * n_runs   # +400 rubric/instructions, 150 verdict tokens per answer
    w("**What is missing, and the minimal re-run.** Not recorded: any automated verdict on a completed answer (a verifier result, a test run by nectra, a user reaction). Recorded and usable: `status`, `error_class`, `answer_chars`, the answer text (`*.answer.md`) and the objective check. To measure a real verifier's recall d and false-alarm rate, no task or model needs re-running: grade the saved answers. "
      f"{n_runs} answers, about {in_tok/1e6:.2f}M input and {out_tok/1e6:.2f}M output tokens (chars/4, 400 tokens of rubric and 150 of verdict per answer). At the registry's list prices the whole set costs, by grader: " +
      "; ".join(f"{m} ${prices[m]['input']*in_tok/1e6 + prices[m]['output']*out_tok/1e6:.2f}" for m in ("haiku", "sonnet", "opus")) +
      ". Comparing each verdict with the objective check gives d and the false-alarm rate to plug into the table above.\n")
    # ---- Follow-up 3: stop rule
    w("## Stop rule: capping escalations\n")
    w("Oracle check as the trigger. 'Nobody-passed' = the 20 tasks no model passes; they climb the whole ladder under no cap. Pass lost = tasks the uncapped cascade passes and the capped one does not.\n")
    w("| cascade | cap (max escalations) | pass | pass lost vs no cap | $/task | $/task saved | spent on nobody-passed $/task | saved there |")
    w("|---|---:|---:|---:|---:|---:|---:|---:|")
    for lname, start in (("luna-first", "luna"), ("haiku-first", "haiku")):
        full = up(start); base = {i: cascade(i, full) for i in ids}; bp = sum(base[i][0] for i in ids); bc = sum(base[i][1] for i in ids) / len(ids)
        bn = sum(base[i][1] for i in ids if cheapest_pass[i] is None) / len(ids)
        for cap in sorted({0, 1, 2, len(full) - 1}):
            lad = full[: cap + 1]; r = {i: cascade(i, lad) for i in ids}
            pp = sum(r[i][0] for i in ids); cc = sum(r[i][1] for i in ids) / len(ids); nn = sum(r[i][1] for i in ids if cheapest_pass[i] is None) / len(ids)
            tag = f"{cap}" + (" (= no cap)" if cap == len(full) - 1 else "")
            w(f"| {lname} | {tag} | {pp}/{len(ids)} | {bp-pp} | {cc:.4f} | {bc-cc:.4f} | {nn:.4f} | {bn-nn:.4f} |")


    # ---- Follow-up 4: measured verifiers (graded saved answers)
    import math
    gfiles = sorted(HERE.glob("verifier-grades-*.json"))
    def wilson(k, n):
        if n == 0: return (float("nan"),) * 2
        z, ph = 1.96, k / n; den = 1 + z * z / n; c = (ph + z * z / (2 * n)) / den; h = z * math.sqrt(ph * (1 - ph) / n + z * z / (4 * n * n)) / den
        return c - h, c + h
    if gfiles:
        G = {f.stem[len("verifier-grades-"):]: json.load(open(f)) for f in gfiles}
        w("\n## Measured verifiers: grading the saved answers\n")
        w("Each grader saw only the task prompt, the task's fixture files, the human rubric text and the answer (`verifier-prompt.md`, same for every grader, run through the `claude` CLI with no tools). It never saw the objective check, `must_labels`, the model name or any nectra metadata. Model/provider names in the answer text were redacted unless the task or its files use the same word (`verifier-redactions.json` logs every redaction with context). Verdict FAIL = the grader would escalate. **d** = P(FAIL | objective check failed) (recall of wrong answers); **false alarm** = P(FAIL | objective check passed). Intervals are Wilson 95%. Grader ERROR verdicts (unparseable) are excluded from rates and treated as accept in cascades. `n<20` cells are flagged.\n")
        red = json.load(open(HERE / "verifier-redactions.json")) if (HERE / "verifier-redactions.json").exists() else {}
        w(f"Blinding log: {sum(1 for v in red.values() if v)} of {len(red)} graded answers needed a redaction ({sum(len(v) for v in red.values())} strings; mostly product or model names inside answers). Self-identification phrases are in the log, not hidden.\n")
        def verd(g, i, m): return G[g].get(f"{i}|{m}", {}).get("verdict")
        w("### Recall and false alarms per producing model (self-graded shown separately from cross-graded)\n")
        w("| grader | producing model | self/cross | wrong answers n | d (recall) | 95% CI | correct answers n | false alarm | 95% CI | errors |")
        w("|---|---|---|---:|---:|---|---:|---:|---|---:|")
        selfmap = {"claude-haiku-5-5": "haiku", "claude-sonnet-5-5": "sonnet", "claude-opus-5-5": "opus"}
        for g in G:
            for m in order:
                bad = [i for i in ids if not T[i]["runs"][m][0] and verd(g, i, m) in ("PASS", "FAIL")]; good = [i for i in ids if T[i]["runs"][m][0] and verd(g, i, m) in ("PASS", "FAIL")]
                if not bad and not good: continue
                kb = sum(verd(g, i, m) == "FAIL" for i in bad); kg = sum(verd(g, i, m) == "FAIL" for i in good)
                err = sum(verd(g, i, m) == "ERROR" for i in ids)
                lo, hi = wilson(kb, len(bad)); lo2, hi2 = wilson(kg, len(good))
                kind = "SELF" if selfmap.get(g) == m else "cross"
                w(f"| {g} | {m} | {kind} | {len(bad)}{' ⚠ n<20' if len(bad) < 20 else ''} | {kb/max(len(bad),1):.1%} | {lo:.2f}-{hi:.2f} | {len(good)} | {kg/max(len(good),1):.1%} | {lo2:.2f}-{hi2:.2f} | {err} |")
        w("\nPooled over all producing models, and split self vs cross:\n")
        w("| grader | scope | wrong n | d | correct n | false alarm |")
        w("|---|---|---:|---:|---:|---:|")
        for g in G:
            for scope, ms in (("all", order), ("self only", [m for m in order if selfmap.get(g) == m]), ("cross only", [m for m in order if selfmap.get(g) != m])):
                pairs = [(i, m) for m in ms for i in ids if verd(g, i, m) in ("PASS", "FAIL")]
                bad = [(i, m) for i, m in pairs if not T[i]["runs"][m][0]]; good = [(i, m) for i, m in pairs if T[i]["runs"][m][0]]
                if not pairs: continue
                kb = sum(verd(g, i, m) == "FAIL" for i, m in bad); kg = sum(verd(g, i, m) == "FAIL" for i, m in good)
                lo, hi = wilson(kb, len(bad)); lo2, hi2 = wilson(kg, len(good))
                w(f"| {g} | {scope} | {len(bad)} | {kb/max(len(bad),1):.1%} ({lo:.2f}-{hi:.2f}) | {len(good)} | {kg/max(len(good),1):.1%} ({lo2:.2f}-{hi2:.2f}) |")
        w("\n### Per authored kind (all producing models pooled; n = wrong answers / correct answers; n<20 flagged)\n")
        w("| grader | kind | wrong n | d | correct n | false alarm |")
        w("|---|---|---:|---:|---:|---:|")
        for g in G:
            for k in kinds:
                pairs = [(i, m) for m in order for i in ids if T[i]["kind"] == k and verd(g, i, m) in ("PASS", "FAIL")]
                bad = [(i, m) for i, m in pairs if not T[i]["runs"][m][0]]; good = [(i, m) for i, m in pairs if T[i]["runs"][m][0]]
                if not pairs: continue
                kb = sum(verd(g, i, m) == "FAIL" for i, m in bad); kg = sum(verd(g, i, m) == "FAIL" for i, m in good)
                w(f"| {g} | {k} | {len(bad)}{' ⚠ n<20' if len(bad) < 20 else ''} | {kb/max(len(bad),1):.0%} | {len(good)}{' ⚠ n<20' if len(good) < 20 else ''} | {kg/max(len(good),1):.0%} |")
        w("\n### Grader cost, tokens and latency actually used\n")
        w("Cost is what the CLI reported per call (list-price equivalent; it ran on the subscription). Tokens include cache reads and writes. Latency = wall seconds per graded answer.\n")
        w("| grader | answers graded | total cost | $/answer | input tokens | output tokens | latency p50 / p90 / max (s) |")
        w("|---|---:|---:|---:|---:|---:|---|")
        def q(xs, p): xs = sorted(xs); return xs[min(len(xs) - 1, int(p * len(xs)))]
        gcost = {}
        for g in G:
            v = list(G[g].values()); ws = [x["wall_s"] for x in v]
            gcost[g] = sum(x["cost_usd"] for x in v) / len(v)
            w(f"| {g} | {len(v)} | ${sum(x['cost_usd'] for x in v):.2f} | {gcost[g]:.4f} | {sum(x['in_tok'] for x in v):,} | {sum(x['out_tok'] for x in v):,} | {q(ws,.5):.1f} / {q(ws,.9):.1f} / {max(ws):.1f} |")
        w("\n### Cascades driven by the measured verdicts\n")
        w("Exact replay: at each rung except the last, the grader's actual verdict on that actual answer decides (FAIL, or a failed turn, escalates; a false alarm escalates a correct answer, so it costs money and does not lose a pass; a missed wrong answer is accepted and counts as a failed pass). Cost = the runs + the grader's actual cost for each graded answer, added to every graded rung. Added seconds = the grader's wall time on each graded answer (the model's own run time is not counted). Graders only cover the answers they graded; tasks with a missing verdict are dropped from that row (n shown).\n")
        def replay(i, ladder, g):
            spent = lat = 0.0
            for k, m in enumerate(ladder):
                ok, c, failed = T[i]["runs"][m][:3]; spent += c
                if k == len(ladder) - 1: return ok, spent, lat
                if failed: continue
                gr = G[g].get(f"{i}|{m}")
                if gr is None: return None
                spent += gr["cost_usd"]; lat += gr["wall_s"]
                if gr["verdict"] != "FAIL": return ok, spent, lat
            return ok, spent, lat
        w("| cascade | signal | tasks | pass | pass 95% CI (task / family) | $/task | $/task 95% CI (task / family) | added grader s/task |")
        w("|---|---|---:|---:|---|---:|---|---:|")
        ref = {}
        for lname, start in (("luna-first", "luna"), ("haiku-first", "haiku")):
            lad = up(start)
            rowsx = [("real: failed turns only (d=0)", None), ("objective check (d=1, free)", "oracle")] + [(f"{g} (measured)", g) for g in G]
            for label, g in rowsx:
                if g is None: ev = {i: expected(i, lad, 0.0) + (0.0,) for i in ids}
                elif g == "oracle": ev = {i: cascade(i, lad)[:2] + (0.0,) for i in ids}
                else: ev = {i: replay(i, lad, g) for i in ids}
                ev = {i: (float(v[0]), v[1], v[2]) for i, v in ev.items() if v is not None}
                ss = sorted(ev); n_ = len(ss)
                pas = lambda s, ev=ev: sum(ev[i][0] for i in s if i in ev) / max(1, sum(1 for i in s if i in ev)); cst = lambda s, ev=ev: sum(ev[i][1] for i in s if i in ev) / max(1, sum(1 for i in s if i in ev))
                a, b, c2, d2 = ci(pas); e, f, gg, h = ci(cst)
                latm = sum(ev[i][2] for i in ss) / n_
                ref[(lname, label)] = (pas(ss), cst(ss), latm)
                w(f"| {lname} | {label} | {n_} | {pas(ss)*n_:.0f}/{n_} ({pas(ss):.1%}) | {a:.3f}-{b:.3f} / {c2:.3f}-{d2:.3f} | {cst(ss):.4f} | {e:.4f}-{f:.4f} / {gg:.4f}-{h:.4f} | {latm:.1f} |")
        w("\nReference rows (no verifier): " + "; ".join(f"{r['name'].split(' (')[0]} {r['passn']}/{len(ids)} at ${r['cost']:.4f}" for r in rows if r['name'].split(' (')[0] in ("laya tier-start", "laya + table as on master", "always-opus")) + f"; haiku alone {first['haiku']}/{len(ids)} at ${mean['haiku']:.4f}; luna alone {first['luna']}/{len(ids)} at ${mean['luna']:.4f}.\n")

        # ---- Like for like: Laya policies under realistic signals
        w("\n### Like for like: Laya policies and single-model references under realistic signals\n")
        w("The earlier Laya rows (tier-start 170/194, table on master 155/194) escalate on the **free objective check**. Here every escalating policy is replayed with the same signals and the same method as the cascades above: real (failed turns only, d=0) and each measured grader (haiku, sonnet; the opus sample covers too few answers). 'Start rung only' rows never escalate and need no signal. CIs are task-level / family-clustered; added s/task is grader wall time.\n")
        def mk_ladders():
            return [("laya tier-start (predicted tier)", lambda i: ladder_tier(i, "pred_tier")),
                    ("laya + table on master (certified, opus-only)", ladder_laya_table),
                    ("tier-start with authored tier", lambda i: ladder_tier(i, "tier"))]
        w("| policy | signal | tasks | pass | pass 95% CI (task / family) | $/task | $/task 95% CI (task / family) | added grader s/task |")
        w("|---|---|---:|---:|---|---:|---|---:|")
        ref2 = {}
        def emit(name, label, ev):
            ev = {i: (float(v[0]), v[1], v[2]) for i, v in ev.items() if v is not None}
            ss = sorted(ev); n_ = len(ss)
            pas = lambda s: sum(ev[i][0] for i in s if i in ev) / max(1, sum(1 for i in s if i in ev)); cst = lambda s: sum(ev[i][1] for i in s if i in ev) / max(1, sum(1 for i in s if i in ev))
            a, b, c2, d2 = ci(pas); e, f, gg, h = ci(cst); latm = sum(ev[i][2] for i in ss) / n_
            ref2[(name, label)] = (pas(ss), cst(ss), latm, n_)
            w(f"| {name} | {label} | {n_} | {pas(ss)*n_:.0f}/{n_} ({pas(ss):.1%}) | {a:.3f}-{b:.3f} / {c2:.3f}-{d2:.3f} | {cst(ss):.4f} | {e:.4f}-{f:.4f} / {gg:.4f}-{h:.4f} | {latm:.1f} |")
        gs = [g for g in ("claude-haiku-5-5", "claude-sonnet-5-5") if g in G]
        for name, lf in mk_ladders():
            emit(name, "free objective check (d=1; earlier rows)", {i: cascade(i, lf(i))[:2] + (0.0,) for i in ids})
            emit(name, "real: failed turns only (d=0)", {i: expected(i, lf(i), 0.0) + (0.0,) for i in ids})
            for g in gs: emit(name, f"{g} (measured)", {i: replay(i, lf(i), g) for i in ids})
        for name, lf in (("laya tier-start, start rung only (no escalation)", lambda i: ladder_tier(i, "pred_tier")[:1]),
                         ("laya + table on master, start rung only", lambda i: ladder_laya_table(i)[:1]),
                         ("always-luna", lambda i: ["luna"]), ("always-haiku", lambda i: ["haiku"]),
                         ("always-sonnet", lambda i: ["sonnet"]), ("always-opus", lambda i: ["opus"])):
            emit(name, "no escalation", {i: cascade(i, lf(i))[:2] + (0.0,) for i in ids})
        def R2(n, l): return ref2[(n, l)]
        pick = [(k, v) for k, v in ref2.items() if "free objective" not in k[1] and k[1] != "real: failed turns only (d=0)" or k[1] == "real: failed turns only (d=0)"]
        real_pool = {k: v for k, v in ref2.items() if "free objective" not in k[1] and v[3] == len(ids)}
        w("\n### Restated conclusion at real-world signals\n")
        best_pass = max(real_pool.items(), key=lambda kv: (round(kv[1][0], 6), -kv[1][1]))
        best_cost_at = lambda floor: min((kv for kv in real_pool.items() if kv[1][0] >= floor), key=lambda kv: kv[1][1])
        hk, sn = R2("always-haiku", "no escalation"), R2("always-sonnet", "no escalation")
        ts_real, ts_h, ts_s = R2("laya tier-start (predicted tier)", "real: failed turns only (d=0)"), R2("laya tier-start (predicted tier)", "claude-haiku-5-5 (measured)"), R2("laya tier-start (predicted tier)", "claude-sonnet-5-5 (measured)")
        tr = R2("laya tier-start, start rung only (no escalation)", "no escalation")
        mr = R2("laya + table on master", "real: failed turns only (d=0)") if ("laya + table on master", "real: failed turns only (d=0)") in ref2 else R2("laya + table on master (certified, opus-only)", "real: failed turns only (d=0)")
        f_ = lambda v: f"{v[0]*len(ids):.0f}/{len(ids)} at ${v[1]:.4f}/task" + (f", +{v[2]:.0f} s" if v[2] else "")
        top = best_pass[1][0]
        value = min((kv for kv in real_pool.items() if kv[1][0] >= top - 2 / len(ids)), key=lambda kv: kv[1][1])
        w(f"- **Best at real-world signals (every task covered):** the highest pass is {best_pass[0][0]} / {best_pass[0][1]} at {f_(best_pass[1])}. Within two tasks of it (noise at n=194), the cheapest and fastest policy is **{value[0][0]} / {value[0][1]}**: {f_(value[1])}, no grader latency. Always-haiku: {f_(hk)}; always-sonnet: {f_(sn)}. Every escalating policy with a real signal (failed turns, or a haiku/sonnet grader) lands at 73-77% pass, no better than always-haiku, at 4-20x its cost, plus about 10 s of grader latency per task when a grader is used.")
        w(f"- **Laya's tier pick, like for like.** Tier-start with the real signal (it just runs its start rung): {f_(ts_real)}; with a haiku grader {f_(ts_h)}; with a sonnet grader {f_(ts_s)}; start rung only {f_(tr)}. Compare always-haiku ({f_(hk)}) and always-sonnet ({f_(sn)}): the tier pick adds {'something' if ts_real[0] > max(hk[0], sn[0]) + 2 / len(ids) else 'nothing measurable'} over the best single model once escalation is realistic. The 170/194 earlier figure came from the free check (a 27% escalation rate nobody can observe in production), not from the tier pick.")
        w(f"- **The table on master** (opus-only for measured kinds): {f_(mr)} with the real signal; escalation barely matters for it (it mostly runs opus once). It is dominated by always-haiku and always-sonnet on both pass and cost.")

        # break-even
        w("### Break-even\n")
        w("Zero-false-alarm recall d that luna-first and haiku-first need (grader cost excluded) to reach the pass of: haiku alone, haiku-first with the free check, and sonnet-first with the free check:\n")
        w("| cascade | to match haiku alone | to match haiku-first, free check | to match sonnet-first, free check |")
        w("|---|---:|---:|---:|")
        tgt = {"haiku alone": first["haiku"], "haiku-first free": next(r["passn"] for r in rows if r["name"] == "haiku-first cascade"), "sonnet-first": next(r["passn"] for r in rows if r["name"] == "sonnet-first cascade")}
        for lname, start in (("luna-first", "luna"), ("haiku-first", "haiku")):
            cells = []
            for tn, tv in tgt.items():
                hit = next((x / 100 for x in range(0, 101) if sum(expected(i, up(start), x / 100)[0] for i in ids) >= tv - 1e-9), None)
                cells.append("not reachable" if hit is None else f"d >= {hit:.2f}")
            w(f"| {lname} | " + " | ".join(cells) + " |")
        w("\nMaximum grader cost per graded answer at which each measured cascade still costs no more than Laya tier-start ($" + f"{next(r['cost'] for r in rows if r['name'].startswith('laya tier-start')):.4f}" + "/task) and than haiku-first with the free check ($" + f"{next(r['cost'] for r in rows if r['name'] == 'haiku-first cascade'):.4f}" + "/task); graded answers per task come from the replay:\n")
        w("| cascade | grader | run cost $/task (grader excluded) | graded answers/task | max grader $/answer vs Laya tier-start | max grader $/answer vs haiku-first free | measured grader $/answer |")
        w("|---|---|---:|---:|---:|---:|---:|")
        lay = next(r["cost"] for r in rows if r["name"].startswith("laya tier-start")); hfree = next(r["cost"] for r in rows if r["name"] == "haiku-first cascade")
        for lname, start in (("luna-first", "luna"), ("haiku-first", "haiku")):
            lad = up(start)
            for g in G:
                rr = [replay(i, lad, g) for i in ids]; rr = [x for x in rr if x]
                if not rr: continue
                graded = 0.0; runc = 0.0
                for i in ids:
                    spent = 0.0; k_ = 0
                    for k, m in enumerate(lad):
                        ok, c, failed = T[i]["runs"][m][:3]; spent += c
                        if k == len(lad) - 1: break
                        if failed: continue
                        k_ += 1
                        gr = G[g].get(f"{i}|{m}")
                        if gr is None or gr["verdict"] != "FAIL": break
                    graded += k_; runc += spent
                graded /= len(ids); runc /= len(ids)
                w(f"| {lname} | {g} | {runc:.4f} | {graded:.2f} | {max(0,(lay-runc)/graded):.4f} | {max(0,(hfree-runc)/graded):.4f} | {gcost[g]:.4f} |")

        # verifier conclusion
        def rf(l, g): return ref[(l, f"{g} (measured)")]
        hh, hs, lh, ls = rf("haiku-first", "claude-haiku-5-5"), rf("haiku-first", "claude-sonnet-5-5"), rf("luna-first", "claude-haiku-5-5"), rf("luna-first", "claude-sonnet-5-5")
        w("\n### Conclusion on verifiers\n")
        w(f"- **Cheap-first plus a measured verifier is not better than always-haiku, and does not reach Laya's pass rate.** Always-haiku: {first['haiku']}/{len(ids)} at ${mean['haiku']:.4f}/task, no added latency. Haiku-first with a haiku grader: {hh[0]*len(ids):.0f}/{len(ids)} at ${hh[1]:.4f} (+{hh[2]:.0f} s/task); with a sonnet grader {hs[0]*len(ids):.0f}/{len(ids)} at ${hs[1]:.4f}. Luna-first with a haiku grader {lh[0]*len(ids):.0f}/{len(ids)} at ${lh[1]:.4f}; with a sonnet grader {ls[0]*len(ids):.0f}/{len(ids)} at ${ls[1]:.4f}. The 170/194 and 155/194 figures quoted earlier for Laya tier-start and the table on master used the free check; like for like, see the next section (tier-start with real signals: 143-149/194; table on master: 147-149/194). The pass-rate gain the free-check cascades showed (174 and 170) does not survive a real grader, for Laya's policies or for the plain cascades.")
        w("- **Why.** The graders catch most wrong luna answers (about 77% recall) but only a third to a half of the wrong answers from haiku, sonnet and opus (33-44%), and flag 13-45% of correct answers. The failures a cascade must catch to climb past haiku are exactly the ones a haiku- or sonnet-class grader cannot see. Sonnet is not clearly better than haiku as a grader (pooled recall 56% vs 53%, false alarms 22% vs 23%) at 16x the cost; opus (a 100-answer sample, wide intervals) catches more (71% recall pooled) but also flags 28% of correct answers, at $0.033 per answer, so it does not pay either.")
        w("- **Self vs cross.** Self-graded recall is not higher than cross-graded (haiku grading haiku 41% vs haiku grading sonnet/opus 33-34%; sonnet grading sonnet 44% vs opus 40%): there is no sign of self-preference here, but intervals overlap. The apparent 'cross > self' pooled gap (56% vs 41% for haiku) is driven by luna's obviously weaker answers.")
        w("- **Break-even.** Grader cost is not the binding constraint: haiku costs $0.0009 per answer, far under the break-even (about $0.01-0.02 per answer vs haiku-first free and Laya tier-start; sonnet at $0.014 is over the haiku-first-free line, the opus sample is not). **Recall is.** With zero false alarms, luna-first needs d >= 0.74 to match haiku alone and d >= 0.96 to match free-check haiku-first; haiku-first needs d >= 0.69 to match free-check sonnet-first. Measured recall on the answers that matter (haiku, sonnet, opus) is 33-44%, with 13-20% false alarms. To make cheap-first plus a verifier worth it, a verifier would need roughly 0.7+ recall on strong-model errors at low false alarms: tests or an executable check, not a same-class model reading the answer.")
        w("- **Caveats.** The graders see the prompt, fixtures and rubric but cannot run anything; the objective check is itself a noisy proxy (grader FAIL on an objective PASS may be a real defect, so false alarms are overstated if the check is lenient); one run per task/model; grader verdicts are single samples (run-to-run grader noise not measured); the opus sample is 100 answers (25 per producing model) and its cascade rows cover 7 and 23 tasks only (ignore them); grading cost $15.13 in all (haiku $0.70, sonnet $11.11, opus sample $3.32) against an estimate of about $3 for haiku plus sonnet: the estimate counted answer, prompt and rubric text only (0.84M input tokens), but each call also carried the task's fixture files (~0.3M tokens in all) and the `claude` CLI's own overhead (cache creation/reads on every call), and the real input totalled 2.7-3.0M tokens per grader; sonnet's list rate is ~15x haiku's, which turns that into $11 (the opus sample was outside the estimate).")

    byname = {r["name"]: r for r in rows}
    best_casc = min((byname[n] for n in ("haiku-first cascade", "luna-first cascade", "sonnet-first cascade")), key=lambda r: r["cost"])
    w("\n## Conclusion\n")
    lt = byname[policies[0][0]]; ts = byname[policies[1][0]]; ot = byname[policies[2][0]]; orc = byname["oracle (cheapest model that passed)"]
    def R(prefix): return next(r for r in rows if r["name"].startswith(prefix))
    lf_, hf_, l2 = R("luna-first cascade"), R("haiku-first"), R("luna-first, capped")
    w(f"- **No classifier row beats the plain cascade.** Luna-first (no classifier) is cheaper than every Laya row and passes at least as many: ${lf_['cost']:.4f}/task, {lf_['passn']}/{len(ids)}, against Laya tier-start ${ts['cost']:.4f}, {ts['passn']}/{len(ids)} and Laya + the table as on master ${lt['cost']:.4f}, {lt['passn']}/{len(ids)}. Haiku-first ({hf_['passn']}/{len(ids)}, ${hf_['cost']:.4f}) also beats both on cost at equal or better pass. With authored labels the tier-start rule is no better ({ot['passn']}/{len(ids)}, ${ot['cost']:.4f}), so label error is not the limit.")
    w(f"- **Laya as it stands on master is the worst of the routed policies**: the certified table is opus-only, and opus alone passes {R('always-opus')['passn']}/{len(ids)}; the table costs {lt['cost']/hf_['cost']:.1f}x haiku-first and passes {hf_['passn']-lt['passn']} fewer tasks. That is the cost of requiring certification at ~22 tasks per kind, not a classifier error.")
    w(f"- **The signal is not there.** Laya's predicted difficulty has AUC {au(diff,'luna')} / {au(diff,'haiku')} / {au(diff,'sonnet')} for luna / haiku / sonnet failing (0.5 = nothing); the authored tier is no better. Skipping haiku needs at least 91% precision to pay (haiku is so cheap that a skip costs 11x when wrong); skipping luna needs only 29% but can save at most ${rung_stats('luna')[2]/len(ids):.4f}/task. The most any skip rule could save over luna-first is its failed-attempt cost (${lf_['attempt_cost']:.4f}/task) plus overshoot (${lf_['over']:.4f}).")
    w(f"- **The money left on the table is elsewhere**: luna-first spends ${lf_['nobody_cost']:.4f}/task climbing the full ladder on the {lf_['nobody']} tasks no model passes. Capping at two escalations costs {lf_['passn']-l2['passn']} passes and ${lf_['cost']-l2['cost']:.4f}/task saved ({l2['passn']}/{len(ids)}, ${l2['cost']:.4f}). That is a stop rule, not a classifier.")
    w(f"- **Free failure detection is the dominant caveat, and it is large.** nectra's own signal (a failed turn) catches 0-5% of wrong answers on these runs, so a cascade driven by it is just its first rung: luna-first {first['luna']}/{len(ids)} at $0.0055, haiku-first {first['haiku']}/{len(ids)} at $0.0105 (against 174 and 170 with the objective check). Every cascade advantage above needs a verifier with high recall; at d=0.5 luna-first drops to about 131/194 and haiku-first to about 159/194. A verifier (tests, a grader model, the user) matters more than any classifier. The saved answers allow measuring a grader's d with no new model runs (cost in the section above).")
    w("- **Where a classifier could still win**: (1) a *stop/give-up* predictor for tasks that will fail at every rung (largest unpaid cost above); (2) a *difficulty* signal with AUC well above 0.5 for luna/haiku failure (none measured here); (3) kinds or workloads where the cheap rung fails often and a mid rung is cheap relative to opus (the p* column). Caveats: one run per model and task, 194 tasks, free failure detection, no project identifier (clusters are id families), noise data empty, and costs are notional API list prices.")
    w("\n## Reproduce\n\n`python3 eval/calibration/classifier_vs_cascade.py` (reads `rhv2-snapshot.json`); add `--data ~/nectra-calibration/eval/calibration` to rebuild the snapshot from the run files.")
    (HERE / "CLASSIFIER-VS-CASCADE.md").write_text("\n".join(L) + "\n")
    print("\n".join(L))


if __name__ == "__main__":
    main()
