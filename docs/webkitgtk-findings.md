# WebKitGTK (Tauri) findings

Why `woints hints` finds nothing in Tauri apps on Linux, and what to change.
Measured on 2026-10-09 against wodeo (Tauri 2, GTK 3.24.52, WebKitGTK
2.52.4 abi 4.1), NixOS, Hyprland 0.56.2, a11y bus
`unix:path=/run/user/1000/at-spi/bus_0`. Independently re-verified live by a
second reviewer the same day; corrections from that review are folded in.

## TL;DR

- Nothing needs to change in Tauri or in the app. Tauri (wry) has no
  accessibility code; WebKitGTK exposes the full DOM over AT-SPI on its own,
  with names, correct `WINDOW` extents and a `press` action.
- woints misses it because of three collection blockers and one actuation
  blocker, all in woints (see [Blockers](#blockers)).
- Fast path confirmed: `Collection.GetMatches` on the first WebKit node that
  implements Collection returns every interactive element of wodeo in
  **0.5 ms**.
- Direction: numeric roles everywhere (A), descend across the process
  boundary and run Collection there (B), detect WebKitGTK (C), and harden
  the actuator (D).

## How the tree is laid out

A Tauri window is two processes, each with its own a11y bus connection:

```
 UI process (GTK3), pid 1084667, :1.411            ToolkitName "gtk" 3.24.52
 └─ application
    └─ frame "wodeo"                     SHOWING
       ├─ panel → panel → filler …       (CSD title bar, not SHOWING)
       └─ filler                         SHOWING
          └─ filler (WebKitWebView)      no SHOWING/VISIBLE/SENSITIVE ← walk prunes here
             └─ child ref (org.webkit.app-<hash>.Sandboxed.WebProcess-<uuid>,
                           /org/a11y/webkit/accessible/<uuid>) ─┐
                                                                │
 Web process (WebKitWebProcess), pid 1084716, :1.412  ◄─────────┘
   owns the org.webkit.… well-known name; NOT listed under the Registry root
 └─ filler (plug, Accessible+Component only, no states)
    └─ scroll pane (role 49, ENABLED|FOCUSABLE|SENSITIVE, has Collection)
       └─ document web (role 95, SHOWING|VISIBLE|ENABLED, Collection)
          ├─ landmark (110) → paragraph, sections, button "Hardware acceleration active",
          │                   button "Replace", button "Show settings"
          ├─ video (107)
          ├─ button "Go to in" / "Previous frame" / "Play" / "Next frame" /
          │  "Go to out" / "Play selection"                       (role 43)
          └─ landmark "Timeline" → section → slider "Trim start",
                                             slider "Trim end"   (role 51)
```

Notes:

- Object paths: only the plug lives at `/org/a11y/webkit/accessible/<uuid>`;
  every other web node is `/org/a11y/atspi/accessible/<uuid>`. Do not filter
  on the path prefix.
- Bus names: the UI process's child ref to the plug uses the **well-known**
  `org.webkit.…Sandboxed.WebProcess-…` name; everything below it, and
  `Parent` of the plug (which points back to UI `:1.x`/1), uses unique
  names. So the plug object is reachable under two bus names and `Obj`
  equality / caches for it are inconsistent.
- `GetApplication` on a web node returns the UI process's application root.
- `GetAttributes` on the plug and every web node returns
  `{toolkit: "WebKitGTK", computed-role: …, tag: …}` (the GTK frame says
  `toolkit=gtk`). This is the most robust WebKit signal (see C).
- Pairing already works: `pair` reports `Some("pid")` on the GTK3 app.
- Web objects implement `Accessible, Component, Hypertext, Action,
  Collection` (+ `Text`, `Value`, `Document`, `Hyperlink` per role).

## Blockers

### 1. Collection on the frame does not cross the process boundary

`GetMatches` → **0** on the frame (`provider.rs:222`), on the app root and
on the UI-process WebKitWebView filler. The GTK3 bridge's Collection only
walks its own in-process tree. Provider reports `collection-empty` and falls
to the walk.

### 2. WebKit returns `''` from `GetRoleName`

Raw D-Bus, every web-process node:

```
'' role=95 :1.412 …  ifs=[Accessible, Component, Action, Document, Collection]
'' role=43 :1.412 …  ifs=[Accessible, Component, Hyperlink, Hypertext, Action, Collection]
'' role=51 :1.412 …  ifs=[…, Value, …]
```

`GetRole` (numeric) is correct. (`GetLocalizedRoleName` does return
`'document web'` / `'push button'` here, but it is locale-dependent; do not
rely on it.) woints decides on role-name strings in many places, so nothing
in web content ever matches:

| use | location |
|---|---|
| walk node role | `a11y/src/provider.rs:277` (`tree_walk`) |
| ancestor role (filters, parents) | `a11y/src/provider.rs:319` |
| role for pending items without one | `a11y/src/provider.rs:456` |
| `needs_parent` (`"list item"`, `"menu item"`) | `a11y/src/provider.rs:503` |
| combo ancestor (`"combo box"`) | `a11y/src/provider.rs:534` |
| `atspi_caps` (`"entry"`, `"password text"`) | `a11y/src/provider.rs:591` |
| web-content ancestor check | `a11y/src/provider.rs:620-633` (`in_web_content`) |
| `CollapsedComboPopup` (`"list item"`) | `a11y/src/filter.rs:43` |
| scroll-bar lookup in `scroll_value` | `a11y/src/actuator.rs:89` |
| `base_filters` `parent_roles` `"list"`, `"menu"` | `core/src/profile.rs:58` |
| `hintable` → `is_scroll_container(&e.role)` | `daemon/src/pipeline.rs:279` |
| name tables | `a11y/src/rule.rs` `INTERACTIVE_ROLE_NAMES`, `SCROLL_CONTAINER_ROLE_NAMES`, `WEB_CONTENT_ROLE_NAMES` |

`Element.role` itself is a display string consumed by core and the daemon,
so A has to keep producing a canonical name (see A).

Side finding, affects plain GTK3 too: GTK 3.24.52's bridge returns
`'button'` (role 43) for the CSD title-bar buttons over D-Bus, not
`'push button'`. `INTERACTIVE_ROLE_NAMES` has `"push button"`, so the GTK3
walk path already misses buttons; it has not shown because GTK3 normally
succeeds via Collection, which matches numeric roles.

### 3. SHOWING pruning stops before the document

`walk.rs:94` skips children of any depth > 0 node without `SHOWING &&
VISIBLE`. Three consecutive nodes above the document carry neither: the
WebKitWebView filler (UI process), the plug filler and the scroll pane (web
process). The walk never reaches `document web`, which *is* SHOWING.

The read-only probe (`examples/probe.rs`) forces `showing: true` and still
finds 0 interactive and no web node, because of blocker 2 (it detects web
nodes by role name too):

```
app wodeo gtk 3.24.52 paired Some("pid")
GetMatches(frame): Ok(0) in 1.1 ms
walk: visited 44 interactive 0 showing+interactive 0 truncated false in 14.7 ms
```

### 4. `Action.GetActions` never answers on WebKit nodes (actuation)

On web buttons, `Action.GetActions` times out (reproduced at 5 s and 15 s
on several buttons), while `NActions` = 1 and `Action.GetName(0)` =
`"press"` reply in ~3 ms. `AtspiActuator::act` (`a11y/src/actuator.rs:114`)
calls `dbus::action_names` → `GetActions` with no timeout before
`DoAction`, so the `DoAction` plan would hang on every web element.

## Fast path evidence

Collection on the `document web` node with roles
`{PUSH_BUTTON, SLIDER, LINK, ENTRY, CHECK_BOX, TOGGLE_BUTTON, COMBO_BOX}`,
states `ALL {SHOWING}`, `traverse=true` (same result with `S+V+E`):

```
GetMatches(doc) -> 11 in 0.5ms
  button 'Hardware acceleration active' 615 18  181x15 press
  button 'Replace'                      808 8   80x35  press
  button 'Show settings'                902 7   36x37  press
  button 'Go to in'                     16  818 36x37  press
  button 'Previous frame'               58  818 36x37  press
  button 'Play'                         100 818 36x37  press
  button 'Next frame'                   142 818 36x37  press
  button 'Go to out'                    184 818 36x37  press
  button 'Play selection'               230 819 116x35 press
  slider 'Trim start'                   20  914 10x88  (empty action name)
  slider 'Trim end'                     928 914 10x88  (empty action name)
GetMatches(frame) -> 0
```

(Action names above come from `GetName(i)`, not `GetActions`; see blocker 4.)

Collection also works one level higher, on the scroll pane (the first web
node implementing Collection): it returns **12**, the 11 above plus the
`document web` itself, which woints wants anyway as a scroll container.

Coordinates: web `WINDOW` extents line up with a 958×1038 frame whose
`WINDOW` origin is 0,0, the space the provider already uses for GTK3.
`GetExtents(SCREEN)` returns the same 0-based numbers although Hyprland
places the window at (1,41), so SCREEN is useless (consistent with D16).
Caveat: WebKit computes `WINDOW` relative to the web view root; in wodeo the
web view sits at (0,0) of the frame, so they coincide. A Tauri window with a
visible GTK header bar / decorations may be offset; see D.

## Proposed direction

### A. Numeric roles everywhere

`GetRole` (u32) replaces `GetRoleName` for every decision; the name tables
become role-id tables (ids already exist in `rule.rs` `role::*`). Map the id
to a canonical name locally so `Element.role` stays meaningful for core,
filters and logs. Fixes blocker 2 and the GTK3 `'button'` drift.

### B. Cross the process boundary, then Collection

When frame Collection misses (or always for a WebKit app, see C):

- Descend from the frame, ignoring SHOWING pruning **only** at cross-bus
  child refs (child bus name ≠ the app's bus name) and the nodes directly
  below them, bounded depth (5 in wodeo; ≤ 8 is plenty).
- Run `GetMatches` on the first cross-bus descendant that implements
  Collection (the scroll pane here). No need to hunt for role 95.
- Merge with the frame's own matches (the GTK chrome).
- Collect every plug, not the first (Tauri multiwebview).

This is also the right shape for Electron/Chromium once their tree is
populated (`docs/toolkits.md` TODO).

### C. WebKitGTK detection

Agreed direction: toolkit `gtk` plus a cross-process web child. Review
refinement on the signal:

- **Preferred:** `GetAttributes` → `toolkit == "WebKitGTK"` on the
  cross-bus child (or simply "the child ref's bus name ≠ the app's bus
  name").
- **Avoid as the only test:** bus-name prefix `org.webkit.`. That
  well-known name is a product of WebKit's bwrap sandbox; an unsandboxed
  WebKit (`WEBKIT_DISABLE_SANDBOX_THIS_IS_DANGEROUS`, distros without
  bwrap) probably hands out a `:1.x` ref and the prefix test silently fails.

When detected: web content is the app, so `skip_web_content = false` and
use B. Unlike Gecko/Chromium browsers, there is no Vimium for WebKitGTK apps
(Tauri, Epiphany, GNOME web views). Profile lookup
(`core/src/profile.rs:80`) runs before the tree is seen, so detection is a
post-pair refinement during collection, not part of
`ToolkitProfile::lookup`. `in_web_content` and Gecko/Chromium
`skip_web_content` behaviour stay, but on numeric roles.

### D. Actuation hardening

- Replace `GetActions` with `NActions` + `GetName(i)` (or `DoAction(0)`
  when `NActions == 1`), and put a timeout on every actuation D-Bus call.
- Web element handles carry the web process's `:1.x`, which `AppCache`
  does not know, so `AtspiActuator::preference` falls back to the default
  profile. Fine today, but a WebKit-specific action preference would not
  apply; key it by app (e.g. via `GetApplication`) if one is ever needed.
- `scroll_value` (`actuator.rs:85`) looks for `scroll bar` children; WebKit
  exposes none, so `scroll` on a web document yields no value. Needs a
  fallback (pointer axis without a value, or the document's own extents).
- Extents guard: compare the plug's `WINDOW` origin with the UI-process
  WebKitWebView filler's `WINDOW` origin and offset web elements if they
  differ. Verify once with a decorated Tauri window.

## Open questions

1. Does `document web` ever appear nested (iframes) in WebKitGTK, and does
   `GetMatches(traverse=true)` cross into them? Not seen in wodeo.
2. Multiple web views per window: one plug per view; does each get its own
   web process / bus name?
3. Does WebKitGTK 6.0 (GTK4) look the same? GTK4 has no Collection on its
   own tree (TreeWalk-only profile), but the web process may still offer
   Collection.
4. Prefer AT-SPI `DoAction` (`press`) over a pointer click for web buttons
   (works when occluded)? `DEFAULT_ACTION_PREFERENCE` already lists `press`;
   blocked on D.
5. Environment: the web process registered with only the session vars
   (`ACCESSIBILITY_ENABLED=1`, `GNOME_ACCESSIBILITY=1`, no `NO_AT_BRIDGE`).
   `NO_AT_BRIDGE=1` would kill the GTK3 UI side and the whole window would
   report `no-a11y-app`; `doctor` already checks it.

## Reproduce

Launch any Tauri app (here `wodeo`), then:

```sh
# woints view
cargo run -p woints-a11y --example probe -- wodeo

# who owns the WebKit name
busctl --address=unix:path=/run/user/1000/at-spi/bus_0 list --no-legend | grep org.webkit
```

Raw-role dump (python + gi; run with
`nix shell --impure --expr 'with import <nixpkgs>{}; [ (python3.withPackages (p: [ p.pygobject3 ])) ]'`
and `GI_TYPELIB_PATH` pointing at gobject-introspection's and glib's
`lib/girepository-1.0`, plus at-spi2-core's for the `Atspi` variant):

```python
import sys
from gi.repository import Gio
c = Gio.DBusConnection.new_for_address_sync(sys.argv[1],
    Gio.DBusConnectionFlags.AUTHENTICATION_CLIENT |
    Gio.DBusConnectionFlags.MESSAGE_BUS_CONNECTION, None, None)
def call(bus, path, m):
    return c.call_sync(bus, path, 'org.a11y.atspi.Accessible', m,
                       None, None, 0, 3000, None).unpack()
def walk(bus, path, d):
    rn, r = call(bus, path, 'GetRoleName')[0], call(bus, path, 'GetRole')[0]
    print('  ' * d + f'{rn!r} role={r} {bus} {path}')
    for b, p in call(bus, path, 'GetChildren')[0]:
        walk(b, p, d + 1)
walk(sys.argv[2], '/org/a11y/atspi/accessible/root', 0)  # argv[2] = app's :1.x
```

Collection test: with `gi.require_version('Atspi', '2.0')`, find the
`Atspi.Role.DOCUMENT_WEB` (or scroll pane) descendant of the app and call
`get_matches(Atspi.MatchRule.new(StateSet([SHOWING]), ALL, None, NONE,
roles, ANY, None, NONE, False), CANONICAL, 0, True)`.

Blocker 4: on a web button, `org.a11y.atspi.Action.GetName(0)` answers,
`org.a11y.atspi.Action.GetActions` times out.
