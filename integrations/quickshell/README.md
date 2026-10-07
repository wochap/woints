# Woints quickshell module

Quickshell's QML windows are not reachable over AT-SPI (the Qt application
root never adopts `QQuickWindow`s), so woints cannot see your bar's buttons
on its own. This module lets your shell tell woints what is hintable over
quickshell IPC. Without it, woints falls back to vision on quickshell layers.

## Install

Pick one:

- **Plain copy into the config** (recommended, verified on quickshell 0.3.1):
  copy only the two QML files to `~/.config/quickshell/<your config>/Woints/`
  (next to `shell.qml`) and import it as **`qs.Woints`**:

  ```sh
  mkdir -p ~/.config/quickshell/shell/Woints
  cp integrations/quickshell/{SHints.qml,Hintable.qml} ~/.config/quickshell/shell/Woints/
  ```

  Do **not** copy `qmldir`: quickshell exposes config subdirectories as
  `qs.<dir>` and synthesises the qmldir (including the `SHints` singleton from
  `pragma Singleton`); a hand-written `module Woints` qmldir disables that and
  the import fails with `module "Woints" is not installed`.
- **Import path**: point `QML_IMPORT_PATH` at a directory containing `Woints/`
  (with the shipped `qmldir`), then use bare `import Woints`. Untested on
  quickshell 0.3.1 (TODO(verify)).
- **Nix**: the flake output `packages.<system>.quickshell-module` installs to
  `share/quickshell/Woints` (with `qmldir`, for the import-path route). For the
  config-copy route link the two QML files, not the directory:

  ```nix
  xdg.configFile."quickshell/shell/Woints/SHints.qml".source =
    "${inputs.woints.packages.${pkgs.system}.quickshell-module}/share/quickshell/Woints/SHints.qml";
  xdg.configFile."quickshell/shell/Woints/Hintable.qml".source =
    "${inputs.woints.packages.${pkgs.system}.quickshell-module}/share/quickshell/Woints/Hintable.qml";
  ```

Then `import qs.Woints` (config-copy route) or `import Woints` (import-path
route) in the files that declare buttons, and import it once in `shell.qml`
so the `woints` IPC target registers at startup. `woints doctor` shows, per
quickshell instance, whether the module is present and how many items it
lists.

### Windows must expose their layer namespace

Woints pairs items with Hyprland layer surfaces by namespace. The attached
`WlrLayershell` object is not readable from JavaScript, so every
`PanelWindow` that contains hintable items needs a plain property mirroring
it:

```qml
PanelWindow {
    WlrLayershell.namespace: "quickshell:bar"
    readonly property string namespace: WlrLayershell.namespace
    ...
}
```

Without it `list()` reports `ns: ""` and the items are dropped.

Do not import the module in a lock-screen config: woints only talks to
instances that register the `woints` IPC target, and hints over a lock
screen are not wanted.

## Usage

Inside a reusable button component, so every button is hintable:

```qml
// BarButton.qml
import QtQuick
import qs.Woints   // or `import Woints` on the import-path route

MouseArea {
    id: button
    property alias text: label.text
    signal triggered(string action)

    implicitWidth: 32
    implicitHeight: 32
    acceptedButtons: Qt.LeftButton | Qt.RightButton
    onClicked: mouse => button.triggered(mouse.button === Qt.RightButton ? "right" : "click")

    Text { id: label; anchors.centerIn: parent }

    Hintable {
        label: button.text
        actions: ["click", "right"]
        onActivated: action => button.triggered(action)
    }
}
```

Wrapping a raw `MouseArea`:

```qml
MouseArea {
    id: area
    width: 24; height: 24
    onClicked: Hyprland.dispatch("workspace 1")

    Hintable {
        label: "Workspace 1"
        onActivated: Hyprland.dispatch("workspace 1")
    }
}
```

`Hintable` properties:

| property | default | meaning |
|---|---|---|
| `target` | `parent` | item whose rect is hinted |
| `label` | `""` | name reported to woints (`snapshot`, logs) |
| `actions` | `["click"]` | actions the item handles; others return `unhandled` |
| `enabled` | `true` | `false` hides it from woints |
| `ns` | `""` | layer namespace; empty reads `WlrLayershell.namespace` of the window |
| `activated(action)` | | signal fired by `invoke` |

An item is listed only while `target.visible`, its window is visible,
`target.opacity > 0` and its rect intersects the window.

If `invoke` fails or returns `unhandled`, woints clicks the element centre
with the pointer instead.

## Protocol

IPC target `woints`:

| function | returns |
|---|---|
| `list(): string` | `{"protocol":1,"items":[{"id","ns","screen","x","y","w","h","label","actions"}]}`; `x, y` window-relative logical px |
| `invoke(id: string, action: string): string` | `ok`, `unknown` (no such id), `unhandled` (action not in `actions`) |

| protocol | woints | notes |
|---|---|---|
| 1 | 0.1 | initial |

woints refuses other protocol versions and says so in `woints doctor`.
Try it by hand: `qs ipc call woints list`.

TODO(verify): the examples above have not been loaded in a live quickshell
session yet.
