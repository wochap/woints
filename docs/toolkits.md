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

## Electron / Chromium — measured 2026-10-10

- Chromium and Electron apps register on the AT-SPI bus but keep the tree
  dormant: the frame's only child is the null object `/org/a11y/atspi/null`
  until Chromium's AX mode is on.
- `org.a11y.atspi.Accessible.GetAttributes` on the frame turns it on.
  Chromium's `AtkGetAttributes` calls `OnExtendedPropertiesUsedInWebContent()`,
  which adds `kAXModeBasic` (`kNativeAPIs | kWebContents`); without
  `kNativeAPIs` nothing below the top-level window is exposed.
- Bruno (Electron 41.9.1, no launch flags): 2 → 165 nodes within 0.2 s.
- Chrome 150: 3 → 240 nodes within 1 s. Browser UI only is kept
  (`skip_web_content`); page content stays with Vimium.
- `ACCESSIBILITY_ENABLED=1`, other env vars, gsettings and `ScreenReaderEnabled`
  toggles only gate bridge registration; none of them wakes the tree.
- `--disable-renderer-accessibility` blocks the wake; `doctor` reports
  `fail  Chromium tree: dormant after wake`.
- Cost: Chrome with one tab went from 512 to about 532 MB PSS; idle CPU
  unchanged. The tree stays on for the app's lifetime.
- woints wakes the frame on the first request per app (up to 1000 ms, outside
  the collection budget); `[a11y] wake_exclude` or `wake_chromium = false`
  opts out.

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
