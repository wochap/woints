"""Generate crates/vision/tests/fixtures/const_boxes.onnx.

A tiny graph with a 1x3x64x64 float input and a constant YOLO-style output
`[1, 4+C, N]` (C = 1, N = 3): two overlapping class-0 boxes scored 0.9 and
0.8, one box scored 0.1. Carries Ultralytics-style `names` and `imgsz`
metadata. Run once; the output is committed.

    nix shell --impure --expr 'with import <nixpkgs> {}; python3.withPackages (ps: [ ps.onnx ])' \
        -c python3 scripts/make_const_onnx.py
"""

import pathlib

import numpy as np
import onnx
from onnx import TensorProto, helper, numpy_helper

OUT = pathlib.Path(__file__).resolve().parent.parent / "crates/vision/tests/fixtures/const_boxes.onnx"

# Columns are boxes: cx, cy, w, h, score(class 0), letterbox pixels.
boxes = np.array(
    [
        [20.0, 22.0, 50.0],
        [20.0, 21.0, 50.0],
        [20.0, 20.0, 10.0],
        [20.0, 20.0, 10.0],
        [0.9, 0.8, 0.1],
    ],
    dtype=np.float32,
)[None]

const = helper.make_node(
    "Constant",
    inputs=[],
    outputs=["output0"],
    value=numpy_helper.from_array(boxes, name="boxes"),
)
graph = helper.make_graph(
    [const],
    "const_boxes",
    inputs=[helper.make_tensor_value_info("images", TensorProto.FLOAT, [1, 3, 64, 64])],
    outputs=[helper.make_tensor_value_info("output0", TensorProto.FLOAT, list(boxes.shape))],
)
model = helper.make_model(graph, opset_imports=[helper.make_opsetid("", 13)])
model.ir_version = 8
for k, v in {"names": "{0: 'button'}", "imgsz": "[64, 64]"}.items():
    model.metadata_props.append(onnx.StringStringEntryProto(key=k, value=v))
onnx.checker.check_model(model)
OUT.parent.mkdir(parents=True, exist_ok=True)
onnx.save(model, OUT)
print(OUT, OUT.stat().st_size, "bytes")
