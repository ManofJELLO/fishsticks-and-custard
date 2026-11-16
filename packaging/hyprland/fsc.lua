-- Fishsticks & Custard: paste into ~/.config/hypr/hyprland.lua

-- Window mode (ui.surface = "window", the default) -----------------------
-- The popup is a normal window with class dev.fsc.FishsticksAndCustard, so
-- your border, shadow, rounding, blur, opacity and animations all apply.
-- Float it, keep it on every workspace, and put it top-centre.
hl.window_rule({
    name  = "fsc-window",
    match = { class = "^(dev\\.fsc\\.FishsticksAndCustard)$" },

    float = true,
    pin   = true,
    move  = "(monitor_w*0.5-window_w*0.5) 80",
    -- Optional:
    -- opacity   = "0.92 0.85",     -- active / inactive
    -- animation = "popin 85%",
    -- dim_around = true,
})

-- Layer mode (ui.surface = "layer") ---------------------------------------
-- A corner/edge popup placed by ui.anchor / ui.margin. Layers get no border
-- or shadow from Hyprland; transparency comes from ui.opacity / ui.gradient.
-- ignore_alpha keeps blur off the transparent rounded corners.
hl.layer_rule({
    name         = "fsc-layer",
    match        = { namespace = "^fsc$" },
    blur         = true,
    ignore_alpha = 0.05,
    animation    = "slide top",     -- or "popin 85%", "fade"
})

-- Keys ---------------------------------------------------------------------
-- SUPER+A: type a question.
hl.bind("SUPER + A", hl.dsp.exec_cmd("fsc type"))
-- SUPER+SHIFT+A: hold to talk. A quick tap also works: it listens until you stop talking.
hl.bind("SUPER + SHIFT + A", hl.dsp.exec_cmd("fsc listen"))
hl.bind("SUPER + SHIFT + A", hl.dsp.exec_cmd("fsc release"), { release = true })

-- Autostart: add this line inside your existing hl.on("hyprland.start", ...) block:
--     hl.exec_cmd("fscd")
