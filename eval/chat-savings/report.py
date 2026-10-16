"""Summarise runs: python report.py RUNS_DIR

Cost is API-equivalent dollars from prices.json, so subscription and API
users can compare on one scale.
"""
import json
import pathlib
import statistics
import sys

PRICES = json.loads((pathlib.Path(__file__).parent / "prices.json").read_text())


def cost(usage):
    total = 0.0
    for model, u in usage.items():
        p = PRICES[model]
        total += sum(u["tokens"][k] * p[k] for k in ("input", "cache_read", "cache_write", "output")) / 1e6
    return total


def main(runs):
    rows = []
    for f in sorted(pathlib.Path(runs).glob("*/result.json")):
        r = json.loads(f.read_text(encoding="utf-8"))
        if r["label"].startswith("smoke"):
            continue
        config = r["pinned"] or ("auto+jev" if r["jev"] else "auto")
        if r.get("mode"):
            config += f" ({r['mode']})"
        config = f"{r.get('workload', 'light')[0]}:{config}"
        tokens = sum(sum(u["tokens"].values()) for u in r["usage"].values())
        rows.append((config, r["label"], cost(r["usage"]), tokens, r["seconds"],
                     r["check"].get("score", 0), r["check"].get("core"), r["valid"], r["problems"], r["routes"]))
    print(f"{'config':<14} {'run':<16} {'cost $':>8} {'tokens':>10} {'secs':>6} {'score':>5} core valid")
    for c, l, d, t, s, sc, core, v, probs, _ in rows:
        print(f"{c:<14} {l:<16} {d:>8.3f} {t:>10} {s:>6} {sc:>5} {str(core):<5} {v} {'; '.join(probs)}")
    print()
    for c in dict.fromkeys(r[0] for r in rows):
        group = [r for r in rows if r[0] == c and r[7]]
        if group:
            costs = [r[2] for r in group]
            print(f"{c:<14} valid runs {len(group)}  median cost ${statistics.median(costs):.3f}"
                  f"  range ${min(costs):.3f}-{max(costs):.3f}  median secs {statistics.median(r[4] for r in group):.0f}"
                  f"  core passed {sum(bool(r[6]) for r in group)}/{len(group)}")
    for c, l, *_, routes in rows:
        if "auto" in c:
            print(f"{l}: {' -> '.join(routes)}")


if __name__ == "__main__":
    main(sys.argv[1])
