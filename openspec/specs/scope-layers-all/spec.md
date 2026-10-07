# scope-layers-all Specification

## Purpose
Layer-shell and multi-window target resolution for the layers, focused+layers and all scopes.

## Requirements

### Requirement: Layer targets
When the scope includes layers, `TargetSelector` SHALL query Hyprland `layers` and emit one `Target::Layer { address, namespace }` per surface on the `top` and `overlay` levels of the relevant monitors, with its global rect, pid and output, skipping surfaces whose namespace is in `scope.ignore_layers` and surfaces of size 1×1 or smaller.

#### Scenario: Bar and popup included
- **WHEN** `layers` reports `quickshell:bar` (0,0,1440,80) and `quickshell:notifications-popups` (0,40,1440,860) on level `top`, plus `awww-daemon` on `background`
- **THEN** two layer targets are produced and `awww-daemon` is absent

#### Scenario: Ignore list and placeholders
- **WHEN** `layers` reports `wayland-pipewire-idle-inhibit` with size 1×1 and `quickshell:bar`
- **THEN** only `quickshell:bar` becomes a target

### Requirement: Scope semantics
`focused+layers` SHALL return the focused toplevel plus layers on the focused toplevel's output; `layers` SHALL return layers on all outputs and no toplevels; `all` SHALL return every mapped, non-hidden client on each monitor's active workspace (and on a visible special workspace) plus layers on all outputs.

#### Scenario: layers only
- **WHEN** `select(Scope::Layers)` runs with a focused window present
- **THEN** no `Target::Toplevel` is returned

#### Scenario: all windows across two monitors
- **WHEN** monitor A shows workspace 1 with two tiled clients and monitor B shows workspace 3 with one floating client, and workspace 2 is not visible
- **THEN** three toplevel targets are returned, none from workspace 2

### Requirement: Stacking assumption
In `all` scope, floating toplevels SHALL be treated as above tiled ones for element culling: elements of a tiled window whose rect is covered by a floating window on the same output are dropped. Other overlaps are not resolved (TODO occlusion).

#### Scenario: Floating over tiled
- **WHEN** a tiled window has an element at (100,100,40,20) and a floating window covers (50,50,300,300)
- **THEN** that element is not hinted

### Requirement: One label space
All targets of a request SHALL share one label assignment; toplevel targets SHALL be prioritised before layer targets so layer elements receive the longer labels.

#### Scenario: Window plus bar
- **WHEN** a focused window has 6 elements and the bar has 10
- **THEN** the 6 window elements receive the single-glyph labels and the bar elements receive two-glyph labels
