#!/usr/bin/env python3
"""Check held-out prompts for exact / near duplicates against Laya training data,
calibration tasks, and each other.  Stdlib only; run on builder.

near-duplicate = Jaccard of word 3-shingles >= --threshold (default 0.5).
"""
import argparse, glob, json, re, sys
from collections import Counter, defaultdict
from pathlib import Path


def norm(s):
    return re.findall(r"[a-z0-9]+", s.lower())


def shingles(s, k=3):
    w = norm(s)
    return {" ".join(w[i:i + k]) for i in range(max(len(w) - k + 1, 1))}


def jacc(a, b):
    return len(a & b) / len(a | b) if a | b else 0.0


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--new", nargs="+", required=True, help="prompt files to check")
    ap.add_argument("--existing", nargs="*", default=[], help="prompt files already in the set (the first 48)")
    ap.add_argument("--laya", default="", help="glob of Laya jsonl files (field 'state')")
    ap.add_argument("--calib-tasks", default="", help="calibration tasks dir (task.json per task)")
    ap.add_argument("--exclude-prefix", default="rh-", help="skip calibration tasks with this id prefix")
    ap.add_argument("--threshold", type=float, default=0.5)
    ap.add_argument("--out", required=True)
    a = ap.parse_args()

    new = [t for f in a.new for t in json.load(open(f, encoding="utf-8"))["tasks"]]
    corpus = {}  # source -> list of (label, text)
    corpus["existing-48"] = [(t["id"], t["prompt"]) for f in a.existing for t in json.load(open(f, encoding="utf-8"))["tasks"]]
    if a.calib_tasks:
        c = []
        for p in sorted(Path(a.calib_tasks).glob("*/task.json")):
            t = json.load(open(p, encoding="utf-8"))
            if not t["id"].startswith(a.exclude_prefix):
                c.append((t["id"], t["prompt"]))
        corpus["calibration"] = c
    if a.laya:
        seen = {}
        for f in sorted(glob.glob(a.laya)):
            for line in open(f, encoding="utf-8"):
                try:
                    s = json.loads(line).get("state")
                except ValueError:
                    continue
                if isinstance(s, str):
                    seen.setdefault(" ".join(norm(s)), s)
        corpus["laya"] = [(f"laya:{i}", s) for i, s in enumerate(seen.values())]
    corpus["within-new"] = [(t["id"], t["prompt"]) for t in new]

    result = {"threshold": a.threshold, "new_prompts": len(new),
              "types": Counter(t["type"] for t in new), "tiers": Counter(t["tier"] for t in new),
              "corpus_sizes": {k: len(v) for k, v in corpus.items()}, "sources": {}, "flagged": []}
    ids = Counter(t["id"] for t in new)
    result["duplicate_ids"] = [i for i, n in ids.items() if n > 1]

    for src, docs in corpus.items():
        sh = [shingles(t) for _, t in docs]
        exact = defaultdict(list)
        index = defaultdict(list)
        for i, (_, t) in enumerate(docs):
            exact[" ".join(norm(t))].append(i)
            for g in sh[i]:
                index[g].append(i)
        worst = 0.0
        for t in new:
            q = shingles(t["prompt"])
            hits = Counter(i for g in q for i in index.get(g, ()))
            best_i, best = None, 0.0
            for i, _ in hits.most_common(25):
                if src == "within-new" and docs[i][0] == t["id"]:
                    continue
                j = jacc(q, sh[i])
                if j > best:
                    best_i, best = i, j
            is_exact = any(docs[i][0] != t["id"] for i in exact.get(" ".join(norm(t["prompt"])), []))
            worst = max(worst, best)
            if is_exact or best >= a.threshold:
                result["flagged"].append({"id": t["id"], "source": src, "match": docs[best_i][0] if best_i is not None else None,
                                          "jaccard": round(best, 3), "exact": is_exact, "match_text": docs[best_i][1][:200] if best_i is not None else None})
        result["sources"][src] = {"max_jaccard": round(worst, 3), "flagged": sum(f["source"] == src for f in result["flagged"])}
    result["pass"] = not result["flagged"] and not result["duplicate_ids"]
    Path(a.out).write_text(json.dumps(result, indent=2, default=dict) + "\n", encoding="utf-8")
    print(json.dumps({k: result[k] for k in ("new_prompts", "types", "tiers", "corpus_sizes", "sources", "duplicate_ids", "pass")}, default=dict))
    for f in result["flagged"][:20]:
        print("FLAG", f["id"], f["source"], f["match"], f["jaccard"])
    sys.exit(0 if result["pass"] else 1)


if __name__ == "__main__":
    main()
