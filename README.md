# woints

Vimium-style keyboard hints for Hyprland. Press a key binding and letter
hints appear over interactive elements; type a hint to act on it.

Milestone 3 (this tree) finds real elements through AT-SPI and acts on them
through AT-SPI or a real pointer: `woints hints` badges the interactive
elements of the focused GTK3, GTK4 or Qt Widgets window; typing filters the
badges, Backspace undoes, a full label flashes and performs the session's
action (click by default). Vision fallback (M4) and quickshell (M4–M5) come
later. Toolkit findings are in [`docs/toolkits.md`](docs/toolkits.md).

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
| `woints-vision` | | placeholder for M4 |

Pipeline stages (each timed, logged at `debug` as `stage=<name> ms=<f32>`):
`hypr.query → select → collect.connect → collect.match → collect.extents →
collect.filter → label → place → render → commit`, plus `act` for the last
action.

## Build

```sh
nix build            # result/bin/woints, result/bin/wointsd
nix develop          # rust toolchain + pkg-config, wayland, libxkbcommon
just build | test | lint | run-daemon | dump
```

## Hyprland setup

```ini
exec-once = wointsd
bind = SUPER, F, exec, woints hints
bind = SUPER SHIFT, F, exec, woints hints --action right
bind = SUPER, D, exec, woints hints --action drag
bind = SUPER, S, exec, woints hints --action scroll
```

Verified against Hyprland **0.56.2**.

## CLI

```
woints hints [--scope focused] [--action <action>]
woints cancel | status | quit
woints snapshot [--scope focused] [--json]
woints act --snapshot N --id E [--action <action>] [--to E2] [--axis v|h --amount N]
woints bench [-n 20]
```

Exit codes: `0` ok, `1` daemon replied `ok:false`, `2` daemon not reachable.

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

Modifiers held on the **final** glyph pick the action: Shift → `right`,
Ctrl → `middle`, Alt → `hover` (`[modifiers] shift/ctrl/alt/super`, each an
action or `"none"`). An explicit `--action` wins over modifiers. Glyphs match
by keycode, so `Shift+d` still matches `d`.

Pointer actions move the cursor with Hyprland `dispatch movecursor x y`
(global logical coordinates) and send buttons, relative motion and axis
events through one long-lived `zwlr_virtual_pointer_v1` on the overlay's
Wayland connection, each followed by `frame` and a roundtrip. They run only
after the overlay's unmap has been confirmed. Absolute virtual-pointer motion
is not used by default (hyprwm/Hyprland#6749); `input.pointer = "absolute"`
selects output-local `motion_absolute` instead. `input.restore_cursor = true`
moves the cursor back afterwards (`hover` excepted). `input.prefer =
"pointer"` skips AT-SPI for `click`/`focus`. ydotool, uinput and the
RemoteDesktop portal are deliberately not used: they need root-adjacent
setup or do not exist on Hyprland.

`copy` needs `zwlr_data_control_manager_v1` (or `ext_data_control_manager_v1`);
without it, `copy` fails with `clipboard unavailable: ...` and never falls
back to `wl-copy`. `woints status` shows `pointer` and `clipboard`
availability.

The `[input]`, `[modifiers]` and `actions.default` keys are constants with
the defaults above until the config loader lands (M5).

## Performance

Target: total p50 under 100 ms from `hints` to badges. TODO(verify): record
`woints bench -n 20` with `gtk3-widget-factory` and `qt6ct` focused.
Measured by the provider's live test against Firefox (Gecko, Collection with
web-content pruning, 18 elements): `collect.connect` 0.01 ms (reused
connection), `collect.match` 44 ms, `collect.extents` 14 ms,
`collect.filter` 0.1 ms.

Vision path, `WOINTS_VISION_MODE=always woints bench -n 20 --scope focused`,
release build, 7840HS laptop, foot focused at 1438×858 on a 2880×1800 scale 2
output (150 elements, capped by `vision.max_elements`):

| stage   | p50 ms | p95 ms |
|---------|-------:|-------:|
| capture |  16.8  |  22.1  |
| detect  |  26.2  |  28.1  |
| merge   |   0.01 |   0.02 |
| total   |  54.0  |  64.3  |

Target total p95 < 300 ms. TODO(verify): the same run on the 5900X desktop.

## Debugging

- `RUST_LOG=debug wointsd` logs every stage duration.
- `WOINTS_DUMP=/tmp/dump wointsd` writes one PNG per output for every rendered frame.
- `WOINTS_DEMO=1 wointsd` uses the M1 demo boxes instead of AT-SPI.
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
