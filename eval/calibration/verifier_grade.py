#!/usr/bin/env python3
"""Grade the SAVED rh-v2 answers with a real grader model (no task re-runs).

  python3 verifier_grade.py --data ~/nectra-calibration/eval/calibration --grader claude-haiku-5-5 [--sample N]

Blinded: the grader sees the task prompt, the task's fixture files, the human rubric text and the answer only.
It never sees the objective check (`answer_checks`, `must_labels`), the model name, nectra metadata or the run JSON.
Model/provider names in the answer text are redacted unless the task prompt or its files use the same word;
every redaction is logged in verifier-redactions.json. Resumable; writes verifier-grades-<grader>.json."""
import argparse, concurrent.futures as cf, glob, json, os, random, re, subprocess, sys, time
from pathlib import Path

HERE = Path(__file__).parent
MODELS = {"luna": "gpt-5.6-luna", "haiku": "haiku", "sonnet": "sonnet", "opus": "opus"}
NAME = re.compile(r"claude|haiku|sonnet|opus|fable|anthropic|gpt-?[\d.]+(?:-\w+)*|openai|luna|astra|codex|gemini|nectra|laya|rh-v2", re.I)
SCRATCH = Path("/tmp/verifier-scratch")


def blind(answer, allowed):
    log = []
    def sub(m):
        if m.group(0).lower() in allowed:
            return m.group(0)
        log.append(answer[max(0, m.start() - 40): m.end() + 40].replace("\n", " "))
        return "[REDACTED]"
    return NAME.sub(sub, answer), log


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--data", required=True); ap.add_argument("--grader", required=True)
    ap.add_argument("--sample", type=int, default=0, help="sample size, split equally across producing models (seeded); 0 = all")
    ap.add_argument("--workers", type=int, default=4); a = ap.parse_args()
    data = Path(a.data); tmpl = (HERE / "verifier-prompt.md").read_text()
    out = HERE / f"verifier-grades-{a.grader}.json"
    done = json.load(open(out)) if out.exists() else {}
    items = []
    for t in sorted(glob.glob(str(data / "tasks-v2" / "rh-*" / "task.json"))):
        j = json.load(open(t)); d = Path(t).parent
        fx = ""; files = sorted(f for f in glob.glob(str(d / "files" / "**"), recursive=True) if os.path.isfile(f))
        for f in files:
            fx += f"--- {os.path.relpath(f, d / 'files')} ---\n{open(f, errors='replace').read()}\n"
        rubric = "\n".join(f"- ({r['points']}) {r['text']}" for r in j["rubric"])
        for short, dirn in MODELS.items():
            p = data / "results" / f"rh-v2-{dirn}" / "runs" / dirn / f"{j['id']}.r0.answer.md"
            if p.exists(): items.append((j, short, p, fx, rubric))
    if a.sample:
        rnd = random.Random(20261115); rnd.shuffle(items)  # stratified: an equal share per producing model
        per = max(1, a.sample // len(MODELS)); items = [x for m in MODELS for x in [y for y in items if y[1] == m][:per]]
    redactions = json.load(open(HERE / "verifier-redactions.json")) if (HERE / "verifier-redactions.json").exists() else {}
    SCRATCH.mkdir(exist_ok=True)

    def run(item):
        j, short, p, fx, rubric = item; key = f"{j['id']}|{short}"
        if key in done: return key, None
        allowed = {w.lower() for w in NAME.findall(j["prompt"] + fx)}
        answer, log = blind(p.read_text(errors="replace"), allowed)
        redactions[key] = log
        prompt = tmpl
        for k, v in (("{task}", j["prompt"]), ("{fixtures}", fx or "(none)"), ("{rubric}", rubric), ("{answer}", answer)):
            prompt = prompt.replace(k, v, 1) if k != "{answer}" else prompt.replace(k, v, 1)
        t0 = time.time()
        r = subprocess.run(["claude", "-p", "--model", a.grader, "--output-format", "json", "--tools", "", "--no-session-persistence",
                            "--disable-slash-commands", "--system-prompt", "You are a strict, concise grader. Output only the JSON object requested."],
                           input=prompt, capture_output=True, text=True, cwd=SCRATCH, timeout=300)
        wall = time.time() - t0
        try:
            res = json.loads(r.stdout); text = res.get("result", "")
            m = re.search(r"\{.*\}", text, re.S); v = json.loads(m.group(0)) if m else {}
            verdict = str(v.get("verdict", "")).upper()
            if verdict not in ("PASS", "FAIL"): verdict = "ERROR"
            u = res.get("usage", {})
            return key, {"verdict": verdict, "reason": v.get("reason", ""), "wall_s": wall, "api_ms": res.get("duration_api_ms"),
                         "cost_usd": res.get("total_cost_usd", 0.0),
                         "in_tok": u.get("input_tokens", 0) + u.get("cache_creation_input_tokens", 0) + u.get("cache_read_input_tokens", 0),
                         "out_tok": u.get("output_tokens", 0)}
        except Exception as e:
            return key, {"verdict": "ERROR", "reason": f"{type(e).__name__}: {r.stdout[:120]} {r.stderr[:120]}", "wall_s": wall, "cost_usd": 0.0, "in_tok": 0, "out_tok": 0}

    n = 0
    with cf.ThreadPoolExecutor(a.workers) as ex:
        for key, res in ex.map(run, items):
            if res is None: continue
            done[key] = res; n += 1
            if n % 20 == 0:
                json.dump(done, open(out, "w"), indent=1); json.dump(redactions, open(HERE / "verifier-redactions.json", "w"), indent=1)
                print(n, "graded; spent $%.2f" % sum(v["cost_usd"] for v in done.values()), flush=True)
    json.dump(done, open(out, "w"), indent=1); json.dump(redactions, open(HERE / "verifier-redactions.json", "w"), indent=1)
    print("done", len(done), "spent $%.2f" % sum(v["cost_usd"] for v in done.values()))


if __name__ == "__main__":
    main()
