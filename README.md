# woints

Vimium-style keyboard hints for Hyprland. Press a key binding and letter
hints appear over interactive elements; type a hint to act on it.

Milestone 2 (this tree) finds real elements through AT-SPI and acts on them:
`woints hints` badges the interactive elements of the focused GTK3, GTK4 or
Qt Widgets window; typing filters the badges, Backspace undoes, a full label
flashes and activates the element (`DoAction`) or focuses it (`GrabFocus`
for text entries). Pointer actions (M3), vision fallback (M4) and quickshell
(M4–M5) come later. Toolkit findings are in [`docs/toolkits.md`](docs/toolkits.md).

## Architecture

```
                     ┌──────────────── wointsd ────────────────┐
  woints CLI ──────► │ Session FSM: Idle ─hints─► Showing      │
  JSON lines         │            ▲  cancel / Esc / quit ◄──┘  │
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
| `woints-vision`, `woints-input` | | placeholders for M3–M4 |

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
```

Verified against Hyprland **0.56.2**.

## CLI

```
woints hints [--scope focused] [--action activate|focus]
woints cancel | status | quit
woints snapshot [--scope focused] [--json]
woints act --snapshot N --id E [--action activate|focus|left|right|middle|double|hover]
woints bench [-n 20]
```

Exit codes: `0` ok, `1` daemon replied `ok:false`, `2` daemon not reachable.

Only `activate` and `focus` succeed in M2; other actions reply
`no actuator for <action>` until the pointer actuator lands (M3).

## Performance

Target: total p50 under 100 ms from `hints` to badges. TODO(verify): record
`woints bench -n 20` with `gtk3-widget-factory` and `qt6ct` focused.
Measured by the provider's live test against Firefox (Gecko, Collection with
web-content pruning, 18 elements): `collect.connect` 0.01 ms (reused
connection), `collect.match` 44 ms, `collect.extents` 14 ms,
`collect.filter` 0.1 ms.

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
