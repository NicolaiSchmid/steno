-- Hyprland window rules for Steno's floating panels, the recording bubble
-- ("Steno bubble") and the meeting prompt ("Steno prompt"). Hyprland 0.55
-- or later, whose config is Lua. Load the file from
-- ~/.config/hypr/hyprland.lua, below your other rules:
--
--   dofile("/usr/share/steno-desktop/hyprland-steno.lua") -- the .deb or the AUR package
--   require("steno")  -- a copy at ~/.config/hypr/steno.lua
--
-- On Hyprland Steno runs under XWayland, so Steno places the panels itself.
-- These rules keep them floating on every workspace, out of the keyboard
-- focus, and without a border, shadow or blur around their transparent
-- corners. The class is "steno-desktop" on Wayland and "Steno-desktop"
-- under XWayland; the panels keep the title they open with.
hl.window_rule({
  name = "steno-panels",
  match = { class = "[Ss]teno-desktop", title = "Steno (bubble|prompt)" },
  float = true,
  pin = true,
  no_initial_focus = true,
  no_follow_mouse = true,
  border_size = 0,
  no_shadow = true,
  no_blur = true,
})
