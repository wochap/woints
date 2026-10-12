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

### Requirement: Ignored instances
`providers.quickshell.ignore_configs` SHALL be a list of globs (`*` matches any run of characters including `/`, `?` matches one character, case-sensitive) matched against the `config_path` each instance reports in `qs list`. For an instance whose `config_path` matches any glob, the provider SHALL NOT run `qs ipc show`, `list` or `invoke` against it. Every layer target owned by its pid SHALL be removed from the session: no quickshell elements, no vision fallback, no badges. The default list SHALL be empty. A config reload SHALL take effect from the next request: a newly ignored instance is dropped, and a no-longer-ignored instance is probed on its next appearance. The option SHALL have no effect when `providers.quickshell.mode = "off"` or when `qs list` fails; those layers keep their current behaviour.

#### Scenario: Lock instance ignored
- **WHEN** `ignore_configs = ["*/lock.qml"]` and `qs list` reports an instance with `config_path` `/home/u/.config/quickshell/shell/lock.qml`
- **THEN** no `qs ipc` command is run with that instance's id or pid during discovery, `hints` or `snapshot`

#### Scenario: Ignored layer gets no hints
- **WHEN** an ignored instance owns a mapped layer and `woints hints --scope layers` runs
- **THEN** no badge is shown on that layer, vision is not run for it, and layers of other instances are hinted as usual

#### Scenario: Other instance unaffected
- **WHEN** `ignore_configs = ["*/lock.qml"]` and another instance's `config_path` is `/home/u/.config/quickshell/shell/shell.qml`
- **THEN** that instance is probed and listed as before

#### Scenario: Reload
- **WHEN** the daemon has probed the `shell.qml` instance and the config is reloaded with `ignore_configs = ["*/shell.qml"]`
- **THEN** the next `hints` shows no badges on that instance's layers and runs no `qs ipc` command against it

#### Scenario: Doctor
- **WHEN** `woints doctor` runs with an ignored instance present
- **THEN** the quickshell section prints `ok  <id> (pid <pid>, <config_path>): ignored (providers.quickshell.ignore_configs)` without probing it
