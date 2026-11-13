-- Fishsticks & Custard: paste into ~/.config/hypr/hyprland.lua

-- Blur behind the popup so the fluted-glass reeds read as glass.
-- ignore_alpha keeps blur off the transparent corners.
hl.layer_rule({
    name         = "fsc-glass",
    match        = { namespace = "^fsc$" },
    blur         = true,
    ignore_alpha = 0.05,
})

-- SUPER+A: type to Claude.
hl.bind("SUPER + A", hl.dsp.exec_cmd("fsc type"))
-- SUPER+SHIFT+A: hold to talk. A quick tap also works: it listens until you stop talking.
hl.bind("SUPER + SHIFT + A", hl.dsp.exec_cmd("fsc listen"))
hl.bind("SUPER + SHIFT + A", hl.dsp.exec_cmd("fsc release"), { release = true })

-- Autostart: add this line inside your existing hl.on("hyprland.start", ...) block:
--     hl.exec_cmd("fscd")
