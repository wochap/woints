# M5 verification checklist

Items that need the live session and a person watching it. Nothing here was
run unattended: importing the module edits your running shell, and `hints`
maps an overlay and grabs the keyboard (Escape dismisses it).

## Verified (unattended, read-only)

- [x] `woints doctor` against the live session (2026-10-06): exits 1 on
  `NO_AT_BRIDGE=1`, lists both quickshell instances as "Woints module not
  imported", all Wayland globals present, Hyprland 0.56.2.
- [x] `woints bench -n 20 --scope focused+layers` (numbers in the README).
- [x] `woints snapshot --scope layers`: bar and popup layers become targets;
  without the module they get vision elements.
- [x] `WOINTS_LIVE_TESTS=1 cargo test -p woints-qs live_doctor`.

## TODO(verify)

### Setup

```sh
nix develop
cargo build --release -p woints-daemon -p woints-cli
pkill -x wointsd; ./target/release/wointsd &
```

### Task 3.5 — quickshell module on your shell

1. Install the module next to `shell.qml` (plain copy; a symlink also works):

   ```sh
   cp -r integrations/quickshell ~/.config/quickshell/shell/Woints
   # or: ln -s "$PWD/integrations/quickshell" ~/.config/quickshell/shell/Woints
   ```

2. In a bar file, e.g. a workspace or tray button component, add:

   ```qml
   import Woints

   // inside the button's MouseArea / root item
   Hintable {
       label: "Workspace 1"            // or a binding to the button's text
       actions: ["click", "right"]
       onActivated: action => { /* same as the button's onClicked */ }
   }
   ```

   Quickshell reloads the config on save. Do not touch `lock.qml`.

3. Find the shell instance pid (the entry whose `config_path` ends in
   `shell.qml`):

   ```sh
   qs list --all --json
   PID=$(qs list --all --json | jq -r '.[] | select(.config_path | endswith("/shell.qml")) | .pid')
   ```

4. The IPC target exists:

   ```sh
   qs ipc --pid "$PID" show | sed -n '/^target woints/,/^target /p'
   ```

   - [ ] shows `function list(): string` and
     `function invoke(id: string, action: string): string`

5. Items are listed:

   ```sh
   qs ipc --pid "$PID" call woints list | jq .
   ```

   - [ ] `"protocol": 1`, one item per visible `Hintable`, `ns` is
     `quickshell:bar`, `screen` is `eDP-1`, `x/y/w/h` match the button
   - [ ] an item inside a hidden panel is absent; it appears when the panel opens

6. Invoke toggles a button (use an `id` from step 5):

   ```sh
   qs ipc --pid "$PID" call woints invoke 'quickshell:bar/1' click     # -> ok
   qs ipc --pid "$PID" call woints invoke 'quickshell:bar/1' middle    # -> unhandled
   qs ipc --pid "$PID" call woints invoke 'nope/0' click               # -> unknown
   ```

   - [ ] `click` runs the button's action and prints `ok`

7. woints sees the module:

   ```sh
   ./target/release/woints doctor | sed -n '/^quickshell/,/^$/p'
   ./target/release/woints snapshot --scope layers --json | jq '.elements[] | select(.source=="quickshell") | {name, rect}'
   ```

   - [ ] doctor prints `Woints module, N items` for the shell instance
   - [ ] snapshot elements have `source: "quickshell"` and rects on the bar

8. Hints on the bar:

   ```sh
   ./target/release/woints hints --scope layers
   ```

   - [ ] badges appear on bar buttons; typing a label runs the button through
     `invoke` (no cursor movement); Escape dismisses
   - [ ] `woints hints --scope focused+layers`: window badges get the
     single-glyph labels, bar badges the longer ones
   - [ ] `woints bench -n 20 --scope focused+layers` shows a `qs.list` row;
     record p50/p95 in the README (follow-up if p95 > ~20 ms)

### Task 1.6 — badge clamping on layers (design §09)

```sh
./target/release/woints hints --scope layers
```

- [ ] the badge of a button at the bar's top-left corner is clamped to
  (1,1), fully on screen
- [ ] the badges of tray icons at the right edge are clamped inside the
  output's right edge
- [ ] with a second monitor attached, `woints hints --scope all` draws badges
  on both outputs

Report failures in the change's tasks file or as issues.

## Decisions to confirm

Made by the implementing agent without the user; each is easy to revert.

1. **`scope.levels` defaults to `["bottom", "top", "overlay"]`** — the spec
   said top/overlay, but your quickshell bar is on the `bottom` level
   (verified live), so it would never be hinted. `background` stays excluded.
2. **`merge.iou_threshold` stays 0.3** — the M5 config spec says 0.5; the
   archived `provider-merge` spec measured and set 0.3.
3. **`hint.alphabet` moved to `keys.alphabet`** — in `[hint]` it now only
   warns "unknown key"; the display alphabet comes from `[keys]`.
4. **`#rrggbbaa` colours are accepted, alpha ignored** — badges are opaque.
5. **Reload with hints on screen:** alphabet change cancels the session;
   colour/layout-only change redraws it.
6. **`WOINTS_VISION_MODE` removed** — use `[vision] mode`.
7. **`Hintable` is a zero-size invisible `Item`**, not item-less as the spec
   says: quickshell's `QsWindow` attached property needs an `Item`.
8. **Quickshell `invoke` failure falls back to the pointer** with the same
   action; drag and scroll fall back to a plain click.
9. **`NO_AT_BRIDGE=1` is always `fail` in `doctor`**, regardless of the
   focused window (spec scenario wins over the design note D18).
10. **New files are untracked** — `nix build` from the git flake will not see
    `crates/qs`, `integrations/` etc. until `git add`; verification used
    `nix build path:.`. Nothing is committed.
