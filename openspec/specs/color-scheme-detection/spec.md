# color-scheme-detection Specification

## Purpose
Detect the system light/dark colour scheme via the XDG settings portal and resolve the active hint palette.

## Requirements

### Requirement: Portal colour scheme
The daemon SHALL read the system colour scheme from the XDG settings portal, `org.freedesktop.portal.Settings.ReadOne("org.freedesktop.appearance", "color-scheme")`, at startup and SHALL follow its `SettingChanged` signal for that namespace and key. Value `1` means dark, `2` means light, and `0` or any other value means no preference. No other source SHALL be consulted.

#### Scenario: Dark at startup
- **WHEN** the portal returns `1` at daemon start
- **THEN** the detected scheme is dark and `status` reports `scheme: dark, source: portal`

#### Scenario: Scheme flips
- **WHEN** the portal emits `SettingChanged("org.freedesktop.appearance", "color-scheme", 2)`
- **THEN** the detected scheme becomes light without a daemon restart

### Requirement: Auto theme resolution
With `hint.theme = "auto"` the active palette SHALL be `[theme.dark]` when the detected scheme is dark and `[theme.light]` when it is light. With `hint.theme = "dark"` or `"light"` the named palette SHALL be used regardless of the detected scheme.

#### Scenario: Auto follows light
- **WHEN** `hint.theme = "auto"` and the detected scheme is light
- **THEN** badges are drawn with the `[theme.light]` tokens

#### Scenario: Forced palette
- **WHEN** `hint.theme = "dark"` and the detected scheme is light
- **THEN** badges are drawn with the `[theme.dark]` tokens

### Requirement: Dark fallback
When the portal is unreachable, the call fails, or the value is `0` (no preference), the detected scheme SHALL be dark with `source: fallback`. The daemon SHALL keep retrying to connect to the portal with backoff, and SHALL switch to the portal value once it becomes available.

#### Scenario: No portal
- **WHEN** no `org.freedesktop.portal.Desktop` owner exists at start
- **THEN** `hint.theme = "auto"` uses `[theme.dark]` and `status` reports `scheme: dark, source: fallback`

#### Scenario: Portal appears later
- **WHEN** the portal starts after the daemon and reports `2`
- **THEN** the detected scheme becomes light and `source` becomes `portal`

### Requirement: Live re-render on scheme change
When the resolved palette changes because the detected scheme changed, the daemon SHALL apply it to the next request and SHALL re-render a showing session in place without cancelling it.

#### Scenario: Flip while showing
- **WHEN** hints are showing with `hint.theme = "auto"` and the scheme flips from dark to light
- **THEN** the overlay re-renders with the light tokens and the typed buffer and labels are unchanged

#### Scenario: Flip with forced palette
- **WHEN** `hint.theme = "dark"` and the scheme flips
- **THEN** the overlay is not re-rendered
