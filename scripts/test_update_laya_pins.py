"""python scripts/test_update_laya_pins.py -- update_laya_pins.py rewrites the pins and refuses a missing asset."""
import subprocess
import sys
import tempfile
from pathlib import Path

HERE = Path(__file__).parent
SRC = (HERE.parent / "src" / "laya.rs").read_text(encoding="utf-8")
sys.path.insert(0, str(HERE))
import update_laya_pins as u  # noqa: E402

old = u.pins(SRC)
assets = sorted({a for _, a, _ in old})
assert len(old) == 9 and "model.onnx" in assets and "onnxruntime-win-x64.dll" in assets, old


def run(tmp, sums, tag="laya-model-7"):
    (tmp / "SHA256SUMS").write_text(sums, encoding="utf-8")
    (tmp / "laya.rs").write_text(SRC, encoding="utf-8")
    r = subprocess.run([sys.executable, str(HERE / "update_laya_pins.py"), tag, str(tmp / "SHA256SUMS"), "--src", str(tmp / "laya.rs")],
                       capture_output=True, text=True)
    return r, (tmp / "laya.rs").read_text(encoding="utf-8")


fake = {a: f"{i:x}".rjust(64, "a") for i, a in enumerate(assets, 1)}
sums = "".join(f"{h}  {a}\n" for a, h in fake.items())
with tempfile.TemporaryDirectory() as d:
    r, new = run(Path(d), sums)
    assert r.returncode == 0, r.stderr
    assert 'releases/download/laya-model-7"' in new and "laya-model-1" not in new
    assert {(p, a): h for p, a, h in u.pins(new)} == {(p, a): fake[a] for p, a, _ in old}
    # Nothing but the tag and the hashes changed.
    assert new.replace("laya-model-7", "laya-model-1").count("\n") == SRC.count("\n")
    for h in (h for _, _, h in old):
        assert h not in new

    # A missing asset refuses and leaves the source untouched.
    r, new = run(Path(d), "".join(l for l in sums.splitlines(True) if "onnxruntime-win-x64.dll" not in l))
    assert r.returncode != 0 and "onnxruntime-win-x64.dll" in r.stderr and new == SRC, r.stderr
    # So do a bad tag and a bad hash.
    r, new = run(Path(d), sums, tag="v7")
    assert r.returncode != 0 and new == SRC
    r, new = run(Path(d), sums.replace(fake["model.onnx"], "zz"))
    assert r.returncode != 0 and new == SRC
print("ok")
