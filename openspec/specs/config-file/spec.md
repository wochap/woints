# config-file Specification

## Purpose
TOML configuration file location, defaults, validation and reload.

## Requirements

### Requirement: Location and defaults
`wointsd` SHALL read `$XDG_CONFIG_HOME/woints/config.toml` (default `~/.config/woints/config.toml`) or the path from `--config`. Missing file means built-in defaults, which SHALL equal the design §11 TOML (`[hint]`, `[timing]`, `[chip]`, `[theme.mocha]`, `[theme.latte]`) plus `[keys] mode="auto" alphabet="asdfghjkl" exclude=[]`, `[scope] default="focused" ignore_layers=["awww-daemon","wayland-pipewire-idle-inhibit"]`, `[vision] mode="auto" min_atspi_elements=3 force_classes=[]`, `[merge] iou_threshold=0.5`, `[input] pointer="movecursor" prefer="action" restore_cursor=true modifiers={shift="right",ctrl="middle",alt="hover"}`, `[providers.quickshell] mode="auto"`, `[apps] disabled_classes=[] force_vision_classes=[]`, `[timing] max_showing_ms=15000`.

#### Scenario: No file
- **WHEN** no config file exists
- **THEN** `status.config.path` is null, `status.config.source` is `"defaults"` and the daemon runs

#### Scenario: Partial file
- **WHEN** the file sets only `[hint] theme = "latte"`
- **THEN** every other value equals the default

### Requirement: Validation
Config loading SHALL fail with a message naming the key when: alphabet has fewer than 4 unique glyphs, `iou_threshold` ∉ (0,1], `min_atspi_elements` < 0, colours are not `#rrggbb`/`#rrggbbaa`, a modifier maps to an unknown action, or an unknown key is present (unknown keys are warnings, not errors).

#### Scenario: Bad colour
- **WHEN** `theme.mocha.bg = "red"`
- **THEN** loading fails with `theme.mocha.bg: expected #rrggbb or #rrggbbaa`

### Requirement: Reload
The daemon SHALL reload on `SIGHUP` and on the `reload` protocol command. On success the new config applies to the next request and the overlay re-renders if showing; on failure the previous config stays active and the error is stored in `status.config.errors` and printed by `doctor`.

#### Scenario: Invalid reload
- **WHEN** the file is edited to an invalid value and `woints reload` runs
- **THEN** the reply is `{"ok":false,"error":"<key>: <message>"}` and the previous config remains in effect

### Requirement: ONNX config table
`config.toml` SHALL accept:
```toml
[vision]
detector = "classical"          # "classical" | "onnx" | "onnx+classical"

[vision.onnx]
model_path       = "~/.local/share/woints/models/ui.onnx"
ep               = "cpu"        # "cpu" | "cuda" | "migraphx"
threads          = 0            # 0 = auto
min_confidence   = 0.35
nms_iou          = 0.5
input_size       = [640, 640]   # override metadata
class_names      = []           # override metadata
interactive_classes = []        # empty = all classes
normalization    = { scale = 0.00392156, mean = [0,0,0], std = [1,1,1] }
output_format    = "auto"       # "auto" | "yolo" | "detr"
```
Unknown keys SHALL warn; `~` SHALL expand; the table SHALL reload on SIGHUP.

#### Scenario: SIGHUP changes threshold only
- **WHEN** `min_confidence` changes and SIGHUP is sent
- **THEN** the new threshold applies to the next request without rebuilding the session

#### Scenario: SIGHUP changes model path
- **WHEN** `model_path` changes and SIGHUP is sent
- **THEN** the session is rebuilt and warm-up runs again, logged as `onnx: reloaded`

### Requirement: A11y config table
`config.toml` SHALL accept:
```toml
[a11y]
wake_chromium = true   # wake dormant Chromium/Electron accessibility trees
wake_exclude  = []     # Hyprland classes never woken (glob, case-insensitive)
```
Missing keys SHALL take these defaults. The table SHALL reload on SIGHUP and `reload`, applying to the next request; applications already woken stay woken. Unknown keys in `[a11y]` SHALL warn.

#### Scenario: Defaults
- **WHEN** the config file has no `[a11y]` table
- **THEN** `wake_chromium` is true and `wake_exclude` is empty

#### Scenario: Wake disabled
- **WHEN** the file sets `[a11y] wake_chromium = false`
- **THEN** no dormant Chromium frame is woken and those windows get vision as before

#### Scenario: Reload adds an exclusion
- **WHEN** `wake_exclude = ["brave-browser"]` is added and `woints reload` runs
- **THEN** the next request on a dormant `brave-browser` window makes no wake call
