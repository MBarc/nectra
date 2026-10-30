#!/usr/bin/env python3
"""Eval gate for a candidate Laya model.

    python scripts/laya_gate.py STAGE_DIR [--set fresh-test-3.json] [--tier-set long-spec-heldout.json]
                                [--floor F] [--max-extra-fp N] [--out gate.json]

STAGE_DIR holds the release assets under their asset names. Lays them out as
src/laya.rs expects, runs `nectra eval` through the built binary, and exits 1
unless every check holds:
- task accuracy on --set is at least the current model's (the `accuracy` in
  eval/laya/<set minus .json>-result.json) and at least --floor;
- on --tier-set, tier accuracy and the router's demanding-model true positives
  are at least the current model's (eval/laya/<tier set>-result.json), and its
  false escalations exceed the current model's by at most --max-extra-fp;
- no prompt in either set fell back to keywords, so every score is the model's.
Uses the same eval path as tools/laya/retrain.
"""
import argparse
import importlib.machinery
import importlib.util
import json
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parent
sys.path.insert(0, str(HERE))
from update_laya_pins import pins  # noqa: E402

loader = importlib.machinery.SourceFileLoader("retrain", str(REPO / "tools" / "laya" / "retrain"))
spec = importlib.util.spec_from_loader("retrain", loader)
retrain = importlib.util.module_from_spec(spec)
loader.exec_module(retrain)

RUNTIME = {"win32": "onnxruntime.dll", "darwin": "libonnxruntime.dylib"}.get(sys.platform, "libonnxruntime.so")


def layout(stage, model_dir):
    for path, asset, _ in pins((REPO / "src" / "laya.rs").read_text(encoding="utf-8")):
        # Other platforms' runtimes are release assets too; only this one is loaded.
        if path.startswith(("onnxruntime", "libonnxruntime")) and path != RUNTIME:
            continue
        if not (stage / asset).is_file():
            sys.exit(f"{stage} has no {asset}")
        (model_dir / path).parent.mkdir(parents=True, exist_ok=True)
        shutil.copy(stage / asset, model_dir / path)


def failures(task, tier, need, tier_base, max_extra_fp):
    """Every check the candidate fails, given its `nectra eval` reports on the
    task set and the tier set and the current model's numbers."""
    bad = []
    if task["accuracy"] < need:
        bad.append(f"task accuracy {task['accuracy']:.4f} is below the required {need:.4f}")
    for name, report in (("task set", task), ("tier set", tier)):
        if report["keyword_fallbacks"]:
            bad.append(f"{report['keyword_fallbacks']} prompts in the {name} fell back to keywords")
    t = tier.get("tier")
    if not t:
        return bad + ["the tier set has no gold tiers"]
    routed = t["demanding_model"]
    if t["accuracy"] < tier_base["accuracy"]:
        bad.append(f"tier accuracy {t['accuracy']:.4f} is below the current model's {tier_base['accuracy']:.4f}")
    if routed["tp"] < tier_base["demanding_model"]["tp"]:
        bad.append(f"{routed['tp']} hard specs reach the demanding model, the current model sends {tier_base['demanding_model']['tp']}")
    if routed["fp"] > tier_base["demanding_model"]["fp"] + max_extra_fp:
        bad.append(f"{routed['fp']} false escalations, over the current model's {tier_base['demanding_model']['fp']} + {max_extra_fp}")
    return bad


def main():
    p = argparse.ArgumentParser()
    p.add_argument("stage", type=Path)
    p.add_argument("--set", default="fresh-test-3.json", help="held-out task set in eval/laya/")
    p.add_argument("--tier-set", default="long-spec-heldout.json", help="held-out difficulty set in eval/laya/")
    p.add_argument("--floor", type=float, default=0.0, help="minimum accuracy, on top of the current model's")
    p.add_argument("--max-extra-fp", type=int, default=0, help="false escalations allowed over the current model's")
    p.add_argument("--nectra", default=str(REPO / "target" / "release" / ("nectra.exe" if sys.platform == "win32" else "nectra")))
    p.add_argument("--out", type=Path, default=Path("gate.json"))
    a = p.parse_args()
    for name in (a.set, a.tier_set):
        if not name or Path(name).name != name:
            sys.exit(f"sets must be file names in eval/laya/, got {name!r}")

    def current(name):
        return json.loads((retrain.LAYA / name.replace(".json", "-result.json")).read_text(encoding="utf-8"))

    baseline = current(a.set)["accuracy"]
    # The current model's `nectra eval` report on the tier set; its "tier" block.
    tier_base = current(a.tier_set)["tier"]
    need = max(baseline, a.floor)
    with tempfile.TemporaryDirectory() as tmp:
        tmp = Path(tmp)
        layout(a.stage, tmp / "model")
        report, _ = retrain.nectra_eval(a.nectra, tmp / "model", retrain.LAYA / a.set, tmp)
        tier, _ = retrain.nectra_eval(a.nectra, tmp / "model", retrain.LAYA / a.tier_set, tmp)
        # A model that fails to load falls back to keywords; route says which ran.
        route = subprocess.run([a.nectra, "--config", tmp / "eval-config.toml", "route", "write a hello world script"],
                               capture_output=True, text=True, encoding="utf-8")
        reason = json.loads(route.stdout or "{}").get("reason", route.stderr)
    bad = failures(report, tier, need, tier_base, a.max_extra_fp)
    if not reason.startswith("laya classified"):
        bad.append(f"nectra did not use the model: {reason}")
    t = tier.get("tier") or {}
    result = {"set": a.set, "passed": report["passed"], "total": report["total"], "accuracy": report["accuracy"],
              "current_model": baseline, "floor": a.floor, "required": need,
              "keyword_fallbacks": report["keyword_fallbacks"] + tier["keyword_fallbacks"],
              "tier_set": a.tier_set, "tier_accuracy": t.get("accuracy"), "demanding_model": t.get("demanding_model"),
              "tier_current_model": tier_base, "max_extra_fp": a.max_extra_fp,
              "failures": bad, "ok": not bad}
    a.out.write_text(json.dumps(result, indent=2), encoding="utf-8")
    print(json.dumps(result, indent=2))
    if bad:
        sys.exit("; ".join(bad))


if __name__ == "__main__":
    main()
