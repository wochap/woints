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

### Requirement: WebKitGTK refinement
`ToolkitProfile` SHALL provide a refinement applied after lookup when the paired application is detected as WebKitGTK (its process maps `libwebkit2gtk-4.*` or `libwebkitgtk-6.0`). The refinement SHALL set `skip_web_content = false` and enable web-root discovery; strategies, filters and action preference are unchanged. Detection SHALL happen once per application and be cached with the application facts.

#### Scenario: Tauri app
- **WHEN** a `gtk` 3.x application maps `libwebkit2gtk-4.1.so`
- **THEN** its profile has `skip_web_content = false` and web-root discovery enabled

#### Scenario: Plain GTK3 app
- **WHEN** a `gtk` 3.x application maps no WebKit library
- **THEN** the profile equals the plain GTK3 profile

#### Scenario: Unknown pid
- **WHEN** the application's pid is unknown or its maps cannot be read
- **THEN** it is not treated as WebKitGTK
