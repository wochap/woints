# quickshell-qml-module Specification

## Purpose
Woints QML module that exposes hintable quickshell items over IPC.

## Requirements

### Requirement: Module layout
The repository SHALL ship `integrations/quickshell/` containing `qmldir` (module `Woints`), `SHints.qml` (singleton), `Hintable.qml`, and `README.md`, and the flake SHALL export it as `packages.quickshell-module` installing to `share/quickshell/Woints`.

#### Scenario: Import
- **WHEN** a shell adds the module directory to its import path and writes `import Woints`
- **THEN** `SHints` and `Hintable` resolve without errors

### Requirement: SHints registry and IPC target
`SHints` SHALL keep a registry of hintable items, expose `register(item, opts)`, `unregister(item)`, `list()` and `invoke(id, action)`, and declare `IpcHandler { target: "woints" }` with functions `list(): string` and `invoke(id: string, action: string): string`.

#### Scenario: list output
- **WHEN** `qs ipc call woints list` is invoked
- **THEN** it returns JSON `{"protocol":1,"items":[...]}` where each item has `id, ns, screen, x, y, w, h, label, actions`

#### Scenario: invoke result
- **WHEN** `invoke("abc","click")` targets a registered item that handles `click`
- **THEN** the item's `activated("click")` signal fires and the function returns `"ok"`; for unknown ids it returns `"unknown"`, for unsupported actions `"unhandled"`

### Requirement: Hintable component
`Hintable` SHALL be a drop-in `Item`-less component with properties `target` (default `parent`), `label`, `actions` (default `["click"]`), `enabled`, and signal `activated(string action)`. It SHALL register on `Component.onCompleted` and unregister on destruction, SHALL only be listed when `target.visible && target.Window.window.visible && target.opacity > 0` and when its rect intersects the window, SHALL report `x, y` via `target.mapToItem(null, 0, 0)` in logical px, `ns` from the window's `WlrLayershell.namespace` and `screen` from `window.screen.name`.

#### Scenario: Hidden item
- **WHEN** a `Hintable`'s target is inside a collapsed panel with `visible: false`
- **THEN** it is absent from `list()`

#### Scenario: Clipped item
- **WHEN** a target's mapped rect lies fully outside its window
- **THEN** it is absent from `list()`

### Requirement: Documentation
`integrations/quickshell/README.md` SHALL document installation (import path and Nix output), a protocol version table, and two usage examples: adding `Hintable` inside a reusable button component, and wrapping a raw `MouseArea`.

#### Scenario: Example compiles
- **WHEN** the README's examples are copied into a minimal quickshell config
- **THEN** `qs` loads them without warnings (TODO(verify) on a live session)
