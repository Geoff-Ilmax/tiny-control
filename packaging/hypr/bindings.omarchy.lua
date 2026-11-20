-- Example tiny-control keybindings for Omarchy (Hyprland-in-Lua).
--
-- Copy the o.bind lines you want into ~/.config/hypr/bindings.lua. Omarchy
-- auto-reloads on save; validate with `hyprctl reload && hyprctl configerrors`.
--
-- These use SUPER+ALT+<key>, which are unbound in a stock Omarchy install.
-- If you have bound any of them, unbind first: hl.unbind("SUPER + ALT + C").
--
-- `-c selected` acts on the camera chosen in the bar widget (or with
-- `tinyctl select <camera>`), so the same keys follow your selection.

-- Camera sleep toggle — the headline feature. Notifies the resulting state.
o.bind("SUPER + ALT + C", "Camera sleep toggle",
  [[sh -c 'notify-send -a tiny-control "Camera" "$(tinyctl -c selected toggle 2>&1)"']])

-- Recenter the gimbal (park).
o.bind("SUPER + ALT + R", "Camera recenter", "tinyctl -c selected recenter")

-- Toggle AI subject tracking. Notifies the resulting state.
o.bind("SUPER + ALT + T", "Camera tracking toggle",
  [[sh -c 'notify-send -a tiny-control "Camera" "Tracking $(tinyctl -c selected track toggle 2>&1)"']])

-- Cycle the selected camera to the next one in `tinyctl list`.
o.bind("SUPER + ALT + N", "Next camera",
  [[sh -c 'notify-send -a tiny-control "Camera" "$(tinyctl select next 2>&1)"']])

-- Pinned to one camera regardless of the selection: use its alias (set with
-- `tinyctl alias <camera> main`). Sleep every camera at once with `-c all`.
-- o.bind("SUPER + ALT + 1", "Main camera sleep toggle", "tinyctl -c main toggle")
-- o.bind("SUPER + ALT + S", "All cameras sleep", "tinyctl -c all sleep")
