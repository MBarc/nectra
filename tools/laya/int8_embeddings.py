"""python tools/laya/int8_embeddings.py SRC_DIR DST_DIR

Stores the token-embedding table (256k x 768, ~60% of mmBERT's weights) as
int8 with one scale per row; the graph gathers int8 rows and scales them back.
Halves the model (1.29 GB -> 0.70 GB). On the 2026-10-19 model it gave the same
answer on every eval prompt at the same speed. Matrix multiplies stay fp32:
quantizing those cost accuracy before (laya large int8: 52/60 vs 56/60).
"""
import shutil
import sys
from pathlib import Path

import numpy as np
import onnx
from onnx import TensorProto, helper, numpy_helper


def quantize(src: Path, dst: Path):
    m = onnx.load(str(src / "model.onnx"))
    inits = {i.name: i for i in m.graph.initializer}
    done = 0
    for node in list(m.graph.node):
        if node.op_type != "Gather" or node.input[0] not in inits:
            continue
        w = numpy_helper.to_array(inits[node.input[0]])
        if w.ndim != 2 or w.shape[0] < 10000:
            continue
        scale = np.abs(w).max(axis=1, keepdims=True) / 127.0
        scale[scale == 0] = 1.0
        q = np.clip(np.round(w / scale), -127, 127).astype(np.int8)
        name, out = node.input[0], node.output[0]
        m.graph.initializer.remove(inits[name])
        m.graph.initializer.extend([
            numpy_helper.from_array(q, name + "_q"),
            numpy_helper.from_array(scale.astype(np.float32), name + "_scale"),
        ])
        at = list(m.graph.node).index(node)
        m.graph.node.remove(node)
        for k, n in enumerate([
            helper.make_node("Gather", [name + "_q", node.input[1]], [out + "_q"], axis=0),
            helper.make_node("Gather", [name + "_scale", node.input[1]], [out + "_s"], axis=0),
            helper.make_node("Cast", [out + "_q"], [out + "_f"], to=TensorProto.FLOAT),
            helper.make_node("Mul", [out + "_f", out + "_s"], [out]),
        ]):
            m.graph.node.insert(at + k, n)
        done += 1
    if not done:
        raise SystemExit("no embedding table found")
    shutil.copytree(src, dst, ignore=shutil.ignore_patterns("model.onnx", "model.onnx.data", "fixtures.npz"))
    onnx.save(m, str(dst / "model.onnx"), save_as_external_data=True,
              all_tensors_to_one_file=True, location="model.onnx.data")


if __name__ == "__main__":
    quantize(Path(sys.argv[1]), Path(sys.argv[2]))
