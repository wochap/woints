# doctor Specification

## Purpose
Diagnostic report of environment, accessibility, Wayland and quickshell readiness.

## Requirements

### Requirement: Checks
`woints doctor` SHALL ask the daemon for a `doctor` report and print, grouped: (1) environment: `NO_AT_BRIDGE`, `GTK_A11Y`, `QT_LINUX_ACCESSIBILITY_ALWAYS_ON`, `ACCESSIBILITY_ENABLED`, `GNOME_ACCESSIBILITY`; (2) a11y bus: reachable, address, `org.a11y.Status.IsEnabled`, gsettings `toolkit-accessibility`; (3) focused window: class, pid, toolkit name/version from AT-SPI `Application`, whether an a11y app matched, Collection interface present, element count by strategy (collection, walk), elements with usable extents, the reason vision would or would not trigger; (4) Wayland globals: `zwlr_layer_shell_v1`, `zwlr_virtual_pointer_manager_v1`, `zwlr_screencopy_manager_v1`, `hyprland_toplevel_export_manager_v1`, `wp_fractional_scale_manager_v1`, `wp_viewporter`; (5) quickshell: instances, module present per instance, item count; (6) Hyprland version; (7) config path and errors. Each line SHALL carry `ok`, `warn` or `fail`.

#### Scenario: NO_AT_BRIDGE set
- **WHEN** the daemon's environment has `NO_AT_BRIDGE=1`
- **THEN** doctor prints `fail  NO_AT_BRIDGE=1 disables GTK3 accessibility; unset it in your session`

#### Scenario: GTK4 window
- **WHEN** the focused window's toolkit is `GTK 4.x`
- **THEN** doctor prints Collection `absent (expected for GTK4)` and the walk count, with status `ok`

#### Scenario: Qt window without module
- **WHEN** the focused window's toolkit is Qt and the a11y app root has zero children
- **THEN** doctor prints `warn  Qt QML window: not reachable from AT-SPI root; use the Woints quickshell module or vision`

### Requirement: Exit code and JSON
`doctor` SHALL exit 0 when no check is `fail`, 1 otherwise, and SHALL support `--json` printing the raw report.

#### Scenario: Healthy system
- **WHEN** every check passes or warns
- **THEN** exit code is 0

### Requirement: No hot-path forks
`doctor` MAY run `gsettings` and `qs` as subprocesses; `hints` and `snapshot` SHALL NOT, except the sanctioned `qs ipc` calls of the quickshell provider.

#### Scenario: Hints path
- **WHEN** `hints` runs with the quickshell provider disabled
- **THEN** no child process is spawned

### Requirement: Documented fixes
README SHALL contain a toolkit enablement table with the fix per toolkit and a column marking each row `verified` (with date and version) or `TODO(verify)`: GTK3 (unset `NO_AT_BRIDGE`; `IsEnabled`/gsettings), GTK4 (works, walk only, WINDOW coords), Qt Widgets (`QT_LINUX_ACCESSIBILITY_ALWAYS_ON=1`), Qt QML/quickshell (Woints module), Electron (`ACCESSIBILITY_ENABLED=1` verified to register with Electron 42; content population TODO(verify); `--force-renderer-accessibility` alternative), Chromium/Firefox (chrome only, web content skipped; Firefox SHOWING issue TODO(verify)), Flutter and Tauri (TODO(verify), not tested).

#### Scenario: Table present
- **WHEN** README is read
- **THEN** every toolkit above has a row with a status column

### Requirement: ONNX doctor section
`woints doctor` SHALL print an `onnx` section: build has feature yes/no; configured detector; model path exists; input shape and output names read from the model; EP requested vs active; warm-up time; license file line; and, when the model lacks metadata, the exact config keys needed.

#### Scenario: Feature absent
- **WHEN** the running `wointsd` was built without `onnx`
- **THEN** doctor prints `onnx: not compiled in (build with --features woints-vision/onnx or nix build .#woints-onnx)` and nothing else in the section

#### Scenario: Model present on CPU
- **WHEN** the model loads on CPU
- **THEN** doctor prints `model: <path> ok`, `input: [1,3,640,640]`, `ep: cpu (requested cpu)`, `warmup: <n> ms`, and the license line or `license: not found (warning)`

### Requirement: WebKitGTK report
For the focused window, `doctor` SHALL print whether the application is detected as WebKitGTK and, when it is, the number of web roots found and the web element count, with status `ok` when at least one web root was found and `warn` otherwise.

#### Scenario: Tauri window
- **WHEN** the focused window is a Tauri app with one web view
- **THEN** doctor prints `ok  WebKitGTK: 1 web root, <n> web elements`

#### Scenario: WebKitGTK without reachable web content
- **WHEN** the app maps WebKitGTK but no web root is found
- **THEN** doctor prints a `warn` line naming WebKitGTK and zero web roots
