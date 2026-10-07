# woints-vision

Capture → detect for targets without usable AT-SPI elements.

## Features

| feature | effect |
|---|---|
| (none) | classical CV detector only; no ONNX Runtime symbols or libraries |
| `onnx` | `OnnxDetector` via `ort` (`load-dynamic`): `libonnxruntime.so` is loaded at runtime from `ORT_DYLIB_PATH`, never linked |
| `cuda` | `onnx` plus the CUDA execution provider (`vision.onnx.ep = "cuda"`); needs an onnxruntime built with CUDA |
| `migraphx` | `onnx` plus the MIGraphX execution provider (AMD; ONNX Runtime dropped ROCm in 1.23); needs an onnxruntime built with MIGraphX |

`ort` is pinned to `=2.0.0-rc.12` (ONNX Runtime API 1.24, matching nixpkgs
`onnxruntime` 1.24.4). Requested providers that are compiled out or fail to
register fall back to CPU with `onnx: ep <name> unavailable, using cpu`.

Pre/post-processing (`prep.rs`) is pure Rust and always compiled. The
end-to-end test against `tests/fixtures/const_boxes.onnx` runs when
`ORT_DYLIB_PATH` is set:

```sh
ORT_DYLIB_PATH=$(nix build --no-link --print-out-paths nixpkgs#onnxruntime)/lib/libonnxruntime.so \
  cargo test -p woints-vision --features onnx
```
