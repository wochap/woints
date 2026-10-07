# modifier-mapping Specification

## Purpose
Modifier keys held on the final label glyph select the action performed.

## Requirements

### Requirement: Modifier on the final glyph selects the action
When the typed glyph completes a label, the modifiers held during that key press SHALL select the action: default `Shift → right`, `Ctrl → middle`, `Alt → hover`, no modifier → the session's action. Modifiers on earlier glyphs SHALL be ignored.

#### Scenario: Shift on last glyph
- **WHEN** label `sd` is typed as `s` then `Shift+d`
- **THEN** the action performed is `right`

#### Scenario: Shift on first glyph only
- **WHEN** label `sd` is typed as `Shift+s` then `d`
- **THEN** the action performed is the session's action (`click` by default)

#### Scenario: Explicit action wins over modifier
- **WHEN** the session was started with `--action drag` and the final glyph is pressed with Shift
- **THEN** the drag flow continues and the modifier is ignored, with a debug log line

### Requirement: Configurable mapping
The mapping SHALL be configurable under `[modifiers]` with keys `shift`, `ctrl`, `alt`, `super`, each naming an action or `"none"`. Invalid action names SHALL be rejected at config load with the offending key named.

#### Scenario: Remap Alt
- **WHEN** config has `alt = "double"`
- **THEN** `Alt+<final glyph>` performs a double click

#### Scenario: Invalid value
- **WHEN** config has `ctrl = "teleport"`
- **THEN** config loading fails with `modifiers.ctrl: unknown action "teleport"`

### Requirement: Modifier keys do not filter hints
Pressing a modifier key alone SHALL NOT change the typed buffer or eliminate hints; the overlay SHALL match glyphs by keycode irrespective of Shift (so `Shift+d` still matches label glyph `d`).

#### Scenario: Shift pressed alone
- **WHEN** Shift is pressed and released without another key
- **THEN** the typed buffer and visible hints are unchanged

#### Scenario: Shifted glyph still matches
- **WHEN** the current buffer is `s` and `Shift+d` is pressed under a qwerty keymap
- **THEN** label `sd` is matched even though the utf8 of the key event is `D`
