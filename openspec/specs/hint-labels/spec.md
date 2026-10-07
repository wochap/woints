# hint-labels Specification

## Purpose
Prefix-free hint label generation, ordering and badge placement.

## Requirements

### Requirement: Prefix-free variable-length labels
Given an alphabet of `k` glyphs and `n` targets, the label generator SHALL produce `n` distinct labels of 1 to 3 glyphs such that no label is a prefix of another, using as many single-glyph labels as possible, then two-glyph labels built from the remaining alphabet tail, then three-glyph labels.

#### Scenario: Fewer targets than glyphs
- **WHEN** alphabet is `asdfghjkl` and `n = 5`
- **THEN** labels are `a s d f g`

#### Scenario: Forty targets
- **WHEN** alphabet is `asdfghjkl` and `n = 40`
- **THEN** 5 labels are single glyphs, 35 are two glyphs whose first glyph is one of the last 4 alphabet glyphs, and no label is a prefix of another

#### Scenario: More than 81 targets
- **WHEN** alphabet is `asdfghjkl` and `n = 200`
- **THEN** 200 distinct prefix-free labels are produced, none longer than 3 glyphs

### Requirement: Priority ordering
Shorter labels SHALL be assigned to targets with higher priority, where priority is computed from target area (larger first) and distance to a focus point (the cursor, or the focused window centre when the cursor is outside it); layer targets SHALL rank after toplevel targets.

#### Scenario: Central large target gets single glyph
- **WHEN** 12 targets include one 200×40 button at the window centre and 11 small 16×16 icons at the edge
- **THEN** the button receives a single-glyph label

### Requirement: Badge placement rules
Badge placement SHALL follow the design rules: preferred slot at target top-left plus `hint.offset`; targets smaller than `hint.center_below` in either dimension are centred; badges are clamped inside their output with a 1 px inset; badges placed in reading order never overlap another badge by trying below, right of the blocker (max 3 hops), then above; unresolved collisions keep the preferred slot and are logged.

#### Scenario: Flush top-left target
- **WHEN** a target at output-local `(0, 0, 40, 40)` with offset `(-3, -3)` is placed
- **THEN** the badge origin is clamped to `(1, 1)`

#### Scenario: Dense toolbar
- **WHEN** 12 targets of 16×16 at an 18 px pitch are placed with two-glyph 22×17 badges
- **THEN** alternate badges are placed below their targets and no two badges intersect

### Requirement: Pure and tested
Label generation and placement SHALL be pure functions in `core` with no Wayland or D-Bus dependencies and SHALL have unit tests for every scenario above.

#### Scenario: Tests run headless
- **WHEN** `cargo test -p woints-core` runs without a Wayland display
- **THEN** all label and placement tests pass
