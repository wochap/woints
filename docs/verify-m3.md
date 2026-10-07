# M3 verification checklist

Items the implementing agent could not verify without watching the live
session. The live tests click, drag and scroll real windows: run them only
when you are not using the session.

## Verified

- [x] Data control protocol: Hyprland 0.56.2 advertises
  `zwlr_data_control_manager_v1`; the overlay binds it and reports
  `clipboard: zwlr_data_control_v1` (live test
  `virtual_pointer_binds_and_roundtrips`, 2026-10-06). `copy` is available.
- [x] `zwlr_virtual_pointer_manager_v1` binds and a `Roundtrip` completes
  (same test).

## TODO(verify)

- [ ] `absolute` backend (`input.pointer = "absolute"`): correct on two
  outputs of different scale? Click a button on each output with the absolute
  backend; if the cursor lands on the element every time, it could become the
  default. Until then `movecursor` stays the default.
- [ ] Electron `DoAction`: does `click` through AT-SPI do anything in an
  Electron app (e.g. VS Code, Discord)? If not, use `input.prefer = "pointer"`
  (per-class overrides come with the M5 config).
- [ ] Every live test below passes.

## Commands

```sh
nix develop

# Overlay: virtual pointer binds, roundtrip (no window, no pointer events)
WOINTS_LIVE_TESTS=1 cargo test -p woints-overlay --test live virtual_pointer -- --nocapture

# 6.1 / 6.2 pointer tests: launch gtk3-widget-factory and thunar, click,
# double click, hover, drag and scroll in the live session.
WOINTS_LIVE_TESTS=1 cargo test -p woints-input --test live -- --test-threads=1 --nocapture

# Esc in drag phase 2 also needs a running daemon:
wointsd &
WOINTS_LIVE_TESTS=1 cargo test -p woints-input --test live escape_in_drag_phase_two -- --nocapture

# Absolute backend: no config loader yet (M5); change `InputConfig::default().pointer`
# to `PointerKind::Absolute`, rebuild, then:
woints snapshot --json        # pick an element id per output
woints act --snapshot N --id E --action click

# Electron DoAction:
woints snapshot --json        # with the Electron window focused
woints act --snapshot N --id E --action click
```
