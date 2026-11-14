//! The popup's panel, in the theme's background colour.
//!
//! - window: a plain, full rectangle. Hyprland adds rounding, border,
//!   shadow and opacity through its window rules.
//! - layer: rounded with a hairline edge, at `ui.opacity`. With
//!   `ui.gradient` the opacity follows the content: most see-through behind
//!   the visualiser, most solid behind text, with smooth fades between.

use gtk::cairo::{Context, LinearGradient};
use std::f64::consts::{FRAC_PI_2, PI};

use crate::style::Palette;

pub const RADIUS: f64 = 26.0;
/// Fade length between regions.
const FADE: f64 = 14.0;

/// Vertical layout of the panel, in panel coordinates.
#[derive(Debug, Clone, Copy)]
pub struct Regions {
    /// Bottom of the header (status + meta text).
    pub header_bottom: f64,
    /// Visualiser top and bottom.
    pub graphic_top: f64,
    pub graphic_bottom: f64,
    /// Where text (conversation or input) begins.
    pub text_top: f64,
}

/// How the panel is painted.
#[derive(Debug, Clone, Copy)]
pub struct Look {
    /// Rounded corners and a hairline edge (layer); a plain rectangle (window).
    pub framed: bool,
    pub opacity: f64,
    pub gradient: bool,
}

/// Gradient opacities relative to `Look::opacity`.
const HEADER: f64 = 0.72;
const GRAPHIC: f64 = 0.45;
const TEXT: f64 = 1.0;

pub fn rounded_rect(cr: &Context, x: f64, y: f64, w: f64, h: f64, r: f64) {
    let r = r.min(w / 2.0).min(h / 2.0);
    cr.new_sub_path();
    cr.arc(x + w - r, y + r, r, -FRAC_PI_2, 0.0);
    cr.arc(x + w - r, y + h - r, r, 0.0, FRAC_PI_2);
    cr.arc(x + r, y + h - r, r, FRAC_PI_2, PI);
    cr.arc(x + r, y + r, r, PI, 3.0 * FRAC_PI_2);
    cr.close_path();
}

pub fn draw(cr: &Context, w: f64, h: f64, p: &Palette, r: Regions, look: Look) {
    if w < 2.0 || h < 2.0 {
        return;
    }
    let bg = p.bg;
    let o = look.opacity.clamp(0.0, 1.0);
    if !look.framed {
        cr.set_source_rgba(bg.0, bg.1, bg.2, o);
        cr.paint().ok();
        return;
    }
    if !look.gradient {
        rounded_rect(cr, 0.5, 0.5, w - 1.0, h - 1.0, RADIUS);
        cr.set_source_rgba(bg.0, bg.1, bg.2, o);
        cr.fill().ok();
        hairline(cr, w, h, p);
        return;
    }

    // (y, alpha) stops, kept monotonic in y.
    let mid = |x: f64, y: f64| (x + y) / 2.0;
    let head_to_gfx = mid(r.header_bottom, r.graphic_top);
    let mut stops = vec![
        (0.0, HEADER * o),
        (head_to_gfx - FADE / 2.0, HEADER * o),
        (head_to_gfx + FADE / 2.0, GRAPHIC * o),
        (r.graphic_bottom - FADE / 2.0, GRAPHIC * o),
        (r.text_top.max(r.graphic_bottom + FADE / 2.0), TEXT * o),
        (h, TEXT * o),
    ];
    let mut last = 0.0;
    for s in stops.iter_mut() {
        s.0 = s.0.clamp(last, h);
        last = s.0;
    }

    let g = LinearGradient::new(0.0, 0.0, 0.0, h);
    for (y, alpha) in stops {
        g.add_color_stop_rgba(y / h, bg.0, bg.1, bg.2, alpha);
    }
    rounded_rect(cr, 0.5, 0.5, w - 1.0, h - 1.0, RADIUS);
    cr.set_source(&g).ok();
    cr.fill().ok();
    hairline(cr, w, h, p);
}

fn hairline(cr: &Context, w: f64, h: f64, p: &Palette) {
    rounded_rect(cr, 0.5, 0.5, w - 1.0, h - 1.0, RADIUS);
    cr.set_source_rgba(p.fg.0, p.fg.1, p.fg.2, 0.14);
    cr.set_line_width(1.0);
    cr.stroke().ok();
}
