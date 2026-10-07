# atspi-actions Specification

## Purpose
Action kind tagging on AT-SPI elements and the actuator chain that performs activate/focus actions.

## Requirements

### Requirement: Action kind tagging
During collection each element SHALL be tagged with `ActionKind::Activate` when it exposes `org.a11y.atspi.Action` with at least one action, `ActionKind::Focus` when its role is entry, password text or it has EDITABLE text, and `ActionKind::Pointer` otherwise. Tags are serialised in `Element.actions` for `snapshot`.

#### Scenario: Button
- **WHEN** a push button exposes actions `["click"]`
- **THEN** its `actions` contain `activate`

#### Scenario: Entry
- **WHEN** an element has role entry
- **THEN** its `actions` contain `focus` first

### Requirement: DoAction preference
`AtspiActuator` SHALL call `Action.DoAction(i)` where `i` is the first action whose name (case-insensitive) is in the profile's `action_preference` (default `click, press, activate, toggle`), falling back to index 0.

#### Scenario: Preferred action
- **WHEN** actions are `["press", "release", "click"]`
- **THEN** `DoAction(2)` is called

#### Scenario: No preferred name
- **WHEN** actions are `["jump"]`
- **THEN** `DoAction(0)` is called

### Requirement: Focus for text entries
For `ActionKind::Focus` elements the actuator SHALL call `Component.GrabFocus` instead of `DoAction`.

#### Scenario: Entry focus
- **WHEN** an entry is selected with the default action
- **THEN** `GrabFocus` is called and `DoAction` is not

### Requirement: Pointer kinds deferred
For `ActionKind::Pointer` elements, or when the requested action is not `activate`/`focus` (for example `right`), the actuator SHALL return `ActError::NeedsPointer` and the daemon SHALL log `no actuator for <action>`; M3 supplies the pointer actuator.

#### Scenario: Right click requested
- **WHEN** `act` is called with action `right` on a button
- **THEN** the result is `NeedsPointer` and the session returns to `Idle`
