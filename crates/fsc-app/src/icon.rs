//! The tray icon: a minimal fishbone, drawn on a 32-unit grid and
//! rasterised at whatever sizes the tray asks for.
//!
//! - idle: white on dark themes, black on light themes
//! - listening: accent red with a soft glow behind it
//! - muted: dimmed

use tiny_skia::{FillRule, LineCap, LineJoin, Paint, PathBuilder, Pixmap, PremultipliedColorU8, Stroke, Transform};

use fsc_core::Phase;
use fsc_core::config::GlowStyle;

use crate::style::{Rgb, ThemeMode};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct IconState {
    pub phase: Phase,
    pub theme: ThemeMode,
    pub muted: bool,
    pub accent: Rgb,
    pub glow: GlowStyle,
}

pub const SIZES: [u32; 6] = [16, 22, 24, 32, 48, 64];

/// Render to straight-alpha RGBA rows (for PNG previews) plus the pixmap.
pub fn render(size: u32, st: &IconState) -> Pixmap {
    let mut pm = Pixmap::new(size, size).expect("non-zero icon size");
    let s = size as f32 / 32.0;
    let recording = st.phase == Phase::Listening;

    let fg = match st.theme {
        ThemeMode::Dark => [255u8, 255, 255],
        ThemeMode::Light => [18, 18, 18],
    };
    let color = if recording { st.accent.bytes() } else { fg };
    let alpha: u8 = if st.muted && !recording { 110 } else { 255 };

    if recording {
        let glow = match st.glow {
            GlowStyle::Contrast => Some(match st.theme {
                ThemeMode::Dark => ([255, 255, 255], 0.92),
                ThemeMode::Light => ([10, 10, 10], 0.80),
            }),
            GlowStyle::Accent => Some((st.accent.bytes(), 0.45)),
            GlowStyle::None => None,
        };
        if let Some((rgb, strength)) = glow {
            paint_glow(&mut pm, rgb, strength);
        }
    }

    let mut paint = Paint::default();
    paint.set_color_rgba8(color[0], color[1], color[2], alpha);
    paint.anti_alias = true;
    let t = Transform::from_scale(s, s);

    // Head: a solid, slightly rounded wedge pointing left.
    let mut head = PathBuilder::new();
    head.move_to(3.5, 16.0);
    head.quad_to(6.5, 10.5, 11.0, 9.5);
    head.line_to(11.0, 22.5);
    head.quad_to(6.5, 21.5, 3.5, 16.0);
    head.close();
    if let Some(path) = head.finish() {
        pm.fill_path(&path, &paint, FillRule::Winding, t, None);
    }

    // Spine, ribs and tail as round-capped strokes.
    let mut bones = PathBuilder::new();
    bones.move_to(11.0, 16.0);
    bones.line_to(24.5, 16.0);
    for (x, h) in [(14.0, 5.6), (18.0, 4.8), (22.0, 3.6)] {
        bones.move_to(x, 16.0);
        bones.line_to(x + 2.4, 16.0 - h);
        bones.move_to(x, 16.0);
        bones.line_to(x + 2.4, 16.0 + h);
    }
    bones.move_to(24.5, 16.0);
    bones.line_to(28.8, 11.2);
    bones.move_to(24.5, 16.0);
    bones.line_to(28.8, 20.8);
    let stroke = Stroke {
        width: if size <= 16 { 2.8 } else { 2.4 },
        line_cap: LineCap::Round,
        line_join: LineJoin::Round,
        ..Default::default()
    };
    if let Some(path) = bones.finish() {
        pm.stroke_path(&path, &paint, &stroke, t, None);
    }
    pm
}

/// Radial glow, strongest in the middle and fading to nothing at the edge.
fn paint_glow(pm: &mut Pixmap, rgb: [u8; 3], strength: f32) {
    let size = pm.width() as f32;
    let c = (size - 1.0) / 2.0;
    let r = size / 2.0;
    let w = pm.width() as usize;
    for (i, px) in pm.pixels_mut().iter_mut().enumerate() {
        let x = (i % w) as f32 - c;
        let y = (i / w) as f32 - c;
        let d = (x * x + y * y).sqrt() / r;
        let a = strength * (1.0 - smoothstep(0.25, 1.0, d));
        if a <= 0.0 {
            continue;
        }
        let a8 = (a * 255.0).round() as u8;
        let pm8 = |v: u8| ((v as f32) * a).round() as u8;
        if let Some(p) = PremultipliedColorU8::from_rgba(pm8(rgb[0]), pm8(rgb[1]), pm8(rgb[2]), a8) {
            *px = p;
        }
    }
}

fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// StatusNotifierItem wants ARGB32 in network byte order, non-premultiplied.
pub fn to_sni(pm: &Pixmap) -> ksni::Icon {
    let mut data = Vec::with_capacity(pm.pixels().len() * 4);
    for p in pm.pixels() {
        let c = p.demultiply();
        data.extend_from_slice(&[c.alpha(), c.red(), c.green(), c.blue()]);
    }
    ksni::Icon { width: pm.width() as i32, height: pm.height() as i32, data }
}

pub fn sni_icons(st: &IconState) -> Vec<ksni::Icon> {
    SIZES.iter().map(|&s| to_sni(&render(s, st))).collect()
}
