# action-modes Specification

## Purpose
Action vocabulary, routing between AT-SPI and pointer actuators, and the hover, drag, scroll and copy modes.

## Requirements

### Requirement: Action vocabulary
`core` SHALL define `Action { Click, Right, Middle, Double, Hover, Focus, Drag, Scroll, Copy }`, parsed from the lowercase strings `click`, `right`, `middle`, `double`, `hover`, `focus`, `drag`, `scroll`, `copy`. Every `Element` SHALL expose `actions: Vec<Action>` listing what it supports.

#### Scenario: Parse action
- **WHEN** `"double"` is parsed
- **THEN** the result is `Action::Double`

#### Scenario: Pointer-only element lists pointer actions
- **WHEN** an element has no AT-SPI interfaces (vision or GTK4 without Action)
- **THEN** its `actions` are `click, right, middle, double, hover, drag, scroll` and not `focus` or `copy`

### Requirement: Element-to-actuator routing
For `click` and `focus`, the daemon SHALL use `AtspiActuator` when the element has a matching AT-SPI capability (`Action` with a click/press/activate action, or `Component.GrabFocus`) and `input.prefer` is `atspi` (default); otherwise it SHALL use `PointerActuator` at the rect centre. `right`, `middle`, `double`, `hover`, `drag`, `scroll` SHALL always use the pointer.

#### Scenario: GTK3 button
- **WHEN** a push button with AT-SPI action `click` is chosen with `click`
- **THEN** `DoAction` is called and no pointer event is sent

#### Scenario: Prefer pointer
- **WHEN** `input.prefer = "pointer"` and the same button is chosen
- **THEN** a pointer click at the rect centre is performed

#### Scenario: Entry focus
- **WHEN** an `entry` element is chosen with `focus`
- **THEN** `GrabFocus` is called; if it returns `false`, a pointer click at the centre is performed as fallback

### Requirement: Hover
`hover` SHALL move the cursor to the element centre and send no button events.

#### Scenario: Hover
- **WHEN** `hover` is chosen
- **THEN** `movecursor` is dispatched and no `button` event appears

### Requirement: Drag, two phases
`drag` SHALL run in two phases. Phase 1 shows hints; the chosen element becomes the source. Phase 2 relabels all remaining elements with fresh labels (source excluded from the alphabet pool), draws the source pinned, and waits for a destination. On destination: `movecursor` to the source centre, `button(272, pressed)`, then at least `input.drag_steps` (default 8) `motion(dx, dy)` relative events toward the destination with `frame` + roundtrip each, then `movecursor` to the destination centre, `button(272, released)`. Escape in phase 2 returns to phase 1 with the original hints; Escape in phase 1 cancels.

#### Scenario: Drag completes
- **WHEN** source A at centre `(100,100)` and destination B at centre `(500,300)` are chosen
- **THEN** the stream is `movecursor 100 100`, press, 8 relative motions summing to `(400, 200)`, `movecursor 500 300`, release

#### Scenario: Source not selectable twice
- **WHEN** phase 2 is active
- **THEN** the source element has no label and typing cannot select it

#### Scenario: Escape in phase 2
- **WHEN** Escape is pressed during phase 2
- **THEN** the session returns to phase 1 with the original labels and no button event has been sent

### Requirement: Scroll mode
`scroll` SHALL first show hints only on scrollable containers (AT-SPI elements with role `scroll pane`, `list`, `tree`, `document web`, `document frame`, or any element exposing a vertical/horizontal scrollbar child; for pointer-only targets, every element). If exactly one container exists, phase 1 is skipped. In scroll mode the overlay stays mapped with keyboard focus; `h j k l` and arrow keys send `axis(vertical|horizontal, value)` plus `axis_discrete`/`axis_value120` events at the container centre, each followed by `frame` + roundtrip; `input.scroll_step` (default 3 lines, 15 logical px per line). Escape exits scroll mode; Tab returns to container picking.

#### Scenario: Scroll down
- **WHEN** scroll mode is active and `j` is pressed
- **THEN** `movecursor` to the container centre (once, on entry), then `axis(vertical, 45.0)`, `axis_discrete(vertical, 3)`, `frame`

#### Scenario: Single container skips picking
- **WHEN** `hints --action scroll` finds exactly one scrollable container
- **THEN** the session enters `Scrolling` immediately without showing labels

#### Scenario: Tab re-picks
- **WHEN** Tab is pressed in scroll mode
- **THEN** the session returns to container picking with labels

### Requirement: Copy text
`copy` SHALL read the element's text through AT-SPI `Text.GetText(0, -1)` (falling back to `Accessible.Name`) and set the clipboard through `zwlr_data_control_manager_v1` with MIME `text/plain;charset=utf-8`. If the protocol is not advertised, `copy` SHALL reply `{"ok":false,"error":"clipboard unavailable: zwlr_data_control_v1 not advertised"}` and SHALL NOT fall back to external tools.

#### Scenario: Copy a label
- **WHEN** a label element with text `Hello` is chosen with `copy` and data control is available
- **THEN** the clipboard offers `text/plain;charset=utf-8` with content `Hello`

#### Scenario: No data control
- **WHEN** `zwlr_data_control_manager_v1` is absent
- **THEN** the reply is an error and `status` reports `clipboard: unavailable`

### Requirement: Default action per element
When no action is given, the default SHALL be `click`; the config key `actions.default` MAY change it.

#### Scenario: No action given
- **WHEN** `{"cmd":"hints","scope":"focused"}` is received
- **THEN** the session's action is `click`
