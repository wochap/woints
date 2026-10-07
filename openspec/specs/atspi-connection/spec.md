# atspi-connection Specification

## Purpose
Persistent accessibility bus connection, application/frame pairing with Hyprland windows and provider availability.

## Requirements

### Requirement: Persistent a11y bus connection
The `a11y` crate SHALL obtain the accessibility bus address by calling `org.a11y.Bus.GetAddress` on the session bus once, open one zbus connection to it at daemon start, keep it open for the daemon lifetime and reconnect with backoff if it drops. It SHALL NOT spawn `busctl` or any other helper process. Peer-to-peer (`p2p`) connections SHALL be disabled and every proxy SHALL be built with its destination bus name pinned.

#### Scenario: Connection reused across requests
- **WHEN** two `hints` requests arrive 1 s apart
- **THEN** the second request performs no `GetAddress` call and reuses the open connection, and `collect.connect` is under 1 ms

#### Scenario: Bus unavailable
- **WHEN** `GetAddress` fails or the bus socket cannot be opened
- **THEN** the provider returns `ProviderError::Unavailable`, the pipeline continues with other providers, and `status` reports `a11y: unavailable`

### Requirement: Application pairing by pid
The provider SHALL list applications under the Registry root and pair the Hyprland target with the application whose bus name owns the target pid (`org.freedesktop.DBus.GetConnectionUnixProcessID`). If no pid matches, it SHALL fall back to a fuzzy match between the Hyprland class and the application name (lowercase, `.` and `_` folded to `-`, substring either way, last `-` segment equality).

#### Scenario: Pid match
- **WHEN** the active window pid is 349213 and bus name `:1.42` is owned by that pid
- **THEN** the provider uses `:1.42` without consulting names

#### Scenario: Fuzzy fallback
- **WHEN** no bus name owns the pid and the class is `org.gnome.Calculator` while an app is named `gnome-calculator`
- **THEN** the provider pairs them by the fuzzy match

#### Scenario: No pairing
- **WHEN** neither pid nor name matches any application
- **THEN** the provider returns zero elements with reason `no-a11y-app`, recorded in the snapshot for `doctor`

### Requirement: Frame selection
Within the paired application the provider SHALL choose the frame carrying the ACTIVE state; if none, the frame whose `WINDOW` extents size equals the target size within 2 px; if still none, the first frame, with reason `frame-guess`.

#### Scenario: Two Firefox windows
- **WHEN** an application has two frames and one is ACTIVE
- **THEN** only the ACTIVE frame is collected

#### Scenario: No ACTIVE frame
- **WHEN** no frame is ACTIVE and the target size is 1438×858 and exactly one frame reports extents 1438×858
- **THEN** that frame is selected
