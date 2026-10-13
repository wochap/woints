-- Run from this directory: lua test.lua
package.path = "./?.lua;" .. package.path

local function stub()
  local s = { binds = {}, defined = {}, listeners = {}, timers = {}, events = {}, resets = 0, current = "" }
  hl = {
    dsp = {
      no_op = function() return "no_op" end,
      submap = function(n) return { submap = n } end,
      event = function(p) return { event = p } end,
    },
    define_submap = function(n, f) s.defined[#s.defined + 1] = n; f() end,
    bind = function(k, d) s.binds[#s.binds + 1] = { k, d } end,
    on = function(ev, f) s.listeners[ev] = f end,
    timer = function(f, o) s.timers[#s.timers + 1] = { f = f, o = o } end,
    get_current_submap = function() return s.current end,
    dispatch = function(d)
      if d.event then
        s.events[#s.events + 1] = d.event
      elseif d.submap == "reset" then
        s.resets = s.resets + 1
        s.current = ""
      end
    end,
  }
  return s
end

local function load()
  package.loaded["woints"] = nil
  local s = stub()
  require("woints").setup()
  return s
end

local function eq(a, b, msg)
  if a ~= b then error(msg .. ": expected " .. tostring(b) .. ", got " .. tostring(a), 2) end
end

do -- only the catchall bind
  local s = load()
  eq(#s.defined, 1, "one submap")
  eq(s.defined[1], "woints-keys", "default name")
  eq(#s.binds, 1, "one bind")
  eq(s.binds[1][1], "catchall", "catchall")
  eq(s.binds[1][2], "no_op", "no_op")
end

do -- events only inside the submap
  local s = load()
  s.listeners["input.keyboard.key"](38, 0, 1)
  eq(#s.events, 0, "no event outside the submap")
  eq(#s.timers, 0, "no timer outside the submap")
  s.current = "woints-keys"
  s.listeners["input.keyboard.key"](38, 0, 1)
  s.listeners["input.keyboard.key"](38, 0, 0)
  eq(s.events[1], "woints-keys key 38 1", "press")
  eq(s.events[2], "woints-keys key 38 0", "release")
  eq(#s.timers, 1, "press re-arms, release does not")
end

do -- escape emits then resets
  local s = load()
  s.current = "woints-keys"
  s.listeners["input.keyboard.key"](9, 0, 1)
  eq(s.events[1], "woints-keys escape", "escape event")
  eq(s.resets, 1, "reset")
  eq(#s.events, 1, "no key event for escape")
end

do -- only the current timer resets
  local s = load()
  s.current = "woints-keys"
  s.listeners["keybinds.submap"]()
  s.listeners["input.keyboard.key"](38, 0, 1)
  eq(#s.timers, 2, "two timers armed")
  s.timers[1].f()
  eq(s.resets, 0, "stale timer is a no-op")
  s.timers[2].f()
  eq(s.resets, 1, "current timer resets")
  eq(s.timers[2].o.timeout, 17000, "timeout")
end

do -- leaving the submap invalidates the timer
  local s = load()
  s.current = "woints-keys"
  s.listeners["keybinds.submap"]()
  s.current = ""
  s.listeners["keybinds.submap"]()
  s.current = "woints-keys"
  s.timers[1].f()
  eq(s.resets, 0, "timer from a previous visit")
end

do -- options
  package.loaded["woints"] = nil
  local s = stub()
  require("woints").setup({ submap = "x", timeout_ms = 5 })
  eq(s.defined[1], "x", "custom name")
  s.current = "x"
  s.listeners["keybinds.submap"]()
  eq(s.timers[1].o.timeout, 5, "custom timeout")
end

print("ok")
