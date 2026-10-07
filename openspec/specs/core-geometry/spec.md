# core-geometry Specification

## Purpose
Typed coordinate spaces, transforms and element/target types shared by all woints crates.

## Requirements

### Requirement: Typed coordinate spaces
`core` SHALL define `Point<S>` and `Rect<S>` parameterised by a coordinate-space marker type with at least `WindowLocal`, `OutputLocal`, `Global` (logical pixels in Hyprland layout space) and `Device` (physical pixels of one output). Conversions between spaces SHALL only be possible through explicit transform functions that take the required context (window origin, output origin, output scale).

#### Scenario: Window-local to global
- **WHEN** a window-local rect `(10, 20, 100, 30)` is converted using a window whose global origin is `(1, 41)`
- **THEN** the result is the global rect `(11, 61, 100, 30)`

#### Scenario: Mixed spaces do not compile
- **WHEN** code attempts to compare or add a `Point<WindowLocal>` with a `Point<Global>` without a transform
- **THEN** compilation fails (verified by a `compile_fail` doctest)

### Requirement: Global to device pixels with fractional scale
`core` SHALL convert global logical rects to device pixels of a given output using the output's global origin and fractional scale, rounding the origin to the nearest device pixel and the size so that the far edge is also rounded (no accumulated drift).

#### Scenario: Scale 2 output at origin
- **WHEN** the global rect `(100, 50, 20, 10)` is converted for an output at `(0, 0)` with scale `2.0`
- **THEN** the device rect is `(200, 100, 40, 20)`

#### Scenario: Fractional scale 1.5
- **WHEN** the global rect `(101, 51, 21, 11)` is converted for an output at `(0, 0)` with scale `1.5`
- **THEN** the device rect origin is `(152, 77)` and the far edge is `round(122*1.5), round(62*1.5)` = `(183, 93)`, giving size `(31, 16)`

#### Scenario: Output with negative offset
- **WHEN** the global rect `(-1800, 100, 40, 40)` is converted for an output whose global origin is `(-1920, 0)` with scale `1.0`
- **THEN** the device rect is `(120, 100, 40, 40)`

### Requirement: Output containment test
`core` SHALL decide which output holds a global rect by the output containing the rect's centre point; ties or no containment SHALL fall back to the output with the largest overlap area, then to the focused output.

#### Scenario: Rect straddling two outputs
- **WHEN** a rect spans the seam between output A `(0,0,1920,1080)` and output B `(1920,0,1440,900)` with centre at `x = 1930`
- **THEN** the rect is assigned to output B

### Requirement: Element and Target types
`core` SHALL define `Target { surface: Toplevel{address} | Layer{address, namespace}, rect: Rect<Global>, output: OutputId, pid: Option<u32>, class: String }` and `Element { id: ElementId, rect: Rect<Global>, role: String, name: String, source: ElementSource, actions: Vec<ActionKind>, confidence: f32 }`, both `serde`-serialisable, so `snapshot` output and later providers share one shape.

#### Scenario: Snapshot JSON round-trip
- **WHEN** an `Element` is serialised to JSON and parsed back
- **THEN** the value is identical and the JSON uses the field names `id, rect, role, name, source, actions, confidence`
