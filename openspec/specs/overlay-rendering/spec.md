# overlay-rendering Specification

## Purpose
Per-output layer-shell overlay rendering of hint badges and keyboard handling.

## Requirements

### Requirement: One layer surface per output
The overlay SHALL create a `zwlr_layer_shell_v1` surface on the OVERLAY layer for each output that holds at least one badge, anchored to all edges with exclusive zone `-1`, namespace `woints`, an empty input region (pointer passthrough) and keyboard interactivity `exclusive`.

#### Scenario: Badges on one output only
- **WHEN** all elements lie on output A
- **THEN** only output A gets a surface and output B is untouched

### Requirement: Fractional scale rendering
The overlay SHALL use `wp_fractional_scale_v1` (falling back to integer `wl_output.scale`) and `wp_viewporter`: buffer size is logical size × scale rounded, `set_buffer_scale(1)`, viewport destination is the logical size. All drawing happens in device pixels through `core` transforms; borders are `max(1, floor(scale))` device px.

#### Scenario: Scale 2 output
- **WHEN** a 1440×900 logical output reports preferred scale `240/120`
- **THEN** the buffer is `2880×1800` and a 24×21 logical badge renders as 48×42 device px

#### Scenario: Scale changes while showing
- **WHEN** a `preferred_scale` event arrives with a new value
- **THEN** the overlay re-renders at the new scale before the next frame

### Requirement: Badge appearance
Badges SHALL be drawn with `tiny-skia` as a filled rounded rect, a 1 px (device-scaled) stroke inset ½ px, and one text run in embedded Atkinson Hyperlegible Mono Bold using the `hint.*` tokens from the design (default theme Mocha: bg `#11111b`, fg `#cdd6f4`, border `#6c7086`, size 11, padding `[4,4]`, radius 3). The default badge height SHALL be 21 logical px (size 11 + 2 × padding 4 + 2 × border 1), matching a Vimium hint marker. When `hint.uppercase = true` (default) the label SHALL be drawn in capital letters; this changes only the drawn text, and filtering still matches typed keys by keycode.

#### Scenario: Demo render
- **WHEN** `hints` runs in demo mode on the focused window
- **THEN** badges labelled with generated labels appear at the window's corners and centre, and a PNG dump of the buffer (debug flag `WOINTS_DUMP=path`) shows the expected pixel sizes

#### Scenario: Default badge size
- **WHEN** a two-glyph label is drawn with default settings at scale 1
- **THEN** the badge is 24×21 logical px

#### Scenario: Uppercase display
- **WHEN** `hint.uppercase` is unset and the label is `as`
- **THEN** the badge shows `AS`, and pressing the keys for `a` then `s` selects it

#### Scenario: Lowercase opt-out
- **WHEN** the config sets `[hint] uppercase = false`
- **THEN** the badge shows `as`

### Requirement: Keyboard handling
While showing, the overlay SHALL receive all keyboard input, map keycodes through the compositor-provided xkb keymap, forward key presses (keycode, keysym, utf8, modifiers) to the daemon, and unmap on Escape. When keyboard focus leaves the overlay while it is showing and does not move to another woints overlay surface, the overlay SHALL unmap and the daemon SHALL return to idle in every showing mode (hints, drag phase 1 and 2, scroll). Focus leaving a surface that woints itself removed SHALL NOT end the session.

#### Scenario: Escape
- **WHEN** Escape is pressed while showing
- **THEN** the overlay unmaps within one frame and the daemon state returns to idle

#### Scenario: Letter forwarded
- **WHEN** the key at keycode 38 is pressed under a Colemak-DH keymap
- **THEN** the daemon receives keycode 38 with utf8 `a` and keysym `a` (same physical key, layout glyph)

#### Scenario: Another exclusive layer takes focus
- **WHEN** hints are showing and a launcher layer with exclusive keyboard interactivity opens
- **THEN** the overlay unmaps and the daemon state returns to idle, so after the launcher closes no badges remain and keys reach the focused window

#### Scenario: Focus loss in drag phase 2
- **WHEN** the session is picking a drag destination and the overlay loses keyboard focus
- **THEN** the session returns to idle, not to drag phase 1

#### Scenario: Focus moves between woints surfaces
- **WHEN** badges span two outputs and keyboard focus moves from the woints surface on one output to the woints surface on the other
- **THEN** the session keeps showing

#### Scenario: Surface removed by re-show
- **WHEN** a re-show drops the woints surface on an output that no longer holds badges
- **THEN** the session keeps showing on the remaining surfaces

### Requirement: Mode chip
The overlay SHALL draw the mode chip (mode, typed buffer, scope) in the bottom-right of the active output, 12 px inset, 22 px high, per the design, moving to bottom-left if any badge would intersect it.

#### Scenario: Chip collision
- **WHEN** a badge rect intersects the bottom-right chip rect
- **THEN** the chip is drawn bottom-left instead

### Requirement: Physical size mode
When `hint.size_unit = "mm"`, the overlay SHALL derive the badge font size from each output's physical size reported by `wl_output.geometry` (`size_mm` ÷ physical width mm × device px), scaling padding and radius by the same factor, so badges have the same physical size on every output. When an output reports 0 mm, it SHALL fall back to logical sizing for that output. Default is `size_unit = "logical"`.

#### Scenario: Two outputs, mm mode
- **WHEN** `size_unit = "mm"`, `size_mm = 3.0`, output A is 1920 px / 527 mm wide and output B is 2880 px / 302 mm wide
- **THEN** the font renders at about 11 device px on A and about 29 device px on B

#### Scenario: Missing physical size
- **WHEN** an output reports `0 × 0` mm
- **THEN** that output uses logical sizing and a warning is logged once

### Requirement: Drag source pinned
In drag phase 2 the overlay SHALL draw the source badge filled with `hint.drag_bg` and text `hint.drag_fg`, keep its label text, and draw a 1 px `hint.drag_bg` outline around the source target rect; destination badges use the idle style.

#### Scenario: Phase 2 render
- **WHEN** the session enters `ShowingSecond`
- **THEN** exactly one badge uses `drag_bg` and the outline rect equals the source element rect

### Requirement: Scroll stroke and rail
In scroll mode the overlay SHALL hide all badges, draw a 2 px `hint.scroll` stroke inset 1 px around the container rect, and, when the container exposes a scrollbar `Value` (current, minimum, maximum), draw a 3 px rail at the container's right edge with a thumb whose length is `viewport / content` of the rail and whose position follows the value. Without a `Value` the rail is omitted.

#### Scenario: Scroll stroke
- **WHEN** the session enters `Scrolling` on a container at `(100, 100, 400, 300)`
- **THEN** a 2 px stroke is drawn at `(101, 101, 398, 298)` and no badges are visible

#### Scenario: No scrollbar value
- **WHEN** the container is a vision element
- **THEN** the stroke is drawn and no rail

### Requirement: Mode chip states for actions
The chip SHALL show the mode text per design §05: `click`, `right-click`, `middle`, `double`, `hover`, `focus`, `drag 1/2 · pick source`, `drag 2/2 · pick destination` with a peach square, `scroll · hjkl ←↓↑→`, `copy-text`, plus the typed buffer and scope.

#### Scenario: Drag phase 2 chip
- **WHEN** the session is `ShowingSecond`
- **THEN** the chip reads `drag 2/2` with sub-text `pick destination` and shows the peach square

### Requirement: Unmap confirmation
The overlay thread SHALL answer `hide` with an `Unmapped` message after the destroying/unmapping commits have been flushed to the compositor, so the daemon can sequence pointer events.

#### Scenario: Hide round trip
- **WHEN** the daemon sends `hide`
- **THEN** it receives `Unmapped` after `wl_display.flush` returns for the commit that removed the surfaces

### Requirement: Vision badge style
Badges for elements with `source = vision` SHALL use a dashed 1 px (device-scaled) border in `hint.vision` with dash pattern `hint.vision_dash` (default `[2, 2]`), unchanged fill and text, and SHALL draw the target outline (dashed, `hint.vision`, radius 2) under the badge even when `hint.outline = false`.

#### Scenario: Mixed sources on one output
- **WHEN** a snapshot has AT-SPI and vision elements
- **THEN** AT-SPI badges have a solid `hint.border` stroke and vision badges have a dashed `hint.vision` stroke with an outline on their target rect

#### Scenario: Dash scaled
- **WHEN** the output scale is 2
- **THEN** the dash pattern is `[4, 4]` device px
