# key-input Specification

## Purpose
How key presses reach the daemon while a session is showing, without taking keyboard focus from the application when the compositor allows it. Popups stay open, and the user can always get out of the key-capturing state.

## Requirements

### Requirement: Key source selection
The daemon SHALL read session keys from one key source chosen by `input.keys`:
- `"auto"` (default): the Hyprland submap source when the woints Hyprland module is loaded (its submap `input.submap` is listed in `j/binds`) on a Lua-enabled build; otherwise the overlay source.
- `"submap"`: the Hyprland submap source. If the module is not loaded or the build is not Lua-enabled, `hints` fails with that reason.
- `"overlay"`: the layer surface takes exclusive keyboard focus, as described in overlay-rendering "Keyboard handling".

Every source SHALL deliver the same key data to the session: keycode, keysym, utf8 when known, and modifiers. Label matching, Backspace, Tab, scroll keys and modifier mapping SHALL behave identically under every source. The daemon SHALL check for the module at startup, on daemon config reload and on Hyprland `configreloaded`.

#### Scenario: Auto on Hyprland
- **WHEN** `input.keys = "auto"` and `j/binds` lists binds in submap `woints-keys`
- **THEN** sessions use the submap source and the overlay surface takes no keyboard focus

#### Scenario: Auto falls back
- **WHEN** `input.keys = "auto"` and no bind is listed in submap `woints-keys`
- **THEN** sessions use the overlay source and `status` reports `keys: overlay (Hyprland module not loaded)`

#### Scenario: Submap required but missing
- **WHEN** `input.keys = "submap"` and the module is not loaded
- **THEN** `hints` fails with an error naming the missing module

#### Scenario: Shift on final glyph under submap
- **WHEN** the submap source is active, the buffer is `s`, and `Shift+d` is pressed
- **THEN** label `sd` is selected with the action mapped to Shift, as with the overlay source

### Requirement: Focus stays with the application
While a session uses the submap source, woints SHALL NOT take keyboard focus. An open xdg popup, menu or in-page dropdown of the focused application SHALL remain open while badges are shown and while keys are typed.

#### Scenario: Select list stays open
- **WHEN** a browser `<select>` list is open and `woints hints` runs
- **THEN** the list is still open when badges appear, and typing a label does not close it before the action fires

### Requirement: Submap key capture
The submap source SHALL capture keys through a Hyprland submap named `input.submap` (default `woints-keys`). The submap SHALL be defined by the woints Hyprland Lua module, which the user loads from their Hyprland config with `require("woints").setup({ submap = ..., timeout_ms = ... })`. The daemon SHALL NOT define, change or remove binds, submaps, timers or event listeners at runtime. It SHALL only enter and reset the submap and query `j/binds` and `j/submap`.

The module SHALL:
- bind only `catchall` to a no-op in the submap, so every key press is swallowed while the submap is active;
- while the current submap is `input.submap`, report every key press and release to the daemon as an event carrying the xkb keycode and the key state;
- on an Escape press, report Escape to the daemon and then reset the submap itself.

The daemon SHALL derive modifiers, keysym and utf8 by feeding the reported key states into an xkb state built from the main keyboard's active keymap. A key press that is not a modifier and maps to no session input (label glyph, Backspace, Tab, scroll key) SHALL end the session. The daemon SHALL enter the submap only after the overlay has committed the first frame of the session.

#### Scenario: Two-key label
- **WHEN** labels are two glyphs long, the submap is active, and `s` then `d` are pressed
- **THEN** the session stays in the submap after `s`, and label `sd` is selected after `d`

#### Scenario: Label key reaches the daemon
- **WHEN** the submap is active and the key at keycode 38 is pressed
- **THEN** the daemon receives a press of keycode 38 and the application receives nothing

#### Scenario: Modifier alone
- **WHEN** the submap is active and Shift is pressed and released alone
- **THEN** the session keeps showing with the typed buffer unchanged

#### Scenario: Remapped modifier
- **WHEN** the layout maps Caps Lock to Control (`caps:ctrl`) and Caps Lock is held while the final glyph is pressed
- **THEN** the action mapped to Ctrl is performed

#### Scenario: Unbound key ends the session
- **WHEN** the submap is active and `F5` is pressed
- **THEN** the session returns to idle, the overlay unmaps, Hyprland is in the default submap, and `F5` is not delivered to the application

#### Scenario: Layout change installs a new submap
- **WHEN** the active keyboard layout changes between sessions
- **THEN** the same submap `woints-keys` is used unchanged, no bind is defined or removed, and the daemon's xkb state follows the new layout

#### Scenario: Name clash avoided
- **WHEN** the user's Hyprland config defines a submap named `woints`
- **THEN** woints' key submap does not touch it, because the module and the daemon use `input.submap`

### Requirement: Never stuck in the submap
Hyprland SHALL never stay in the woints submap without a showing session for longer than the guarantees below allow.
- The module SHALL reset the submap on an Escape press inside the compositor, so Escape works even when the daemon is hung or dead.
- The module SHALL arm a compositor-side timer whenever the submap is entered and whenever a key is pressed in it. If `timeout_ms` (default 17000) passes with no key press while Hyprland is still in the submap, the timer SHALL reset it, whether or not the daemon is alive.
- When the session reaches idle by any path (action done, Escape, cancel, unbound key, timeout, monitor change, overlay death, error, `quit`), the daemon SHALL reset the submap if Hyprland is still in it.
- Before performing an action, the daemon SHALL reset the submap.
- At startup, if Hyprland's current submap is `input.submap`, the daemon SHALL reset it.
- On SIGTERM, SIGINT or `quit`, the daemon SHALL reset the submap before exiting.
- When Hyprland reports `configreloaded`, the daemon SHALL end the session, reset the submap, and check for the module again.

#### Scenario: Daemon killed mid-session
- **WHEN** the submap is active and `wointsd` is killed with SIGKILL
- **THEN** Escape returns Hyprland to the default submap, and with no key pressed the timer does so within `timeout_ms`

#### Scenario: Long scroll session
- **WHEN** scroll mode is active and `j` is pressed every few seconds for longer than `timeout_ms`
- **THEN** the timer does not reset the submap, because every key press re-arms it

#### Scenario: Restart after crash
- **WHEN** `wointsd` starts while Hyprland's current submap is `woints-keys`
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
`status` and `doctor` SHALL report the active key source (`submap <name>` or `overlay`) and, when `auto` fell back, the reason. When the module is not loaded on a Lua-enabled Hyprland build, `doctor` SHALL print a warning naming the fix: add `require("woints").setup()` to the Hyprland config.

#### Scenario: Doctor on Hyprland
- **WHEN** `woints doctor` runs on Hyprland with the module loaded
- **THEN** it prints a passing line `keys: submap woints-keys`

#### Scenario: Doctor without the module
- **WHEN** `woints doctor` runs on a Lua-enabled Hyprland without the module
- **THEN** it prints a warning `keys: overlay (Hyprland module not loaded); add require("woints").setup() to hyprland.lua`
