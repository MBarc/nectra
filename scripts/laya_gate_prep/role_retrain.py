#!/usr/bin/env python3
"""Stage the Laya ROLE-head retrain on calibration oracle labels (builds data only; trains nothing, calls no model API).

  role_retrain.py build   --oracle ORACLE.json --out DIR [--replay-from train.jsonl --replay N]
  role_retrain.py estimate --records N [--epochs 2] [--threads 4]

build:
  * keeps ONLY oracle rows with split == "train" (sha256(task id) % 10 < 7, the oracle's own rule) and never an rh-* id;
  * label = role of the cheapest acceptable model that claims a role (`prefers` in the registry); no claimant -> "general";
  * assert_clean: every training prompt is checked against every guarded set (the 200 held-out rh prompts, the oracle's
    heldout-split tasks, eval/laya/{fresh-test*,dev,long-spec-*,old60-rubric}.json); an exact or token-Jaccard >= 0.8
    near-duplicate (the same filter tools/laya/retrain uses) aborts the build with the offending pair;
  * writes DIR/train.jsonl (role-only laya-train records, half with the claimed-roles subset as options, plus --replay rows
    from an earlier train.jsonl so the task/difficulty heads don't drift), DIR/dev.jsonl, DIR/manifest.json.
"""
import argparse, hashlib, json, random, re, sys, tomllib
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
CAL = Path.home() / "nectra-calibration"
SEC_PER_ITEM_EPOCH = 3.8   # measured: run 20261030-164330, 3311 question-items x 2 epochs in 6h57m (20:44->03:40) on builder, CPU


def norm(t):
    return " ".join(re.sub(r"[^a-z0-9]+", " ", t.lower()).split())


def prompt_text(x):
    if isinstance(x, str):
        return x
    for k in ("prompt", "text", "message", "state"):
        if isinstance(x.get(k) if isinstance(x, dict) else None, str):
            return x[k]
    return ""


def guarded_prompts(oracle_rows):
    g = {}  # normalised -> source
    def add(text, src):
        if text.strip():
            g[norm(text)] = src
    for f in sorted((REPO / "eval/laya").glob("*.json")):
        if "adjudication" in f.name or "result" in f.name or f.name in ("criteria.json",):
            continue
        try:
            d = json.loads(f.read_text(encoding="utf-8"))
        except ValueError:
            continue
        for item in (d if isinstance(d, list) else d.get("items", d.get("cases", []))):
            add(prompt_text(item), f.name)
        if not any(v == f.name for v in g.values()):
            raise SystemExit(f"guard file {f.name} yielded no prompts: unknown layout, refusing to certify clean")
    for f in (CAL / "new-heldout-prompts.json", CAL / "new-heldout-prompts-2.json"):
        if not f.is_file():
            raise SystemExit(f"guard manifest missing: {f}; refusing to certify clean")
        if True:
            d = json.loads(f.read_text(encoding="utf-8-sig"))
            for item in (d if isinstance(d, list) else d.get("prompts", d.get("tasks", []))):
                add(prompt_text(item), f.name)
    for r in oracle_rows:
        if r.get("split") != "train":
            add(r["prompt"], "oracle heldout split")
    return g


def assert_clean(train_prompts, guarded):
    gsets = [(set(k.split()), v, k) for k, v in guarded.items()]
    for t in train_prompts:
        n = norm(t)
        if n in guarded:
            raise SystemExit(f"LEAK: training prompt is identical to a guarded prompt from {guarded[n]}: {t[:80]!r}")
        w = set(n.split())
        for gw, src, _ in gsets:
            if w and gw and len(w & gw) / len(w | gw) >= 0.8:
                raise SystemExit(f"LEAK: near-duplicate (Jaccard>=0.8) of a guarded prompt from {src}: {t[:80]!r}")
    return len(guarded)


def claims(registry):
    out = {}
    for m in tomllib.loads(Path(registry).read_text(encoding="utf-8"))["model"]:
        if m.get("enabled", True) and m.get("prefers"):
            out[m["id"]] = m["prefers"][0]
    return out


def role_for(row, claim):
    cands = [m for m in row["acceptable_set"] if m in claim]
    if not cands:
        return "general"
    cost = row.get("cost_usd_median", {})
    return claim[min(cands, key=lambda m: (cost.get(m, 1e9), m))]


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sp = ap.add_subparsers(dest="cmd", required=True)
    b = sp.add_parser("build")
    b.add_argument("--oracle", required=True)
    b.add_argument("--out", required=True)
    b.add_argument("--registry", default=str(REPO / "config/models.toml"))
    b.add_argument("--replay-from", default=None)
    b.add_argument("--replay", type=int, default=800)
    e = sp.add_parser("estimate")
    e.add_argument("--records", type=int, required=True, help="question-items per epoch (role-only record = 1)")
    e.add_argument("--epochs", type=int, default=2)
    e.add_argument("--threads", type=int, default=4)
    a = ap.parse_args()
    if a.cmd == "estimate":
        s = a.records * a.epochs * SEC_PER_ITEM_EPOCH
        print(f"~{s/3600:.1f} h wall if it matches the reference run (3.8 s per question-item-epoch, torch default threads on a "
              f"24-core, shared builder; thread count of that run not recorded). Capped to {a.threads} threads expect up to "
              f"~{s*24/a.threads/3600:.0f} h worst case (linear scaling, pessimistic). An epoch is checkpointed, so it resumes.")
        return
    oracle = json.loads(Path(a.oracle).read_text(encoding="utf-8"))["tasks"]
    train = [r for r in oracle if r.get("split") == "train" and not r["task"].startswith("rh-")]
    assert train, "no train-split rows"
    guarded = guarded_prompts(oracle)
    n = assert_clean([r["prompt"] for r in train], guarded)
    claim = claims(a.registry)
    crit = json.loads((REPO / "eval/laya/criteria.json").read_text(encoding="utf-8"))
    roles = [k for k, _ in crit["roles"]]
    subset = [r for r in roles if r in set(claim.values())]
    rq = lambda opts: {"role": {"type": "choice", "instructions": crit["questions"]["role"], "criteria": {r: "" for r in opts}}}
    rows, labels = [], {}
    for i, r in enumerate(train):
        role = role_for(r, claim)
        labels[role] = labels.get(role, 0) + 1
        opts = subset if (role in subset and i % 2) else roles
        rows.append({"state": r["prompt"], "questions": rq(opts), "expected": {"role": role}})
    if a.replay_from:
        old = [json.loads(l) for l in Path(a.replay_from).read_text(encoding="utf-8").splitlines() if l.strip()]
        random.Random(1).shuffle(old)
        old = old[:a.replay]
        def texts(r):   # follow-up records carry {previous_message, current_message}
            st = r["state"]
            return [st] if isinstance(st, str) else [v for v in st.values() if isinstance(v, str)]
        assert_clean([x for r in old for x in texts(r)], guarded)   # replay rows are training data too
        rows += old
    random.Random(1).shuffle(rows)
    out = Path(a.out); out.mkdir(parents=True, exist_ok=True)
    (out / "train.jsonl").write_text("".join(json.dumps(x) + "\n" for x in rows), encoding="utf-8")
    dev = [r for r in oracle if r.get("split") != "train" and not r["task"].startswith("rh-")]
    # resumable_train's per-epoch dev check reads the *task* answer of every row, so dev.jsonl is the task dev set (as in
    # tools/laya/retrain): it also shows whether the role fine-tune made the task head forget. The oracle's heldout-split role
    # rows go to dev-role.jsonl (informational; not read by the trainer).
    tq = {"type": "choice", "instructions": crit["questions"]["task"], "criteria": {k: "" for k, _ in crit["tasks"]}}
    tdev = json.loads((REPO / "eval/laya/dev.json").read_text(encoding="utf-8-sig"))
    (out / "dev.jsonl").write_text("".join(json.dumps({"state": c["prompt"], "questions": {"task": tq}, "expected": {"task": c["task"]}}) + "\n" for c in tdev), encoding="utf-8")
    (out / "dev-role.jsonl").write_text("".join(json.dumps({"state": r["prompt"], "questions": rq(roles), "expected": {"role": role_for(r, claim)}}) + "\n" for r in dev), encoding="utf-8")
    items = sum(len(x["questions"]) for x in rows)
    man = {"oracle": str(a.oracle), "train_tasks": len(train), "dev_tasks_oracle_heldout_split": len(dev), "role_labels": labels,
           "replay_rows": len(rows) - len(train), "question_items_per_epoch": items, "guarded_prompts_checked": n,
           "assert_clean": "passed", "note": "dev.jsonl = task dev set (forgetting check); dev-role.jsonl = oracle heldout split; the 200 rh prompts are never used"}
    (out / "manifest.json").write_text(json.dumps(man, indent=1) + "\n", encoding="utf-8")
    print(json.dumps(man, indent=1))


if __name__ == "__main__":
    main()
