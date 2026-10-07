# model-policy Specification

## Purpose
Model weights policy: user-supplied ONNX models, metadata and overrides, license reporting and README recommendation rules.

## Requirements

### Requirement: No bundled weights
The repository, crates and Nix packages SHALL contain no model weights. Models SHALL be supplied by the user via `vision.onnx.model_path`.

#### Scenario: Package contents
- **WHEN** `nix build .#woints-onnx` completes
- **THEN** `result/` contains no `.onnx`, `.pt` or `.safetensors` file

### Requirement: Model metadata and overrides
The detector SHALL read `input_size`, `normalization`, `class_names` and `output_format` from ONNX model metadata keys (`woints.*` or Ultralytics-style `names`, `imgsz`) and SHALL allow TOML overrides under `[vision.onnx]`. Missing metadata without overrides SHALL be an error naming the missing key.

#### Scenario: Missing class names
- **WHEN** a model has no `names` metadata and `class_names` is not set in config
- **THEN** startup logs `onnx: class_names missing (set vision.onnx.class_names)` and the detector is disabled

### Requirement: License file reporting
`woints doctor` SHALL look for `LICENSE*` or `<model>.license` next to the model and print its first line; absence SHALL be reported as a warning, never an error.

#### Scenario: License present
- **WHEN** `LICENSE` beside the model begins with `Apache License`
- **THEN** doctor prints `model license: Apache License ... (LICENSE)`

### Requirement: README recommendation rule
README SHALL list candidate detectors with license, source URL and verdict, and SHALL only recommend models whose weights license is compatible with the project's MIT license. AGPL-3.0 weights (Ultralytics YOLO derivatives, OmniParser `icon_detect`) SHALL be listed as "usable locally by the user, not recommended, not redistributed".

#### Scenario: License matrix present
- **WHEN** a reader opens README's "ONNX detector" section
- **THEN** every listed model has license, URL and verdict columns, and no AGPL model is marked recommended
