//! The popup's translucent panel.
//!
//! Opacity follows the content: most see-through behind the visualiser
//! (graphics), most solid behind the conversation and input (text), with
//! smooth fades between. The caller passes where those regions are.

use gtk::cairo::{Context, LinearGradient};
use std::f64::consts::{FRAC_PI_2, PI};

use crate::style::{Palette, ThemeMode};

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

struct Alphas {
    header: f64,
    graphic: f64,
    text: f64,
}

fn alphas(mode: ThemeMode) -> Alphas {
    match mode {
        ThemeMode::Dark => Alphas { header: 0.58, graphic: 0.32, text: 0.86 },
        ThemeMode::Light => Alphas { header: 0.62, graphic: 0.40, text: 0.88 },
    }
}

pub fn rounded_rect(cr: &Context, x: f64, y: f64, w: f64, h: f64, r: f64) {
    let r = r.min(w / 2.0).min(h / 2.0);
    cr.new_sub_path();
    cr.arc(x + w - r, y + r, r, -FRAC_PI_2, 0.0);
    cr.arc(x + w - r, y + h - r, r, 0.0, FRAC_PI_2);
    cr.arc(x + r, y + h - r, r, FRAC_PI_2, PI);
    cr.arc(x + r, y + r, r, PI, 3.0 * FRAC_PI_2);
    cr.close_path();
}

pub fn draw(cr: &Context, w: f64, h: f64, p: &Palette, r: Regions) {
    if w < 2.0 || h < 2.0 {
        return;
    }
    let a = alphas(p.mode);
    let bg = p.bg;

    // (y, alpha) stops, kept monotonic in y.
    let mid = |x: f64, y: f64| (x + y) / 2.0;
    let head_to_gfx = mid(r.header_bottom, r.graphic_top);
    let mut stops = vec![
        (0.0, a.header),
        (head_to_gfx - FADE / 2.0, a.header),
        (head_to_gfx + FADE / 2.0, a.graphic),
        (r.graphic_bottom - FADE / 2.0, a.graphic),
        (r.text_top.max(r.graphic_bottom + FADE / 2.0), a.text),
        (h, a.text),
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

    // Hairline edge.
    rounded_rect(cr, 0.5, 0.5, w - 1.0, h - 1.0, RADIUS);
    cr.set_source_rgba(p.fg.0, p.fg.1, p.fg.2, 0.14);
    cr.set_line_width(1.0);
    cr.stroke().ok();
}
