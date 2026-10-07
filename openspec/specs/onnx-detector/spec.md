# onnx-detector Specification

## Purpose
Feature-gated ONNX Runtime detector implementing the Detector port: warm session, execution providers, pre/post-processing and timing.

## Requirements

### Requirement: Feature-gated detector
The `onnx` cargo feature SHALL add `OnnxDetector: Detector` to `woints-vision`. Without the feature the crate SHALL compile with no `ort` dependency and `vision.detector = "onnx"` SHALL fail at config load with a clear message.

#### Scenario: Default build rejects onnx config
- **WHEN** a build without the `onnx` feature loads a config with `vision.detector = "onnx"`
- **THEN** startup fails with `vision.detector = "onnx" requires a build with the onnx feature` and exit code 1

#### Scenario: Both builds compile in CI
- **WHEN** CI runs `cargo build --workspace` and `cargo build --workspace --features woints-vision/onnx`
- **THEN** both succeed

### Requirement: Warm session
When enabled, the daemon SHALL create one `ort::Session` at startup, run one warm-up inference on a blank letterboxed frame, and reuse the session for every request. The session SHALL be rebuilt on SIGHUP only if `model_path` or EP settings changed.

#### Scenario: Warm-up at start
- **WHEN** `wointsd` starts with `vision.detector = "onnx"` and a valid model
- **THEN** the log shows `onnx: loaded <path> input=<shape> ep=<name> warmup_ms=<n>` before the socket is bound

#### Scenario: Model missing
- **WHEN** `model_path` does not exist
- **THEN** the daemon logs an error, disables the ONNX detector and continues with the classical detector; `status` reports `onnx: unavailable (<reason>)`

### Requirement: Execution provider selection
`vision.onnx.ep = "cpu" | "cuda" | "migraphx"` SHALL select the provider; providers compiled out SHALL fall back to CPU with a warning. CPU SHALL set intra-op threads to `vision.onnx.threads` (default: physical cores / 2).

#### Scenario: Unavailable EP
- **WHEN** `ep = "cuda"` on a build without the `cuda` feature
- **THEN** the detector runs on CPU and logs `onnx: ep cuda unavailable, using cpu`

### Requirement: Pre-processing
Frames SHALL be letterboxed to the model input size preserving aspect ratio (pad colour configurable, default grey 114), converted to the model's expected layout (`NCHW` float32 by default) and normalised per metadata (`scale`, `mean`, `std`).

#### Scenario: Letterbox of a 2880×1800 frame to 640×640
- **WHEN** a 2880×1800 frame is pre-processed for a 640×640 input
- **THEN** the image is scaled by `640/2880`, placed at vertical offset `(640 − 400) / 2 = 120`, and the returned `Letterbox { scale, pad_x, pad_y }` is `(0.2222, 0, 120)`

### Requirement: Post-processing
Outputs SHALL be decoded for YOLO-style `[1, 4+C, N]` / `[1, N, 4+C]` tensors and DETR-style `(boxes, scores, labels)` tensors, filtered by `min_confidence` and `interactive_classes`, deduplicated by NMS (`vision.onnx.nms_iou`, default 0.5) and mapped from letterbox space back to frame pixels, then to `Rect<Global>` via the capture's `OutputGeom`.

#### Scenario: Synthetic constant model
- **WHEN** the test model `tests/fixtures/const_boxes.onnx` returns two overlapping boxes with scores 0.9 and 0.8 for class 0 and one box with score 0.1
- **THEN** post-processing yields exactly one box (the 0.9 one) when `min_confidence = 0.3` and `nms_iou = 0.5`

#### Scenario: Box mapped back through letterbox
- **WHEN** a decoded box at letterbox coords `(0, 120, 640, 520)` comes from the 2880×1800 frame above
- **THEN** the frame-space box is `(0, 0, 2880, 1800)` and, for an output at global `(0,0)` scale 2, the global logical rect is `(0, 0, 1440, 900)`

### Requirement: Timing and bench
The detector SHALL record `detect.onnx` (pre, run, post as sub-spans) in `Timings`; `woints bench` SHALL show the row when enabled.

#### Scenario: Bench row
- **WHEN** `woints bench -n 20` runs with the ONNX detector enabled
- **THEN** output includes `detect.onnx` p50/p95 and the README records the measured values with the hardware name or `TODO(verify)`
