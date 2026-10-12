# key-input Specification

## Purpose
How key presses reach the daemon while a session is showing, without taking keyboard focus from the application when the compositor allows it. Popups stay open, and the user can always get out of the key-capturing state.

## Requirements

### Requirement: Key source selection
The daemon SHALL read session keys from one key source chosen by `input.keys`:
- `"auto"` (default): the Hyprland submap source when it installs successfully, otherwise the overlay source.
- `"submap"`: the Hyprland submap source. If it cannot be installed, `hints` fails with the install error.
- `"overlay"`: the layer surface takes exclusive keyboard focus, as described in overlay-rendering "Keyboard handling".

Every source SHALL deliver the same key data to the session: keycode, keysym, utf8 when known, and modifiers. Label matching, Backspace, Tab, scroll keys and modifier mapping SHALL behave identically under every source.

#### Scenario: Auto on Hyprland
- **WHEN** `input.keys = "auto"` and the submap installs at startup
- **THEN** sessions use the submap source and the overlay surface takes no keyboard focus

#### Scenario: Auto falls back
- **WHEN** `input.keys = "auto"` and installing the submap fails
- **THEN** sessions use the overlay source, a warning names the install error, and `status` reports `keys: overlay (submap install failed: <reason>)`

#### Scenario: Shift on final glyph under submap
- **WHEN** the submap source is active, the buffer is `s`, and `Shift+d` is pressed
- **THEN** label `sd` is selected with the action mapped to Shift, as with the overlay source

### Requirement: Focus stays with the application
While a session uses the submap source, woints SHALL NOT take keyboard focus. An open xdg popup, menu or in-page dropdown of the focused application SHALL remain open while badges are shown and while keys are typed.

#### Scenario: Select list stays open
- **WHEN** a browser `<select>` list is open and `woints hints` runs
- **THEN** the list is still open when badges appear, and typing a label does not close it before the action fires

### Requirement: Submap key capture
The submap source SHALL capture keys through a dedicated Hyprland submap whose name is `input.submap` (default `woints-keys`) followed by `-` and a short hash of its bind table. It SHALL be defined by the daemon at runtime and SHALL NOT require any user config. The daemon SHALL NOT remove or unbind Hyprland binds at runtime. A changed bind table SHALL be installed under a new name, and an already-defined name SHALL be reused. The submap SHALL bind:
- every label key, Backspace, Tab, and the scroll keys (`h j k l` and arrows), each with no modifier and with each of Shift, Ctrl, Alt and Super, to an event that tells the daemon the key and its modifiers;
- the modifier keys alone to a no-op;
- Escape to an event that tells the daemon Escape was pressed, plus a submap reset done by the compositor;
- every other key (catchall) to a submap reset.

The daemon SHALL enter the submap only after the overlay has committed the first frame of the session.

#### Scenario: Label key reaches the daemon
- **WHEN** the submap is active and the key at keycode 38 is pressed
- **THEN** the daemon receives a key with keycode 38 and the application receives nothing

#### Scenario: Modifier alone
- **WHEN** the submap is active and Shift is pressed and released alone
- **THEN** the session keeps showing with the typed buffer unchanged

#### Scenario: Unbound key ends the session
- **WHEN** the submap is active and `F5` is pressed
- **THEN** Hyprland leaves the submap, the session returns to idle, the overlay unmaps, and `F5` is not delivered to the application

#### Scenario: Layout change installs a new submap
- **WHEN** the active keyboard layout changes so the bind table differs
- **THEN** the next session uses a submap with a different hash suffix, and no bind is removed from Hyprland

#### Scenario: Name clash avoided
- **WHEN** the user's Hyprland config defines a submap named `woints`
- **THEN** woints' key submap does not modify it, because woints uses `input.submap`

### Requirement: Never stuck in the submap
Hyprland SHALL never stay in the woints submap without a showing session for longer than the guarantees below allow.
- Escape and the catchall SHALL reset the submap inside the compositor, so they work even when the daemon is hung or dead.
- When the session reaches idle by any path (action done, Escape, cancel, timeout, monitor change, overlay death, error, `quit`), the daemon SHALL reset the submap if Hyprland is still in it.
- Before performing an action, the daemon SHALL reset the submap.
- At startup, if Hyprland's current submap name starts with `input.submap` followed by `-`, the daemon SHALL reset it.
- On SIGTERM, SIGINT or `quit`, the daemon SHALL reset the submap before exiting.
- On Lua builds, entering the submap SHALL also arm a compositor-side timer. If the same session is still in the submap after `timing.max_showing_ms` plus 2 s, the timer SHALL reset it, whether or not the daemon is alive.
- When Hyprland reports `configreloaded`, the daemon SHALL end the session, reset the submap, and reinstall it.

#### Scenario: Daemon killed mid-session
- **WHEN** the submap is active and `wointsd` is killed with SIGKILL
- **THEN** Escape (or any unbound key) returns Hyprland to the default submap, and on Lua builds the timer does so without any key within `max_showing_ms + 2 s`

#### Scenario: Restart after crash
- **WHEN** `wointsd` starts while Hyprland's current submap is `woints-keys-3fa9c1d2`
- **THEN** the daemon resets the submap before accepting requests

#### Scenario: Action resets first
- **WHEN** a label is completed in the submap
- **THEN** the submap is reset before the click or AT-SPI action is performed

#### Scenario: Config reload during a session
- **WHEN** Hyprland's config is reloaded while hints are showing
- **THEN** the session returns to idle, Hyprland is in the default submap, and the next `hints` works

### Requirement: Session ends on outside changes
With the submap source, the daemon SHALL end a showing session (every showing mode: hints, drag phase 1 and 2, scroll) and unmap the overlay when either:
- Hyprland leaves the woints submap for a reason the daemon did not cause and that was not preceded by the Escape event (Escape keeps its per-mode meaning: idle, or drag phase 1 from phase 2); or
- the active window changes to one other than the session's focused window when it started.

#### Scenario: Submap reset from outside
- **WHEN** hints are showing and Escape is pressed
- **THEN** the overlay unmaps and the daemon returns to idle

#### Scenario: Focus moves by mouse
- **WHEN** hints are showing and the user clicks another window
- **THEN** the overlay unmaps, the daemon returns to idle, and Hyprland is in the default submap

#### Scenario: Escape in drag phase 2
- **WHEN** the session is picking a drag destination and Escape is pressed under the submap source
- **THEN** the session returns to drag phase 1 as with the overlay source, and the daemon re-enters the submap

#### Scenario: Unbound key in drag phase 2
- **WHEN** the session is picking a drag destination and an unbound key is pressed under the submap source
- **THEN** the session returns to idle, not to drag phase 1

### Requirement: Key source reporting
`status` and `doctor` SHALL report the active key source (`submap <name>` or `overlay`) and, when `auto` fell back, the reason.

#### Scenario: Doctor on Hyprland
- **WHEN** `woints doctor` runs on Hyprland with the submap installed
- **THEN** it prints a passing line `keys: submap woints-keys-<hash>`
