# hint-filtering Specification

## Purpose
Typed-prefix filtering of visible hint badges and match resolution while the overlay is showing.

## Requirements

### Requirement: Typed-prefix filtering
While `Showing`, each forwarded key press whose keycode maps to a label glyph SHALL append that glyph to the typed buffer. Badges whose label starts with the buffer remain, drawn as one text run coloured per glyph: with `hint.prefix_style = "highlight-matched"` (default) matched glyphs use `hint.accent` and remaining glyphs `hint.fg_dim`; with `"dim-matched"` (Vimium style) matched glyphs use `hint.fg_dim` and remaining glyphs `hint.fg`. All others are eliminated. Matching is by keycode (M1 design D7), so the glyph shown and the key pressed always agree under the current layout.

#### Scenario: One glyph typed
- **WHEN** labels are `a s d fa fs` and the key for `f` is pressed
- **THEN** badges `fa` and `fs` remain with `f` highlighted and `a s d` are eliminated

#### Scenario: Non-label key
- **WHEN** a key that maps to no alphabet glyph (for example `q`) is pressed
- **THEN** the buffer is unchanged and nothing is eliminated

### Requirement: Backspace
Backspace SHALL remove the last glyph from the buffer and restore the badges that prefix-match the shorter buffer.

#### Scenario: Undo
- **WHEN** the buffer is `f` and Backspace is pressed
- **THEN** the buffer is empty and all badges are shown again

### Requirement: Elimination style
Eliminated badges SHALL disappear on the same frame when `hint.eliminated = "hide"`, or fade to 20 % alpha over `timing.fade_ms` (default 60) when `"fade"`.

#### Scenario: Fade
- **WHEN** `hint.eliminated = "fade"` and a badge is eliminated
- **THEN** it is drawn at 20 % alpha after 60 ms and removed on the next re-render

### Requirement: Selection and hand-off
When the buffer equals a label exactly, the session SHALL draw that badge with `flash_bg`/`flash_fg` for `timing.flash_ms` (default 80, hard cap 120, 0 allowed), unmap the overlay, wait for the unmap commit, then call the actuator for that element with the session's action. The session returns to `Idle` whether or not the action succeeds, and failures are logged and reported in `status.last_action`.

#### Scenario: Match and activate
- **WHEN** labels include `d` and the key for `d` is pressed
- **THEN** the `d` badge flashes, the overlay unmaps, `Actuator::act` is called once and the state becomes `Idle`

#### Scenario: Flash disabled
- **WHEN** `timing.flash_ms = 0`
- **THEN** the overlay unmaps immediately on match and the action fires

### Requirement: Mode chip reflects the buffer
The mode chip SHALL show the current action mode, the typed buffer in `hint.accent` and the scope, updating on every key.

#### Scenario: Buffer shown
- **WHEN** the buffer is `f`
- **THEN** the chip shows `f` in the buffer segment
