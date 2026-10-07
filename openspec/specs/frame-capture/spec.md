# frame-capture Specification

## Purpose
Capture of toplevel and region pixels into frames for vision detection, excluding the woints overlay.

## Requirements

### Requirement: FrameCapture port
The `vision` crate SHALL define `trait FrameCapture { async fn capture(&mut self, target: &Target) -> Result<Frame> }` where `Frame { rgba: Vec<u8>, width: u32, height: u32, rect: Rect<Global>, scale: f32 }` so that frame pixel `(px, py)` maps to global logical `(rect.x + px / scale, rect.y + py / scale)`.

#### Scenario: Pixel to global mapping at scale 2
- **WHEN** a frame has `rect = (100, 50, 400, 300)` and `scale = 2.0`
- **THEN** frame pixel `(200, 100)` maps to global `(200, 100)` and frame size is `800×600`

### Requirement: Toplevel export capture
`ToplevelExportCapture` SHALL capture a single toplevel via `hyprland_toplevel_export_manager_v1` `capture_toplevel_with_wlr_toplevel_handle` or by Hyprland window address, with cursor excluded, into a `wl_shm` buffer, and SHALL report the frame's `rect` as the target's global rect and `scale` as `frame_width / rect.width`.

#### Scenario: Focused toplevel captured
- **WHEN** `capture` is called for a `Target::Toplevel` that is mapped
- **THEN** the returned frame contains only that window's content, no other windows, no cursor, and no woints overlay

#### Scenario: Frame size versus logical size
- **WHEN** the target is `1200×800` logical on an output at scale 2
- **THEN** the frame is `2400×1600` and `scale = 2.0`; if Hyprland includes decorations or shadow the implementation records the measured offset and marks it `TODO(verify)` in the log

#### Scenario: Export fails
- **WHEN** the compositor replies `failed` or the toplevel is unmapped
- **THEN** `capture` returns `Err(CaptureError::Unavailable)` and the caller falls back to region capture

### Requirement: Screencopy region capture
`ScreencopyRegionCapture` SHALL capture a global logical rect using `zwlr_screencopy_manager_v1.capture_output_region` on the output that holds the rect's centre, with `overlay_cursor = 0`, converting the rect to output-local logical coordinates with `core` transforms.

#### Scenario: Layer surface capture
- **WHEN** `capture` is called for a `Target::Layer` whose rect is `(0, 0, 1440, 80)` on an output at origin `(0, 0)` scale 2
- **THEN** the frame is `2880×160` with `rect = (0, 0, 1440, 80)` and `scale = 2.0`

#### Scenario: Output with negative origin
- **WHEN** the rect is `(-1800, 100, 400, 200)` on an output at origin `(-1920, 0)` scale 1
- **THEN** the region passed to screencopy is `(120, 100, 400, 200)` in output-local coordinates

### Requirement: Overlay excluded from frames
Capture SHALL run before woints maps its overlay surface for the current session, or while it is unmapped, so frames never contain woints badges.

#### Scenario: Re-run with overlay showing
- **WHEN** `hints` arrives while the overlay is already mapped
- **THEN** the overlay is unmapped and a frame callback is awaited before the new capture starts

### Requirement: Warm capture objects
Capture managers, output bindings and `wl_shm` pools SHALL be created once at daemon start and reused; pools SHALL grow but not shrink between requests.

#### Scenario: Second capture is cheaper
- **WHEN** two consecutive captures of the same target run
- **THEN** the second allocates no new shm pool and its `capture` stage is not slower than the first
