# pointer-actuation Specification

## Purpose
Pointer actuator: Hyprland movecursor placement plus virtual-pointer button events.

## Requirements

### Requirement: Cursor placement through Hyprland IPC
`PointerActuator` SHALL move the cursor with Hyprland `dispatch movecursor <x> <y>` using global logical coordinates taken from `Rect<Global>::center()` of the element, rounded to the nearest integer. It SHALL NOT send `zwlr_virtual_pointer_v1.motion_absolute` in the default backend.

#### Scenario: Element on a second monitor with negative offset
- **WHEN** the chosen element's global rect is `(-1800, 100, 40, 40)`
- **THEN** the actuator dispatches `movecursor -1780 120` and the dispatch reply is `ok`

#### Scenario: Dispatch failure
- **WHEN** Hyprland replies anything other than `ok` to `movecursor`
- **THEN** the action aborts, no button event is sent, and `act` replies `{"ok":false,"error":"movecursor failed: <reply>"}`

### Requirement: Button events through the virtual pointer
The actuator SHALL send button events with `zwlr_virtual_pointer_v1.button(time, code, state)` using Linux codes 272 (left), 273 (right), 274 (middle), each followed by `frame()` and a display roundtrip. A click is press then release; a double click is two clicks separated by `input.double_click_gap_ms` (default 40).

#### Scenario: Left click sequence
- **WHEN** `click` is performed
- **THEN** the Wayland stream contains `button(272, pressed)`, `frame`, `button(272, released)`, `frame`, in that order, with a roundtrip after each frame

#### Scenario: Middle click
- **WHEN** `middle` is performed
- **THEN** the button code is 274

### Requirement: Single long-lived virtual pointer
The daemon SHALL bind `zwlr_virtual_pointer_manager_v1` once at start-up and create one `zwlr_virtual_pointer_v1` kept for the daemon's lifetime; it SHALL NOT open a new Wayland connection per click.

#### Scenario: Two consecutive clicks
- **WHEN** two `act` requests arrive 100 ms apart
- **THEN** both use the same virtual pointer object and no second `wl_display` connection is opened

#### Scenario: Manager absent
- **WHEN** the compositor does not advertise `zwlr_virtual_pointer_manager_v1`
- **THEN** `status` reports `pointer: unavailable` and pointer actions reply `{"ok":false,"error":"virtual pointer unavailable"}`

### Requirement: Overlay unmapped before the pointer acts
Before any pointer event, the daemon SHALL hide the overlay and wait for the overlay thread's confirmation that the unmap commit has been flushed; it SHALL NOT use a fixed sleep. The confirmation wait SHALL time out after `input.unmap_timeout_ms` (default 200) and proceed with a warning.

#### Scenario: Normal click
- **WHEN** a label is completed
- **THEN** the order of events is: overlay `hide` → unmap commit flushed → `movecursor` → button events

#### Scenario: Overlay thread unresponsive
- **WHEN** the unmap confirmation does not arrive within the timeout
- **THEN** a warning is logged and the click proceeds

### Requirement: Optional cursor restore
When `input.restore_cursor = true`, the actuator SHALL read the cursor position with Hyprland `cursorpos` before moving and dispatch `movecursor` back to it after the action's last button event. Default is `false`.

#### Scenario: Restore enabled
- **WHEN** `restore_cursor = true` and the cursor was at `(300, 400)`
- **THEN** after the click the cursor is moved back to `(300, 400)`

#### Scenario: Restore disabled for hover
- **WHEN** the action is `hover`
- **THEN** the cursor is never restored regardless of the setting

### Requirement: Alternative absolute-motion backend
An `absolute` backend SHALL be selectable with `input.pointer = "absolute"`. It SHALL create the virtual pointer with `create_virtual_pointer_with_output` for the element's output and call `motion_absolute(x, y, logical_w, logical_h)` in output-local logical coordinates (after applying the output transform), followed by `frame` and a roundtrip. It is not the default.

#### Scenario: Absolute backend on scale-2 output
- **WHEN** `input.pointer = "absolute"` and the element centre is global `(1500, 450)` on an output at origin `(0,0)` with logical size `1440×900`
- **THEN** `motion_absolute(1500, 450, 1440, 900)` is sent on a pointer bound to that output

### Requirement: Timing stage
The actuator SHALL record its total duration as stage `act` in `Timings` and log it at debug level.

#### Scenario: Timings include act
- **WHEN** an action completes
- **THEN** `status.last_timings` contains an `act` entry in milliseconds
