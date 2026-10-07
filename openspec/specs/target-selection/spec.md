# target-selection Specification

## Purpose
Scope selection and resolution of hint targets.

## Requirements

### Requirement: Scope enum
`core` SHALL define `Scope { Focused, FocusedAndLayers, Layers, All }`, parsed from the strings `focused`, `focused+layers`, `layers`, `all`.

#### Scenario: Parse
- **WHEN** `"focused+layers"` is parsed
- **THEN** the result is `Scope::FocusedAndLayers`

### Requirement: Focused scope resolution
`TargetSelector::select(Scope::Focused)` SHALL return exactly one `Target` for the Hyprland active window: its global logical rect from `at`/`size`, its output chosen by centre containment, its pid and class. Scopes other than `Focused` SHALL return `Err(Unsupported)` in this milestone.

#### Scenario: Active window on second monitor
- **WHEN** the active window is at `(2000, 100)` and output B spans `x ∈ [1920, 3360)`
- **THEN** the target's output is B

#### Scenario: No focused window
- **WHEN** no window is focused
- **THEN** `select` returns an empty target list and `hints` replies `{"ok":true,"state":"idle","elements":0}`
