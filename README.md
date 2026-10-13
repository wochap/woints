# woints

Vimium-style keyboard hints for Hyprland. Press a key binding and letter
hints appear over interactive elements; type a hint to act on it.

`woints hints` badges the interactive elements of the focused window (or
bars, popups and every visible window, see [Scopes](#scopes)); typing filters
the badges, Backspace undoes, a full label flashes and performs the session's
action (click by default). Elements come from AT-SPI (GTK3, GTK4, Qt Widgets,
Firefox chrome), from your quickshell bar through the
[Woints QML module](integrations/quickshell/README.md), and from a classical
vision fallback when neither has enough. `woints doctor` explains what works
on your machine. Toolkit findings are in [`docs/toolkits.md`](docs/toolkits.md).

## Architecture

```
                     ┌──────────────── wointsd ────────────────┐
  woints CLI ──────► │ Session FSM: Idle ─hints─► Showing      │
  JSON lines         │   Showing ─drag─► ShowingSecond ─► Idle │
                     │   Showing ─scroll─► Scrolling ─Esc─► Idle│
                     │            ▲  cancel / Esc / quit ◄──┘  │
  $XDG_RUNTIME_DIR/  └──┬──────────┬───────────┬──────────┬────┘
  woints.sock    TargetSelector  ElementProvider  Overlay  Actuator   (ports, crates/core)
                      │              │              │         │
                    hypr        a11y / vision /   overlay   input     (adapters)
                                quickshell-ipc
```

| crate | binary | role |
|---|---|---|
| `woints-core` | | typed coordinate spaces, domain types, port traits, labels, placement, theme tokens, protocol |
| `woints-hypr` | | Hyprland IPC over `.socket.sock` / `.socket2.sock` (never forks `hyprctl`) |
| `woints-overlay` | | `zwlr_layer_shell_v1` overlay thread: sctk + calloop, `wp_fractional_scale_v1` + `wp_viewporter`, tiny-skia |
| `woints-daemon` | `wointsd` | tokio daemon: socket server, session FSM, pipeline, stage timing |
| `woints-cli` | `woints` | thin client |
| `woints-a11y` | | AT-SPI provider and actuator: one zbus connection to the a11y bus, pid/fuzzy pairing, `Collection.GetMatches` or a bounded pipelined walk per toolkit profile, `WINDOW` extents, phantom filters |
| `woints-input` | | pointer actuator: Hyprland `movecursor` + `zwlr_virtual_pointer_v1` buttons, motion and axis events (or output-local `motion_absolute`) |
| `woints-vision` | | vision fallback: toplevel export / screencopy capture, classical edge detector |
| `woints-qs` | | quickshell provider: `qs list` discovery, `qs ipc call woints list/invoke` |

Pipeline stages (each timed, logged at `debug` as `stage=<name> ms=<f32>`):
`hypr.query → select → collect.connect → collect.match → collect.extents →
collect.filter` (AT-SPI, toplevels) in parallel with `qs.list` (quickshell,
layers), then `capture → detect → merge` when vision runs, `label → place →
render → commit`, plus `act` for the last action.

## Build

```sh
nix build            # result/bin/woints, result/bin/wointsd
nix develop          # rust toolchain + pkg-config, wayland, libxkbcommon
just build | test | lint | run-daemon | dump
```

## Hyprland setup

```ini
exec-once = wointsd

# Scopes
bind = SUPER, F, exec, woints hints                          # scope.default (focused)
bind = SUPER ALT, F, exec, woints hints --scope focused+layers
bind = SUPER CTRL, F, exec, woints hints --scope layers       # bars, popups only
bind = SUPER SHIFT CTRL, F, exec, woints hints --scope all    # every visible window + layers

# Actions
bind = SUPER SHIFT, F, exec, woints hints --action right
bind = SUPER, M, exec, woints hints --action middle
bind = SUPER, G, exec, woints hints --action double
bind = SUPER, V, exec, woints hints --action hover
bind = SUPER, I, exec, woints hints --action focus
bind = SUPER, D, exec, woints hints --action drag
bind = SUPER, S, exec, woints hints --action scroll
bind = SUPER, Y, exec, woints hints --action copy
```

`exec-once` processes inherit Hyprland's environment, so the accessibility
variables `woints doctor` checks (`NO_AT_BRIDGE`, `QT_LINUX_ACCESSIBILITY_ALWAYS_ON`,
`ACCESSIBILITY_ENABLED`, ...) must be set where Hyprland starts (`env = ...` in
`hyprland.conf` or your session wrapper), not in your shell rc. Apps need them
too, so set them session-wide. A systemd user service sees the systemd user
environment instead; `doctor` prints the daemon's pid and reads its env.

Verified against Hyprland **0.56.2**.

## NixOS / home-manager

```nix
# flake.nix inputs
inputs.woints.url = "github:wochap/woints";

# home-manager module
{ inputs, pkgs, ... }:
let
  woints = inputs.woints.packages.${pkgs.system};
in {
  home.packages = [ woints.default ];

  systemd.user.services.wointsd = {
    Unit = {
      Description = "woints hint daemon";
      PartOf = [ "graphical-session.target" ];
      After = [ "graphical-session.target" ];
    };
    Service = {
      ExecStart = "${woints.default}/bin/wointsd";
      Restart = "on-failure";
    };
    Install.WantedBy = [ "graphical-session.target" ];
  };

  # Optional: the quickshell module next to your shell.qml (import as `qs.Woints`;
  # link the QML files only, quickshell must synthesise the qmldir itself)
  xdg.configFile."quickshell/shell/Woints/SHints.qml".source =
    "${woints.quickshell-module}/share/quickshell/Woints/SHints.qml";
  xdg.configFile."quickshell/shell/Woints/Hintable.qml".source =
    "${woints.quickshell-module}/share/quickshell/Woints/Hintable.qml";

  # Optional: config (defaults shown in config.example.toml)
  xdg.configFile."woints/config.toml".text = ''
    [scope]
    default = "focused+layers"
  '';

  home.sessionVariables = {
    QT_LINUX_ACCESSIBILITY_ALWAYS_ON = "1";
    ACCESSIBILITY_ENABLED = "1";
  };
}
```

With the systemd service, run `systemctl --user import-environment
WAYLAND_DISPLAY HYPRLAND_INSTANCE_SIGNATURE XDG_RUNTIME_DIR` (Hyprland's
`dbus-update-activation-environment --systemd --all` does this). On other
distros, build with `cargo build --release` and use `exec-once = wointsd`.

## Scopes

| scope | targets |
|---|---|
| `focused` (default) | the focused window |
| `focused+layers` | the focused window plus layer surfaces (bars, popups, OSDs) on its output |
| `layers` | layer surfaces on every output, no windows |
| `all` | every mapped, non-hidden window on each monitor's active workspace (and a shown special workspace), plus layers on every output |

Layer surfaces come from Hyprland `layers`: levels in `scope.levels`
(default `bottom`, `top`, `overlay`; bars often live on `bottom`, wallpapers
on `background` are excluded), minus `scope.ignore_layers` and 1×1
placeholders. All targets share one label space; window elements get the
shorter labels, layer elements the longer ones. In `all`, floating windows are
assumed to be above tiled ones: elements of a tiled window covered by a
floating window are dropped. Other overlaps between windows are not resolved
(Hyprland does not expose z-order).

Layer elements come from the quickshell module when the shell imports it,
else from vision. To skip a quickshell instance entirely (for example a lock
screen), list a glob of its config path in `providers.quickshell.ignore_configs`
(`["*/lock.qml"]`): it is never probed and its layers get no hints, not even
from vision. `woints doctor` marks ignored instances.

## CLI

```
woints hints [--scope <scope>] [--action <action>]
woints cancel | status | quit
woints reload                       # re-read config.toml (same as SIGHUP)
woints doctor [--json]              # exit 1 when a check fails
woints snapshot [--scope <scope>] [--json]
woints act --snapshot N --id E [--action <action>] [--to E2] [--axis v|h --amount N]
woints bench [-n 20] [--scope <scope>]
wointsd [--config PATH]
```

`--scope` defaults to `scope.default` from the config. Exit codes: `0` ok,
`1` daemon replied `ok:false` (or `doctor` found a failure), `2` daemon not
reachable.

### Shell completions

zsh completions live in [`integrations/zsh/_woints`](integrations/zsh/_woints):
subcommands, flags and the `--scope`, `--action` and `--axis` values. The Nix
package installs them to `share/zsh/site-functions`, so a `nix profile` or
home-manager install with `programs.zsh.enable` picks them up automatically.
Elsewhere, copy the file into a directory on `fpath` before `compinit`:

```zsh
fpath=(~/.zsh/completions $fpath)   # contains _woints
autoload -U compinit && compinit
```

## Configuration

`$XDG_CONFIG_HOME/woints/config.toml` (`~/.config/woints/config.toml`), or
`wointsd --config PATH`. Every key is optional; [`config.example.toml`](config.example.toml)
lists all defaults. Sections: `[hint]`, `[timing]`, `[chip]`, `[theme.dark]`,
`[theme.light]`, `[keys]`, `[scope]`, `[vision]`, `[merge]`, `[input]`,
`[actions]`, `[providers.quickshell]`, `[apps]`, `[a11y]`.

- `[a11y]`: `wake_chromium = true` wakes dormant Chromium/Electron
  accessibility trees on the first request; `wake_exclude` lists class globs
  (case-insensitive) never woken, which get vision instead. Set
  `wake_chromium = false` to turn the wake off.

- `hint.theme = "auto"` (default) picks `[theme.dark]` or `[theme.light]` from
  the system colour scheme (XDG settings portal `org.freedesktop.appearance`
  `color-scheme`, followed live; dark when the portal is unavailable);
  `"dark"` / `"light"` force one. Each table accepts any subset of tokens and
  keeps the default for the rest. The mode chip has its own `chip_bg`,
  `chip_fg`, `chip_border` and `chip_accent` tokens.
- Reload with `woints reload` or `pkill -HUP wointsd`. A file that fails
  validation is rejected with the key named (`theme.dark.bg: expected
  #rrggbb or #rrggbbaa`) and the previous config stays active; `woints status`
  and `doctor` show the error. Unknown keys only warn.
- A reload that changes the label alphabet cancels a session on screen; a
  colour or layout change redraws it.
- `keys.mode = "auto"` displays the glyphs your active layout puts on the
  home-row keys (Colemak-DH shows `arstneiogm`), read from Hyprland `devices`
  and recompiled on layout switches; `"explicit"` uses `keys.alphabet`.
  Punctuation is dropped. Keys always match by physical key.

## Key capture

By default (`input.keys = "auto"`) hint keys come from a Hyprland submap, not
from a focused overlay surface, so the focused window keeps keyboard focus and
popups (a `<select>` list, a GTK menu) stay open while hints show. The submap
is defined by a Lua module shipped with woints
(`integrations/hyprland/woints.lua`, or the flake's `hyprland-module` output).
Load it from `hyprland.lua`:

```lua
require("woints").setup()
```

The module's submap (default `woints-keys`, `setup({ submap = ... })`; it must
match `input.submap`) binds only a no-op catchall, so every key is swallowed
while it is active. A key listener reports each press and release to the
daemon, which turns them into keysyms and modifiers with an xkb state of your
keyboard layout. A key that is not a label glyph, Backspace, Tab, a scroll key,
Escape or a modifier ends the session. A session enters the submap when hints
show and leaves it when they go. The daemon never defines binds or submaps.

**Breaking change:** the daemon no longer defines the submap itself, and
`wointsd --print-submap` is gone. Add the `require` line above. Until you do,
`"auto"` uses the overlay source (`woints doctor` prints the line to add), and
legacy (non-Lua) Hyprland builds always do.

`input.keys = "overlay"` makes the overlay take exclusive keyboard focus
instead, which closes popups. `"submap"` fails `hints` when the module isn't
loaded. `woints status` / `woints doctor` show the key source and the reason.

Stuck-state guarantees, each independent of the others:

- Escape resets the submap inside Hyprland, without the daemon.
- A Hyprland-side timer resets it after `timeout_ms` (default 17 s) without a
  key press, even if the daemon is dead.
- The daemon leaves the submap on every way out of a session: selection,
  Escape, an unbound key, cancel, watchdog, quit, overlay loss, a click on
  another window (active window change), a lost event socket and a Hyprland
  config reload.
- `wointsd` resets a leftover submap when it starts.

If Hyprland is ever left in the submap, run `hyprctl dispatch submap reset`;
`woints doctor` flags this state.

## Actions

| action | what happens | actuator |
|---|---|---|
| `click` (default) | left click at the element centre | AT-SPI `DoAction` (`GrabFocus` on entries) when available, else pointer |
| `right` | right click (button 273) | pointer |
| `middle` | middle click (button 274) | pointer |
| `double` | two left clicks, `input.double_click_gap_ms` (40) apart | pointer |
| `hover` | move the cursor only, never restored | pointer |
| `focus` | `GrabFocus`; a pointer click when it returns `false` | AT-SPI, pointer fallback |
| `drag` | pick a source (`drag 1/2`), then a destination (`drag 2/2`, fresh labels, source pinned); press, `input.drag_steps` (8) relative motions, `movecursor` to the destination, release. Esc in phase 2 returns to phase 1 | pointer |
| `scroll` | pick a scroll container (skipped when there is one); `h j k l` / arrows scroll `input.scroll_step` (3) lines of 15 px; Tab re-picks, Esc exits | pointer axis events |
| `copy` | element text (`Text.GetText`, else its name) to the clipboard as `text/plain;charset=utf-8` | AT-SPI + data control |

Losing keyboard focus (e.g. a launcher opening) ends the session in every mode, drag phase 2 included.

Modifiers held on the **final** glyph pick the action: Shift → `right`,
Ctrl → `middle`, Alt → `hover` (`input.modifiers.shift/ctrl/alt/super`, each
an action or `"none"`). An explicit `--action` wins over modifiers. Glyphs match
by keycode, so `Shift+d` still matches `d`.

Pointer actions move the cursor with Hyprland `dispatch movecursor x y`
(global logical coordinates) and send buttons, relative motion and axis
events through one long-lived `zwlr_virtual_pointer_v1` on the overlay's
Wayland connection, each followed by `frame` and a roundtrip. They run only
after the overlay's unmap has been confirmed. Absolute virtual-pointer motion
is not used by default (hyprwm/Hyprland#6749); `input.pointer = "absolute"`
selects output-local `motion_absolute` instead. `input.restore_cursor = true`
moves the cursor back afterwards (`hover` excepted; on by default).
`input.prefer = "pointer"` skips AT-SPI for `click`/`focus`. ydotool, uinput and the
RemoteDesktop portal are deliberately not used: they need root-adjacent
setup or do not exist on Hyprland.

`copy` needs `zwlr_data_control_manager_v1` (or `ext_data_control_manager_v1`);
without it, `copy` fails with `clipboard unavailable: ...` and never falls
back to `wl-copy`. `woints status` shows `pointer` and `clipboard`
availability.

These are `[input]` keys (`input.modifiers = { shift = "right", ... }`) and
`actions.default`.

## Performance

Target: total p50 under 100 ms from `hints` to badges. TODO(verify): record
`woints bench -n 20` with `gtk3-widget-factory` and `qt6ct` focused.
Measured by the provider's live test against Firefox (Gecko, Collection with
web-content pruning, 18 elements): `collect.connect` 0.01 ms (reused
connection), `collect.match` 44 ms, `collect.extents` 14 ms,
`collect.filter` 0.1 ms.

Vision path, `[vision] mode = "always"`, `woints bench -n 20 --scope focused`,
release build, 7840HS laptop, foot focused at 1438×858 on a 2880×1800 scale 2
output (150 elements, capped by `vision.max_elements`):

| stage   | p50 ms | p95 ms |
|---------|-------:|-------:|
| capture |  16.8  |  22.1  |
| detect  |  26.2  |  28.1  |
| merge   |   0.01 |   0.02 |
| total   |  54.0  |  64.3  |

Target total p95 < 300 ms. TODO(verify): the same run on the 5900X desktop.

`woints bench -n 20 --scope focused+layers`, release build, same laptop,
2026-10-06: footclient focused (no AT-SPI, so vision runs) plus the
quickshell bar and notification layers without the Woints module (vision
again, 3 targets, 190 elements):

| stage | p50 ms | p95 ms |
|---|---:|---:|
| hypr.query | 0.41 | 0.59 |
| collect.match | 0.31 | 0.50 |
| capture | 56.5 | 111.5 |
| detect | 45.2 | 92.2 |
| place | 0.51 | 1.12 |
| render | 15.1 | 26.3 |
| total | 121.1 | 221.3 |

Vision on three surfaces dominates. TODO(verify): the same run with the
Woints module imported, where `qs.list` replaces vision on the bar (design
budget 40 ms; a native IPC client is the follow-up if p95 exceeds ~20 ms).

## ONNX detector

Optional learned detector behind the classical one. Off by default; the
default build carries no ONNX Runtime code.

1. Build with the feature: `nix build .#woints-onnx` (wraps the binaries
   with `ORT_DYLIB_PATH` pointing at nixpkgs `onnxruntime`), or
   `cargo build --features woints-vision/onnx` and set `ORT_DYLIB_PATH`
   yourself.
2. Put a detector `.onnx` somewhere, ideally with its `LICENSE` next to it.
3. Configure and `woints reload` (or SIGHUP):

```toml
[vision]
detector = "onnx+classical"     # "classical" | "onnx" | "onnx+classical"

[vision.onnx]
model_path     = "~/.local/share/woints/models/ui.onnx"
ep             = "cpu"          # "cpu" | "cuda" | "migraphx"
threads        = 0              # 0 = physical cores / 2
min_confidence = 0.35
nms_iou        = 0.5
# Overrides for models without metadata (Ultralytics `names`/`imgsz`, or
# `woints.class_names`, `woints.input_size`, `woints.normalization`,
# `woints.output_format`):
# input_size          = [640, 640]
# class_names         = ["button", "icon", "link"]
# interactive_classes = []      # empty = all
# normalization       = { scale = 0.00392156, mean = [0,0,0], std = [1,1,1] }
# output_format       = "auto"  # "auto" | "yolo" | "detr"
```

`woints doctor` shows the `onnx` section (model, input shape, outputs, EP,
warm-up, license line); `woints status` shows `detector` and `onnx`. A
missing or broken model disables ONNX and keeps the classical detector.
Threshold changes apply on reload without a new session; `model_path`,
`ep` and `threads` changes rebuild it (`onnx: reloaded`).

**No model is bundled or recommended.** No permissively licensed
general desktop-GUI detector with clean provenance was found (checked
2026-10-06). You may run any of these locally; woints never redistributes
them:

| model | license | source | verdict |
|---|---|---|---|
| Ultralytics YOLOv8/11 (code, official weights) | AGPL-3.0 | https://github.com/ultralytics/ultralytics | usable locally by the user, not recommended, not redistributed |
| microsoft/OmniParser-v2.0 `icon_detect` | AGPL-3.0 | https://huggingface.co/microsoft/OmniParser-v2.0 | usable locally by the user, not recommended, not redistributed |
| microsoft/OmniParser-v2.0 `icon_caption` (Florence-2) | MIT | https://huggingface.co/microsoft/OmniParser-v2.0 | not a detector (captions, no boxes) |
| Salesforce/GPA-GUI-Detector | states MIT; derived from AGPL weights | https://huggingface.co/Salesforce/GPA-GUI-Detector | treated as AGPL until clarified; `.pt` only; not recommended |
| OpenDILabCommunity/webpage_element_detection | Apache-2.0 card; YOLOv8 (AGPL) derivation | https://huggingface.co/OpenDILabCommunity/webpage_element_detection | web pages only; provenance unclear; not recommended |
| Roboflow RF-DETR (base) | Apache-2.0 | https://github.com/roboflow/rf-detr | permissive, but no GUI finetune exists; candidate for a future own finetune |

Only models whose weights license is compatible with MIT will ever
be recommended here.

Latency, `woints bench -n 20` with the ONNX detector at 640×640 on CPU:
TODO(verify) on the 7840HS + RTX 4060 laptop and the 5900X + RX 6800 XT
desktop (`detect.onnx` p50/p95).

## Toolkit enablement

| toolkit | what to do | status |
|---|---|---|
| GTK3 | unset `NO_AT_BRIDGE` (it disables the bridge); `org.a11y.Status.IsEnabled` / gsettings `toolkit-accessibility = true` helps apps that check it | verified 2026-10-06, GTK 3.24 (absent with `NO_AT_BRIDGE=1`, registers without) |
| GTK4 | works out of the box; no Collection, tree walk only; `WINDOW` coordinates | verified 2026-10-06, GTK 4.22 |
| Qt Widgets | `QT_LINUX_ACCESSIBILITY_ALWAYS_ON=1` | verified 2026-10-06, Qt 6.11 |
| Qt QML / quickshell | not reachable over AT-SPI (windows never attach to the app root); import the [Woints module](integrations/quickshell/README.md), else vision | verified negative 2026-10-06, quickshell 0.3.1 / Qt 6.11; module TODO(verify) |
| Electron | `ACCESSIBILITY_ENABLED=1` to register; woints wakes the dormant tree automatically on the first request (up to 1 s once per app); opt out per class with `[a11y] wake_exclude` | verified 2026-10-10, Electron 41.9.1 (Bruno) |
| Chromium browsers | tree woken automatically; browser UI only (toolbar, tabs, omnibox, bookmarks), web content skipped (use Vimium); about +20 MB memory per woken browser; opt out with `[a11y] wake_exclude` | verified 2026-10-10, Chrome 150 |
| Firefox | browser chrome via Collection, web content skipped (use Vimium); page content never SHOWING | verified 2026-10-06, Gecko 152 (chrome); SHOWING issue TODO(verify) |
| Flutter | not tested | TODO(verify) |
| Tauri (WebKitGTK) | not tested | TODO(verify) |

Anything without usable AT-SPI elements gets vision hints (`vision.mode`).

## Troubleshooting

Manual M5 checks (quickshell module, layer badges) and decisions awaiting review: [`docs/verify-m5.md`](docs/verify-m5.md).

Start with `woints doctor`; each line maps to a fix:

| doctor line | fix |
|---|---|
| `fail NO_AT_BRIDGE=1 disables GTK3 accessibility` | remove `NO_AT_BRIDGE` from the session environment (Hyprland `env`, display manager, home-manager `sessionVariables`) and restart the apps |
| `fail a11y bus unreachable` | `at-spi2-core` not running: on NixOS `services.gnome.at-spi2-core.enable = true` |
| `warn org.a11y.Status.IsEnabled = false` | run the printed `busctl` command, or set gsettings `toolkit-accessibility = true` |
| `warn no a11y app matched` | the app does not speak AT-SPI or lacks its env var (table above); vision covers it |
| `ok Collection absent (expected for GTK4)` | nothing to do |
| `warn Qt QML window: not reachable from AT-SPI root` | quickshell or another QML app: import the Woints module, or rely on vision |
| `fail zwlr_layer_shell_v1 missing` / `zwlr_virtual_pointer_manager_v1 missing` | not Hyprland, or a very old one; woints needs both |
| `warn ... Woints module not imported` | add the module to that shell (ignore for a lock-screen instance) |
| `warn qs unavailable` | `qs` not on the daemon's `PATH`; quickshell layers use vision |
| `fail config error: <key>: ...` | fix the key, then `woints reload` |
| `warn config: <key>: unknown key` | typo or a key from another version |
| `ok onnx: not compiled in (...)` | build `.#woints-onnx` to use `vision.detector = "onnx"` |
| `fail model: class_names missing (set vision.onnx.class_names)` | the model lacks metadata; set the named key |
| `warn license: not found (warning)` | put the model's `LICENSE` (or `<model>.license`) next to it |

## Debugging

- `RUST_LOG=debug wointsd` logs every stage duration.
- `WOINTS_DUMP=/tmp/dump wointsd` writes one PNG per output for every rendered frame.
- `WOINTS_DEMO=1 wointsd` uses the demo boxes instead of AT-SPI.
- `woints status` shows provider availability, pairing reason (`pid`,
  `fuzzy`, `no-a11y-app`, `frame-guess`), strategy used, `truncated` and the
  last action.
- `WOINTS_LIVE_TESTS=1 cargo test` enables tests that talk to the live
  Hyprland session; the overlay one maps a surface (and grabs the keyboard)
  for 500 ms. The a11y ones are read-only and skip apps that are not running;
  `WOINTS_LIVE_CLASS=<class>` collects any open window.

## Font licence

Badges use **Atkinson Hyperlegible Mono Bold**, © 2020–2024 The Atkinson
Hyperlegible Mono Project Authors, embedded from
`crates/overlay/assets/AtkinsonHyperlegibleMono-Bold.ttf` under the SIL Open
Font License 1.1 (`crates/overlay/assets/OFL.txt`).

## License

MIT (`LICENSE`). See `CONTRIBUTING.md` for the prior-art rule.
