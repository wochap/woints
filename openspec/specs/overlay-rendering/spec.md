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
- **THEN** the buffer is `2880×1800` and a 22×17 logical badge renders as 44×34 device px

#### Scenario: Scale changes while showing
- **WHEN** a `preferred_scale` event arrives with a new value
- **THEN** the overlay re-renders at the new scale before the next frame

### Requirement: Badge appearance
Badges SHALL be drawn with `tiny-skia` as a filled rounded rect, a 1 px (device-scaled) stroke inset ½ px, and one text run in embedded Atkinson Hyperlegible Mono Bold using the `hint.*` tokens from the design (default theme Mocha: bg `#11111b`, fg `#cdd6f4`, border `#6c7086`, size 11, padding `[2,3]`, radius 3).

#### Scenario: Demo render
- **WHEN** `hints` runs in demo mode on the focused window
- **THEN** badges labelled with generated labels appear at the window's corners and centre, and a PNG dump of the buffer (debug flag `WOINTS_DUMP=path`) shows the expected pixel sizes

### Requirement: Keyboard handling
While showing, the overlay SHALL receive all keyboard input, map keycodes through the compositor-provided xkb keymap, forward key presses (keycode, keysym, utf8, modifiers) to the daemon, and unmap on Escape.

#### Scenario: Escape
- **WHEN** Escape is pressed while showing
- **THEN** the overlay unmaps within one frame and the daemon state returns to idle

#### Scenario: Letter forwarded
- **WHEN** the key at keycode 38 is pressed under a Colemak-DH keymap
- **THEN** the daemon receives keycode 38 with utf8 `a` and keysym `a` (same physical key, layout glyph)

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
