# Woints quickshell module

Quickshell's QML windows are not reachable over AT-SPI (the Qt application
root never adopts `QQuickWindow`s), so woints cannot see your bar's buttons
on its own. This module lets your shell tell woints what is hintable over
quickshell IPC. Without it, woints falls back to vision on quickshell layers.

## Install

Pick one:

- **Plain copy** (any distro): copy this directory to
  `~/.config/quickshell/<your config>/Woints` (next to `shell.qml`). Quickshell
  resolves `import Woints` from the config directory.
- **Import path**: point `QML_IMPORT_PATH` (or `QML2_IMPORT_PATH`) at a
  directory containing `Woints/`, e.g. this repo's `integrations/quickshell`
  parent with the directory renamed or symlinked to `Woints`.
- **Nix**: the flake output `packages.<system>.quickshell-module` installs to
  `share/quickshell/Woints`; add `${pkg}/share/quickshell` to
  `QML_IMPORT_PATH` of your quickshell service, or link it into your config:

  ```nix
  xdg.configFile."quickshell/shell/Woints".source =
    "${inputs.woints.packages.${pkgs.system}.quickshell-module}/share/quickshell/Woints";
  ```

Then `import Woints` in the files that declare buttons. `woints doctor` shows,
per quickshell instance, whether the module is present and how many items it
lists.

Do not import the module in a lock-screen config: woints only talks to
instances that register the `woints` IPC target, and hints over a lock
screen are not wanted.

## Usage

Inside a reusable button component, so every button is hintable:

```qml
// BarButton.qml
import QtQuick
import Woints

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
