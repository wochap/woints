# Contributing

woints is licensed under MIT (`LICENSE`).

- MIT or Apache-2.0 prior art (e.g. computer-use-linux) may be adapted with
  attribution.
- GPL/AGPL prior art (hints, wl-kbptr, hyprland-mcp) is read for ideas only.
  Do not copy code from it.
- Never commit model weights (`.onnx`, `.pt`, `.safetensors`). Test fixtures
  are generated graphs with constant outputs (`scripts/make_const_onnx.py`).
- `just lint` and `just test` must pass with and without the `onnx` feature
  (`just test-onnx`, `just lint-onnx`).
