#!/usr/bin/env python3
"""Repoints src/laya.rs at a new model release.

    python scripts/update_laya_pins.py laya-model-N SHA256SUMS [--src src/laya.rs]

Sets RELEASE to the tag and the model files' sha256 (model*, tokenizer/*,
rl_agent_config.json) to the ones SHA256SUMS lists. The ONNX Runtime binaries
and ONNXRUNTIME-LICENSE are not retrained, so their SHA256SUMS entries must match
the existing pins. Refuses, changing nothing, if the tag is malformed, an asset
the source pins is missing from SHA256SUMS, a hash isn't 64 hex digits, or a
runtime file's hash differs from its pin.
"""
import argparse
import re
import sys
from pathlib import Path

# One (path in the model directory, release asset, sha256) tuple of src/laya.rs.
PIN = re.compile(r'\(\s*"([^"]+)",\s*"([^"]+)",\s*"([0-9a-f]{64})",?\s*\)')
TAG = re.compile(r"laya-model-[0-9]+")
RELEASE = re.compile(r'(releases/download/)laya-model-[0-9]+(")')


def pins(text):
    """(path, asset, sha256) for every model file and platform runtime."""
    return PIN.findall(text)


def repinnable(path):
    """Only the retrained files may change; the runtime and its license are pinned for good."""
    return path.startswith(("model", "tokenizer/")) or path == "rl_agent_config.json"


def read_sums(path):
    sums = {}
    for line in Path(path).read_text(encoding="utf-8").splitlines():
        if not line.strip():
            continue
        m = re.fullmatch(r"([0-9a-fA-F]{64}) [ *](\S+)", line.strip())
        if not m:
            sys.exit(f"{path}: not a sha256sum line: {line!r}")
        sums[m[2]] = m[1].lower()
    return sums


def update(text, tag, sums):
    if not TAG.fullmatch(tag):
        sys.exit(f"tag must be laya-model-N, got {tag!r}")
    current = pins(text)
    if not current:
        sys.exit("found no pinned assets in the source")
    missing = sorted({asset for _, asset, _ in current} - sums.keys())
    if missing:
        sys.exit(f"SHA256SUMS has no entry for: {', '.join(missing)}")
    changed = sorted(asset for path, asset, sha in current if not repinnable(path) and sums[asset] != sha)
    if changed:
        sys.exit(f"runtime files differ from their pins (only the model may be repinned): {', '.join(changed)}")
    assets = {asset for path, asset, _ in current if repinnable(path)}
    text, n = RELEASE.subn(rf"\g<1>{tag}\g<2>", text)
    if n != 1:
        sys.exit(f"expected one RELEASE url, found {n}")
    for asset in assets:
        # The hash follows its asset name; `"model.onnx",` as a path isn't followed by a hash.
        text, n = re.subn(rf'("{re.escape(asset)}",\s*")[0-9a-f]{{64}}(")', rf"\g<1>{sums[asset]}\g<2>", text)
        if n != 1:
            sys.exit(f"expected one pin for {asset}, found {n}")
    return text


if __name__ == "__main__":
    p = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    p.add_argument("tag")
    p.add_argument("sums")
    p.add_argument("--src", default=str(Path(__file__).resolve().parents[1] / "src" / "laya.rs"))
    a = p.parse_args()
    src = Path(a.src)
    src.write_text(update(src.read_text(encoding="utf-8"), a.tag, read_sums(a.sums)), encoding="utf-8", newline="\n")
    print(f"{src}: RELEASE -> {a.tag}, model pins updated")
