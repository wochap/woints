# woints

Vimium-style keyboard hints for Hyprland. Press a key binding and letter
hints appear over interactive elements; type a hint to act on it.

Milestone 1 (this tree) ships the foundation: the resident daemon, the CLI and
its JSON socket protocol, Hyprland IPC and a fractional-scale-correct
layer-shell overlay. `woints hints` draws **demo** badges at the focused
window's corners and centre. Element providers (AT-SPI, vision, quickshell)
and actions arrive in later milestones.

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
| `woints-a11y`, `woints-vision`, `woints-input` | | placeholders for M2–M4 |

Pipeline stages (each timed, logged at `debug` as `stage=<name> ms=<f32>`):
`hypr.query → select → collect → label → place → render → commit`.

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
woints hints [--scope focused] [--action A]
woints cancel | status | quit
woints snapshot [--scope focused] [--json]
woints bench [-n 20]
```

Exit codes: `0` ok, `1` daemon replied `ok:false`, `2` daemon not reachable.

## Debugging

- `RUST_LOG=debug wointsd` logs every stage duration.
- `WOINTS_DUMP=/tmp/dump wointsd` writes one PNG per output for every rendered frame.
- `WOINTS_LIVE_TESTS=1 cargo test` enables tests that talk to the live
  Hyprland session; the overlay one maps a surface (and grabs the keyboard)
  for 500 ms.

## Font licence

Badges use **Atkinson Hyperlegible Mono Bold**, © 2020–2024 The Atkinson
Hyperlegible Mono Project Authors, embedded from
`crates/overlay/assets/AtkinsonHyperlegibleMono-Bold.ttf` under the SIL Open
Font License 1.1 (`crates/overlay/assets/OFL.txt`).
