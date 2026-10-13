# hyprland-ipc Specification

## Purpose
Socket-based Hyprland IPC: queries, dispatch and event stream.

## Requirements

### Requirement: Socket-based Hyprland IPC
The `hypr` crate SHALL talk to Hyprland through `$XDG_RUNTIME_DIR/hypr/$HYPRLAND_INSTANCE_SIGNATURE/.socket.sock` using the `j/` JSON request prefix and SHALL NOT spawn `hyprctl`.

#### Scenario: Active window query
- **WHEN** `active_window()` is called while a window is focused
- **THEN** it returns class, title, address, pid, global logical position `at`, `size`, monitor id, floating and xwayland flags parsed from the `j/activewindow` reply

#### Scenario: No active window
- **WHEN** `active_window()` is called with no focused window
- **THEN** it returns `Ok(None)` rather than an error

### Requirement: Monitor and layer queries
`hypr` SHALL expose `monitors()` (name, id, global `x y`, pixel `width height`, `scale`, `transform`, `focused`, active workspace), `clients()` and `layers()` (per monitor and level: `namespace`, global rect, `pid`, `address`).

#### Scenario: Monitor logical size
- **WHEN** a monitor reports pixel size `2880×1800` and scale `2`
- **THEN** `Monitor::logical_size()` returns `(1440, 900)` and `logical_rect()` is `(x, y, 1440, 900)`

#### Scenario: Layer with pid
- **WHEN** `layers()` runs with a quickshell bar mapped
- **THEN** the returned layer entry carries the quickshell pid and namespace `quickshell:bar`

### Requirement: Dispatch
`hypr` SHALL expose `dispatch(cmd)` for commands such as `movecursor x y` and `focuswindow address:0x...`, treating any reply other than `ok` as an error.

#### Scenario: movecursor
- **WHEN** `dispatch("movecursor 100 200")` is sent
- **THEN** the call returns `Ok(())` on an `ok` reply and `Err` carrying the reply text otherwise

### Requirement: Event stream
`hypr` SHALL expose an async stream of events from `.socket2.sock`, parsing at least `activelayout`, `focusedmon`, `activewindowv2`, `monitoradded`, `monitorremoved`, `submap` (the new submap name, empty for the default), `configreloaded`, and `custom` (the raw payload after `custom>>`).

#### Scenario: Monitor hot-plug
- **WHEN** a `monitoradded` event arrives
- **THEN** the daemon refreshes its monitor list before the next `hints` request

#### Scenario: Custom event
- **WHEN** the line `custom>>woints key 38 0` arrives
- **THEN** the stream yields a custom event with payload `woints key 38 0`

#### Scenario: Submap reset event
- **WHEN** the line `submap>>` arrives
- **THEN** the stream yields a submap event with an empty name

### Requirement: Runtime submap control
`hypr` SHALL be able to enter a named submap, reset to the default submap, read the current submap name, and tell whether `j/binds` lists any bind in a named submap. It SHALL use the dispatch syntax of the running build. `hypr` SHALL NOT define, change or remove binds, submaps or timers, and SHALL NOT send Lua `eval` scripts or `keyword` commands: on Hyprland 0.56.2, `Keybind:remove()` through `eval` crashes the compositor.

#### Scenario: Current submap
- **WHEN** Hyprland is in the default submap
- **THEN** reading the current submap returns an empty or `default` name, and both are treated as default

#### Scenario: Submap present
- **WHEN** `j/binds` lists a catchall bind with `submap = "woints-keys"`
- **THEN** asking whether submap `woints-keys` has binds returns true, and for `other` returns false

#### Scenario: Reinstall is idempotent
- **WHEN** the daemon starts, reloads its config, or sees `configreloaded` several times
- **THEN** it sends no bind or submap definition, so `j/binds` lists the module's binds exactly as the user's config defined them

#### Scenario: Unsupported build
- **WHEN** Hyprland is not a Lua-enabled build
- **THEN** no submap `woints-keys` is listed, and the daemon reports the overlay source without sending any definition

#### Scenario: Enter rejected
- **WHEN** entering the submap is rejected by Hyprland
- **THEN** the call returns an error carrying Hyprland's reply
