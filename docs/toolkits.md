# Toolkit findings

What each toolkit exposes over AT-SPI, measured on the development machine
(NixOS, Hyprland 0.56.2, at-spi2-core 2.60.4) on 2026-10-06. The probe used for
the counts below is read-only:

```sh
cargo run -p woints-a11y --example probe -- <hyprland-class> [--register]
```

It pairs the window like the provider does, calls `GetMatches` on the frame
with the provider's rule, walks the whole frame (hidden subtrees included) and
calls `GetMatches` on every `document web` / `embedded` node.

## Firefox (Gecko 152.0.4) — design D22, task 7.1

Two Firefox windows open, both 1438×858; the ACTIVE frame is selected.

| run | `GetMatches(frame)` | walk visited | interactive | SHOWING + interactive | `GetMatches(document web)` |
|---|---|---|---|---|---|
| no listener | 32 (5–7 ms) | 3202 | 81 | 12 | 0 |
| `RegisterEvent("object:state-changed")` first | 32 (8.5 ms) | 3202 | 81 | 15 | 0 |

- Registering an `object:state-changed` listener does **not** flip page
  content to SHOWING: the count moved from 12 to 15 between runs on the
  same frame, within the noise of page activity. Collection on the browser
  chrome works (32 matches), Collection on the web document returns nothing.
- When Firefox is not the focused window, no frame carries ACTIVE and the two
  frames have the same size, so pairing falls back to `frame-guess` (first
  frame); in that run `GetMatches(frame)` returned 0 and a full walk visited
  5122 nodes in 1.4 s with 959 interactive nodes, none SHOWING.
- With `skip_web_content` (Gecko profile) the provider returned 18 elements
  via Collection; `collect.match` was ~45–50 ms because every match is checked
  for a web-content ancestor. Page content is left to Vimium.

## Electron / Chromium — task 7.2

- Bitwarden (Electron, `ToolkitName = Chromium`, version `1.0`), launched by
  the session with `ACCESSIBILITY_ENABLED=1` in its environment: the frame
  has **no children** (walk visited 1 node, `GetMatches` 0). The provider
  reports `collection-empty, walk-empty`.
- Chrome PWA (`chrome-chat.openai.com__-Default`, Google Chrome): same, 1 node.
- So `ACCESSIBILITY_ENABLED=1` alone does not populate Chromium's tree here.

TODO(verify) — needs a GUI launch, not run by the implementer:

```sh
ACCESSIBILITY_ENABLED=1 element-desktop &
cargo run -p woints-a11y --example probe -- Element
pkill -f element-desktop
element-desktop --force-renderer-accessibility &
cargo run -p woints-a11y --example probe -- Element
```

Record visited / interactive / SHOWING counts for both launches.

## GTK3, GTK4, Qt Widgets

From the M1 measurements (design "Verified on the target machine"):
GTK3 3.24 Collection 39 hits in 2 ms; GTK4 4.22 has no Collection, walk of
101 nodes; Qt6 Widgets 6.11 Collection 74 hits in 2 ms, 11 of them phantom
items of closed combo popups (filtered by `CollapsedComboPopup`).

TODO(verify) with the M2 provider (live tests, apps must be running):

```sh
gtk3-widget-factory & qt6ct & gnome-calculator &
WOINTS_LIVE_TESTS=1 cargo test -p woints-a11y --test live -- --nocapture --test-threads=1
```

## `doctor` inputs (M5) — task 7.3

- `NO_AT_BRIDGE=1` disables the GTK3 AT-SPI bridge. It is set in this
  session's environment; Thunar (GTK3) inherits it and never registers with
  the Registry, so pairing reports `no-a11y-app`. Check:
  `tr '\0' '\n' < /proc/<pid>/environ | grep NO_AT_BRIDGE`.
- `org.a11y.Status.IsEnabled` and `ScreenReaderEnabled` are both `false`
  (`busctl --user get-property org.a11y.Bus /org/a11y/bus org.a11y.Status IsEnabled`),
  yet Gecko, Qt and Electron register anyway because the session sets
  `GNOME_ACCESSIBILITY=1`, `QT_ACCESSIBILITY=1`,
  `QT_LINUX_ACCESSIBILITY_ALWAYS_ON=1` and `ACCESSIBILITY_ENABLED=1`.
  `doctor` should report all of these, not only `IsEnabled`.
- `gsettings get org.gnome.desktop.interface toolkit-accessibility` is
  `false`; GTK apps here do not depend on it.
