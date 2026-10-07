# vision-detection Specification

## Purpose
Classical CV detection of interactive regions in captured frames behind a swappable Detector port.

## Requirements

### Requirement: Detector port
The `vision` crate SHALL define `trait Detector { fn detect(&mut self, frame: &Frame, params: &DetectParams) -> Vec<Detection> }` where `Detection { rect_px: Rect<FramePx>, confidence: f32, class: DetectionClass }` and `DetectionClass ∈ { Box, TextLine, Link, Image, Unknown }`, so an ONNX-backed detector can implement it later without pipeline changes.

#### Scenario: Trait object swap
- **WHEN** the pipeline is given a `Box<dyn Detector>` returning fixed detections
- **THEN** the pipeline produces vision elements without any change to merge or labelling code

### Requirement: Classical detector pipeline
`ClassicalDetector` SHALL: convert to grayscale; downscale so width ≤ `params.downscale_width` (default 1920); compute edges (Canny with `params.canny = (low, high)`, default `(70, 220)`); dilate with a kernel of `max(2, round(3 × frame.scale / downscale_factor))` px; find connected components; emit bounding rects; then apply filters below; then map rects back to full-frame pixels.

#### Scenario: Buttons on a flat background
- **WHEN** a synthetic 800×600 frame contains five 120×36 rounded buttons on a flat background
- **THEN** exactly five detections are returned, each within 2 px of its button rect, class `Box`

#### Scenario: Dense toolbar
- **WHEN** a synthetic frame contains 12 icons of 24×24 at a 32 px pitch
- **THEN** 12 detections are returned and none spans two icons

### Requirement: Size, aspect and nesting filters
Detections SHALL be dropped when either side in logical px is below `params.min_box` (default 8) or above `params.max_box` (default 600), when aspect ratio exceeds `params.max_aspect` (default 20), or when the rect is nested inside another detection with the inner area above 85 % of the outer (keep the outer) or when the outer contains more than 6 inner detections (keep the inners, drop the outer).

#### Scenario: Window-sized rect dropped
- **WHEN** a detection covers 95 % of the frame and contains 20 smaller detections
- **THEN** the large detection is dropped and the 20 remain

#### Scenario: Tiny noise dropped
- **WHEN** a detection is 4×4 logical px
- **THEN** it is dropped

### Requirement: Text-line merging
Components whose height in logical px is at most `params.text_height` (default 15) SHALL be merged into one `TextLine` detection when their vertical centres differ by at most 3 px and the horizontal gap is at most 4 px, iterating until stable.

#### Scenario: Words become one line
- **WHEN** a synthetic frame has the words "Open recent file" rendered as three separate components 12 px tall with 3 px gaps
- **THEN** one `TextLine` detection covers all three words

### Requirement: Link heuristic
When `params.link_hue` is enabled (default on), `TextLine` detections whose pixels are at least 40 % saturated (s > 0.5) with hue in `[200°, 270°]` SHALL be classified `Link` with confidence raised by 0.2.

#### Scenario: Blue text is a link
- **WHEN** a text line is rendered in `#1e66f5` on white
- **THEN** its class is `Link`

### Requirement: Confidence
Each detection SHALL carry a confidence in `[0, 1]`: base 0.5 for `Box`, 0.4 for `TextLine`, plus 0.2 for rounded-rect-like edge density, plus 0.2 for `Link`, capped at 1.0.

#### Scenario: Plain box
- **WHEN** a sharp-cornered box is detected with no other signals
- **THEN** its confidence is 0.5

### Requirement: Warm buffers and headless tests
Detector scratch buffers SHALL be reused between frames of equal size. All detector requirements SHALL be covered by unit tests on synthetic images that run without a display.

#### Scenario: Headless test run
- **WHEN** `cargo test -p woints-vision` runs with no `WAYLAND_DISPLAY`
- **THEN** all detector and merge tests pass

### Requirement: Detector chain selection
`vision.detector` SHALL accept `"classical"` (default), `"onnx"` and `"onnx+classical"`. For `"onnx+classical"` both detectors run on the same frame (ONNX first), their boxes are unioned and merged by the M4 IoU merge with ONNX boxes winning ties; `confidence` is kept per source.

#### Scenario: Union then merge
- **WHEN** ONNX returns a box `A` and classical returns a box `B` with `IoU(A,B) = 0.7` and `merge.iou = 0.5`
- **THEN** the result contains `A` only, with `source = Vision(Onnx)`

#### Scenario: ONNX unavailable at runtime
- **WHEN** `vision.detector = "onnx+classical"` and the ONNX detector is disabled (missing model)
- **THEN** the classical detector still runs and `status` shows `detector: classical (onnx unavailable)`
