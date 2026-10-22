#!/usr/bin/env python3
"""Eval gate for a candidate Laya model.

    python scripts/laya_gate.py STAGE_DIR [--set fresh-test-3.json] [--floor F] [--out gate.json]

STAGE_DIR holds the release assets under their asset names. Lays them out as
src/laya.rs expects, runs `nectra eval` on a held-out set through the built
binary, and exits 1 unless accuracy is at least the current model's (the
`accuracy` in eval/laya/<set minus .json>-result.json) and at least --floor.
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


def main():
    p = argparse.ArgumentParser()
    p.add_argument("stage", type=Path)
    p.add_argument("--set", default="fresh-test-3.json", help="held-out set in eval/laya/")
    p.add_argument("--floor", type=float, default=0.0, help="minimum accuracy, on top of the current model's")
    p.add_argument("--nectra", default=str(REPO / "target" / "release" / ("nectra.exe" if sys.platform == "win32" else "nectra")))
    p.add_argument("--out", type=Path, default=Path("gate.json"))
    a = p.parse_args()
    if not a.set or Path(a.set).name != a.set:
        sys.exit(f"--set must be a file name in eval/laya/, got {a.set!r}")

    baseline = json.loads((retrain.LAYA / a.set.replace(".json", "-result.json")).read_text(encoding="utf-8"))["accuracy"]
    need = max(baseline, a.floor)
    with tempfile.TemporaryDirectory() as tmp:
        tmp = Path(tmp)
        layout(a.stage, tmp / "model")
        report, _ = retrain.nectra_eval(a.nectra, tmp / "model", retrain.LAYA / a.set, tmp)
        # A model that fails to load falls back to keywords; route says which ran.
        route = subprocess.run([a.nectra, "--config", tmp / "eval-config.toml", "route", "write a hello world script"],
                               capture_output=True, text=True, encoding="utf-8")
        reason = json.loads(route.stdout or "{}").get("reason", route.stderr)
    if not reason.startswith("laya classified"):
        sys.exit(f"nectra did not use the model: {reason}")
    result = {"set": a.set, "passed": report["passed"], "total": report["total"], "accuracy": report["accuracy"],
              "current_model": baseline, "floor": a.floor, "required": need, "ok": report["accuracy"] >= need}
    a.out.write_text(json.dumps(result, indent=2), encoding="utf-8")
    print(json.dumps(result, indent=2))
    if not result["ok"]:
        sys.exit(f"accuracy {report['accuracy']:.4f} is below the required {need:.4f}")


if __name__ == "__main__":
    main()
