//! Fluted (reeded) glass panel.
//!
//! A Wayland client can't read the pixels behind its own window, so real
//! refraction isn't possible from inside the app. Instead:
//!   1. Hyprland blurs whatever is behind the layer (a `layer_rule` with blur),
//!   2. we lay a translucent tint over it, and
//!   3. draw vertical "reeds": each one a highlight on its left edge, clear
//!      in the middle and shaded on its right edge.
//!
//! Over the compositor blur this reads as ribbed glass. Without blur it
//! still looks like frosted, ribbed acrylic.

use gtk::cairo::{Context, LinearGradient};
use std::f64::consts::{FRAC_PI_2, PI};

use crate::style::{Palette, ThemeMode};

pub const RADIUS: f64 = 26.0;
const REED: f64 = 11.0;

pub fn rounded_rect(cr: &Context, x: f64, y: f64, w: f64, h: f64, r: f64) {
    let r = r.min(w / 2.0).min(h / 2.0);
    cr.new_sub_path();
    cr.arc(x + w - r, y + r, r, -FRAC_PI_2, 0.0);
    cr.arc(x + w - r, y + h - r, r, 0.0, FRAC_PI_2);
    cr.arc(x + r, y + h - r, r, FRAC_PI_2, PI);
    cr.arc(x + r, y + r, r, PI, 3.0 * FRAC_PI_2);
    cr.close_path();
}

pub fn draw(cr: &Context, w: f64, h: f64, p: &Palette) {
    if w < 2.0 || h < 2.0 {
        return;
    }
    let (hi, lo) = match p.mode {
        ThemeMode::Dark => ((1.0, 0.085), (0.0, 0.26)),
        ThemeMode::Light => ((1.0, 0.45), (0.0, 0.06)),
    };

    cr.save().ok();
    rounded_rect(cr, 0.5, 0.5, w - 1.0, h - 1.0, RADIUS);
    cr.clip();

    // Tint.
    cr.set_source_rgba(p.bg.0, p.bg.1, p.bg.2, p.bg_alpha);
    cr.paint().ok();

    // Reeds.
    let mut x = -(REED - (w % REED)) / 2.0;
    while x < w {
        let g = LinearGradient::new(x, 0.0, x + REED, 0.0);
        g.add_color_stop_rgba(0.00, hi.0, hi.0, hi.0, hi.1);
        g.add_color_stop_rgba(0.14, hi.0, hi.0, hi.0, hi.1 * 0.25);
        g.add_color_stop_rgba(0.45, 0.5, 0.5, 0.5, 0.0);
        g.add_color_stop_rgba(0.80, lo.0, lo.0, lo.0, lo.1 * 0.35);
        g.add_color_stop_rgba(1.00, lo.0, lo.0, lo.0, lo.1);
        cr.set_source(&g).ok();
        cr.rectangle(x, 0.0, REED, h);
        cr.fill().ok();
        x += REED;
    }

    // Soft top sheen so it reads as a glass slab rather than a flat stripe pattern.
    let sheen = LinearGradient::new(0.0, 0.0, 0.0, h);
    sheen.add_color_stop_rgba(0.0, 1.0, 1.0, 1.0, if p.mode == ThemeMode::Dark { 0.06 } else { 0.25 });
    sheen.add_color_stop_rgba(0.35, 1.0, 1.0, 1.0, 0.0);
    cr.set_source(&sheen).ok();
    cr.paint().ok();
    cr.restore().ok();

    // Hairline edge.
    rounded_rect(cr, 0.5, 0.5, w - 1.0, h - 1.0, RADIUS);
    cr.set_source_rgba(p.fg.0, p.fg.1, p.fg.2, 0.16);
    cr.set_line_width(1.0);
    cr.stroke().ok();
}
