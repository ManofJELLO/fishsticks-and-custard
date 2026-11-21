//! The popup's panel, in the theme's background colour.
//!
//! - window: a plain, full rectangle. Hyprland adds rounding, border,
//!   shadow and opacity through its window rules. On macOS it's drawn like
//!   layer, a tint over the system's glass, which rounds to `RADIUS` too.
//! - layer: rounded with a hairline edge, at `ui.opacity`. With
//!   `ui.gradient` the opacity follows the content: most see-through behind
//!   the visualiser, most solid behind text, with smooth fades between.

use tiny_skia::{GradientStop, LinearGradient, Point, SpreadMode, Transform};

use super::{Canvas, color, rounded_rect};
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

pub fn draw(cr: &mut Canvas, w: f64, h: f64, p: &Palette, r: Regions, look: Look) {
    if w < 2.0 || h < 2.0 {
        return;
    }
    let bg = p.bg;
    let o = look.opacity.clamp(0.0, 1.0);
    if !look.framed {
        cr.paint(bg, o);
        return;
    }
    let Some(shape) = rounded_rect(0.5, 0.5, w - 1.0, h - 1.0, RADIUS) else { return };
    if !look.gradient {
        cr.fill(&shape, bg, o);
        hairline(cr, &shape, p);
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

    let stops = stops.into_iter().map(|(y, alpha)| GradientStop::new((y / h) as f32, color(bg, alpha))).collect();
    let g = LinearGradient::new(Point::from_xy(0.0, 0.0), Point::from_xy(0.0, h as f32), stops, SpreadMode::Pad, Transform::identity());
    match g {
        Some(g) => cr.fill_with(&shape, g),
        None => cr.fill(&shape, bg, o),
    }
    hairline(cr, &shape, p);
}

fn hairline(cr: &mut Canvas, shape: &tiny_skia::Path, p: &Palette) {
    cr.stroke(shape, p.fg, 0.14, 1.0);
}

/// `FSC_VIZ_PNG=dir cargo test -p fsc-app panel_preview -- --ignored`:
/// the layer-mode panel, both themes, as PNGs.
#[cfg(test)]
mod preview {
    use super::*;
    use crate::style::{NOTHING_RED, ThemeMode};

    #[test]
    #[ignore]
    fn panel_preview() {
        let Ok(dir) = std::env::var("FSC_VIZ_PNG") else { return };
        let (w, h) = (680.0, 300.0);
        let regions = Regions { header_bottom: 40.0, graphic_top: 56.0, graphic_bottom: 144.0, text_top: 160.0 };
        for (mode, name) in [(ThemeMode::Dark, "dark"), (ThemeMode::Light, "light")] {
            let mut c = Canvas::new(w, h, 2.0).unwrap();
            let look = Look { framed: true, opacity: 0.85, gradient: true };
            draw(&mut c, w, h, &Palette::new(mode, NOTHING_RED), regions, look);
            c.pixmap().save_png(format!("{dir}/panel-{name}.png")).unwrap();
        }
    }
}
