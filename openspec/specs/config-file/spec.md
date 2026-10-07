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
