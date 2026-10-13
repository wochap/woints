-- woints key capture for Hyprland (Lua config).
--
-- Load from hyprland.lua with `require("woints").setup()`. Defines a static
-- submap whose only bind is a no-op catchall, reports every key press and
-- release to wointsd while the submap is active, and resets the submap on
-- Escape or after `timeout_ms` without a key press, whether or not the daemon
-- is alive.

local M = {}

function M.setup(opts)
  opts = opts or {}
  local name = opts.submap or "woints-keys"
  local timeout = opts.timeout_ms or 17000
  local token = 0

  local function arm()
    token = token + 1
    local mine = token
    hl.timer(function()
      if token == mine and hl.get_current_submap() == name then
        hl.dispatch(hl.dsp.submap("reset"))
      end
    end, { timeout = timeout, type = "oneshot" })
  end

  hl.define_submap(name, function()
    hl.bind("catchall", hl.dsp.no_op())
  end)

  hl.on("keybinds.submap", function()
    if hl.get_current_submap() == name then
      arm()
    else
      token = token + 1
    end
  end)

  hl.on("input.keyboard.key", function(keycode, _, state)
    if hl.get_current_submap() ~= name then
      return
    end
    if state == 1 then
      arm()
    end
    if keycode == 9 and state == 1 then
      hl.dispatch(hl.dsp.event(name .. " escape"))
      hl.dispatch(hl.dsp.submap("reset"))
    else
      hl.dispatch(hl.dsp.event(name .. " key " .. keycode .. " " .. state))
    end
  end)
end

return M
