# woints Hyprland module

Key capture for `wointsd` on Hyprland builds with Lua config. The module defines
a static submap (default `woints-keys`) whose only bind is a no-op catchall. While
the submap is active it reports every key press and release to the daemon, resets
the submap on Escape, and resets it after `timeout_ms` without a key press, even if
the daemon is dead.

## Setup

Plain copy:

```sh
cp woints.lua ~/.config/hypr/woints.lua
```

Nix / home-manager: build `packages.<system>.hyprland-module` and link
`share/hypr/woints.lua` next to `hyprland.lua`.

Then add to `hyprland.lua` and reload:

```lua
require("woints").setup()
```

Options: `setup({ submap = "woints-keys", timeout_ms = 17000 })`. `submap` must
match `input.submap` in the woints config.

## Check

`woints doctor` prints `keys: submap woints-keys` once the module is loaded. Without
it, woints falls back to the overlay key source, which closes popups.

## Troubleshooting

If Hyprland is stuck in the submap, press Escape, or run
`hyprctl dispatch submap reset`.
