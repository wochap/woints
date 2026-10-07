# toolkit-profiles Specification

## Purpose
Per-toolkit profiles selecting collection strategies, phantom filters and action preferences.

## Requirements

### Requirement: Profile lookup by toolkit name
`core` SHALL define `ToolkitProfile { strategies: Vec<Strategy>, phantom_filters: Vec<PhantomFilter>, skip_web_content: bool, action_preference: Vec<String> }` and a lookup keyed by the AT-SPI `Application.ToolkitName` (case-insensitive: `gtk`, `qt`, `gecko`, `chromium`, default) refined by Hyprland class where stated.

#### Scenario: GTK4
- **WHEN** toolkit name is `GTK` with version 4.x
- **THEN** strategies are `[TreeWalk]` only

#### Scenario: GTK3 and Qt
- **WHEN** toolkit name is `gtk` 3.x or `Qt`
- **THEN** strategies are `[Collection, TreeWalk]` and Qt additionally has the `CollapsedComboPopup` phantom filter

#### Scenario: Browsers prune web content
- **WHEN** toolkit is `Gecko`, or toolkit is `Chromium` and the class is one of `brave-browser`, `google-chrome`, `chromium`
- **THEN** `skip_web_content` is true

#### Scenario: Electron keeps web content
- **WHEN** toolkit is `Chromium` and the class is not a known browser
- **THEN** `skip_web_content` is false

### Requirement: Profiles are data
Profiles SHALL be plain data constructed in one module with unit tests, overridable per class from configuration in M5 without code changes.

#### Scenario: Unknown toolkit
- **WHEN** toolkit name is unrecognised
- **THEN** the default profile `[Collection, TreeWalk]`, no filters, `skip_web_content = false` applies
