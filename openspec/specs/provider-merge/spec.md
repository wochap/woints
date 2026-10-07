# provider-merge Specification

## Purpose
Conversion of vision detections to elements and duplicate-suppressing merge with AT-SPI elements.

## Requirements

### Requirement: Vision detections become elements
Each `Detection` SHALL be converted to an `Element` with `rect` in global logical coordinates via the frame's `rect` and `scale`, `source = ElementSource::Vision`, `confidence` copied, `actions = [Pointer]`, `role` from the class (`box`, `text line`, `link`, `image`), empty `name`.

#### Scenario: Mapping at scale 1.5
- **WHEN** a detection `rect_px = (150, 75, 300, 60)` comes from a frame with `rect = (100, 50, 1000, 800)` and `scale = 1.5`
- **THEN** the element rect is `(200, 100, 200, 40)`

### Requirement: Duplicate suppression by IoU
`merge(atspi: Vec<Element>, vision: Vec<Element>, params)` SHALL drop a vision element when its IoU with any AT-SPI element exceeds `params.iou_threshold` (default 0.3).

#### Scenario: Overlapping button
- **WHEN** a vision box `(10, 10, 100, 30)` and an AT-SPI box `(12, 11, 98, 30)` are merged
- **THEN** only the AT-SPI element remains

#### Scenario: Distinct boxes kept
- **WHEN** a vision box and an AT-SPI box have IoU 0.1
- **THEN** both remain

### Requirement: Duplicate suppression by containment
A vision element SHALL also be dropped when it contains at least `params.containment_threshold` (default 0.9) of an AT-SPI element's area and is at most 3× that element's area, or when an AT-SPI element contains ≥ 0.9 of the vision element's area and the AT-SPI element is at most 3× larger.

#### Scenario: Vision box around a labelled button
- **WHEN** a vision box `(0, 0, 120, 40)` fully contains AT-SPI box `(4, 4, 112, 32)`
- **THEN** the vision box is dropped

#### Scenario: Vision box inside a large panel
- **WHEN** a vision box `(10, 10, 50, 20)` lies inside an AT-SPI `panel` element of `(0, 0, 1000, 800)`
- **THEN** the vision box is kept

### Requirement: Ordering and provenance
Merged output SHALL list AT-SPI elements first, then vision elements sorted by confidence descending, and `snapshot` JSON SHALL expose `source` and `confidence` for every element.

#### Scenario: Snapshot shows provenance
- **WHEN** `snapshot` runs on a target where vision ran
- **THEN** every element has `"source": "atspi"` or `"source": "vision"` and a numeric `confidence`

### Requirement: Merge is pure and tested
`merge` SHALL be a pure function in `core` with unit tests for every scenario above.

#### Scenario: Headless
- **WHEN** `cargo test -p woints-core merge` runs
- **THEN** all merge tests pass without a display
