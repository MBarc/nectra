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
