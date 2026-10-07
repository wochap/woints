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
`TargetSelector::select(Scope::Focused)` SHALL return exactly one `Target` for the Hyprland active window: its global logical rect from `at`/`size`, its output chosen by centre containment, its pid and class.

#### Scenario: Active window on second monitor
- **WHEN** the active window is at `(2000, 100)` and output B spans `x ∈ [1920, 3360)`
- **THEN** the target's output is B

#### Scenario: No focused window
- **WHEN** no window is focused
- **THEN** `select` returns an empty target list and `hints` replies `{"ok":true,"state":"idle","elements":0}`

### Requirement: All scopes supported
`TargetSelector::select` SHALL accept every `Scope` variant; the M1 `Err(Unsupported)` behaviour for non-focused scopes is removed. Default scope comes from `scope.default` in config when the request omits it.

#### Scenario: Default from config
- **WHEN** `scope.default = "focused+layers"` and `{"cmd":"hints"}` arrives without `scope`
- **THEN** targets include the focused window and layers on its output
