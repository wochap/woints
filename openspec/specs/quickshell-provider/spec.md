# quickshell-provider Specification

## Purpose
Element provider for quickshell layer surfaces via the Woints IPC target.

## Requirements

### Requirement: Instance discovery
`QuickshellIpcProvider` SHALL discover running quickshell instances with `qs list --all --json` and, for each, probe once whether an IPC target `woints` exists. Results SHALL be cached per instance pid and re-probed when the pid for a quickshell layer in Hyprland `layers` changes or the instance disappears from `qs list`.

#### Scenario: Module absent
- **WHEN** an instance has no `woints` IPC target
- **THEN** the provider marks it unavailable, emits no elements for its layers and logs at `info` once

#### Scenario: Quickshell restarted
- **WHEN** the pid behind `quickshell:bar` changes
- **THEN** the next request re-runs discovery and probing

### Requirement: Listing and coordinate mapping
For a `Target::Layer` whose pid belongs to an available instance, the provider SHALL call `qs -i <id> ipc call woints list`, parse `{protocol, items[]}`, reject `protocol != 1`, and map each item with `(ns, screen)` equal to the target's namespace and output name into an `Element` with `rect = layer.rect.origin + (x, y, w, h)`, `source = Quickshell`, `actions` from the item.

#### Scenario: Bar button
- **WHEN** the bar layer is at global (0,0) and `list` returns `{id:"ws-1", ns:"quickshell:bar", screen:"eDP-1", x:12, y:24, w:32, h:32, label:"Workspace 1", actions:["click","right"]}`
- **THEN** an element with rect (12,24,32,32), name `Workspace 1`, and actions `click, right` is produced

#### Scenario: Item from another window
- **WHEN** `list` returns an item with `ns = "quickshell:which-keys"` while collecting for `quickshell:bar`
- **THEN** that item is ignored for this target

### Requirement: Activation
Acting on a quickshell element SHALL call `qs -i <id> ipc call woints invoke <item id> <action>`; if the call fails or returns `"unhandled"`, the actuator SHALL fall back to a pointer click at the element centre.

#### Scenario: Semantic invoke
- **WHEN** the user picks a quickshell element with action `click`
- **THEN** `invoke` is called and no pointer movement occurs when it returns `"ok"`

### Requirement: Budget and configuration
The provider SHALL respect the pipeline budget (default 40 ms) by killing an overrunning `qs` process and returning what it has, SHALL record its stage time, and SHALL be disabled entirely when `providers.quickshell.mode = "off"`.

#### Scenario: Disabled
- **WHEN** `providers.quickshell.mode = "off"`
- **THEN** no `qs` process is spawned during `hints` or `snapshot`
