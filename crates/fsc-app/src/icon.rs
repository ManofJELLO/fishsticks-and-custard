//! The tray icon, drawn on a 32-unit grid and rasterised at whatever
//! sizes the tray asks for. Two shapes: a police box (default) or a
//! fishbone at 45°.
//!
//! - white on dark themes, black on light themes
//! - recording: a red dot in the bottom-right corner, cut out of the shape
//! - muted: dimmed

use tiny_skia::{
    BlendMode, FillRule, LineCap, LineJoin, Paint, PathBuilder, Pixmap, Rect, Stroke, Transform,
};

use fsc_core::Phase;
use fsc_core::config::TrayIcon;

use crate::style::{Rgb, ThemeMode};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct IconState {
    pub phase: Phase,
    pub theme: ThemeMode,
    pub muted: bool,
    pub accent: Rgb,
    pub shape: TrayIcon,
}

pub const SIZES: [u32; 6] = [16, 22, 24, 32, 48, 64];

/// Recording dot, in grid units.
const DOT_C: (f32, f32) = (26.0, 26.0);
const DOT_R: f32 = 4.6;
/// Clear gap kept around the dot so it reads as separate from the shape.
const DOT_GAP: f32 = 1.8;

pub fn render(size: u32, st: &IconState) -> Pixmap {
    let mut pm = Pixmap::new(size, size).expect("non-zero icon size");
    let s = size as f32 / 32.0;
    let t = Transform::from_scale(s, s);
    let recording = st.phase == Phase::Listening;

    let fg = match st.theme {
        ThemeMode::Dark => [255u8, 255, 255],
        ThemeMode::Light => [18, 18, 18],
    };
    let alpha: u8 = if st.muted { 110 } else { 255 };
    let mut paint = Paint::default();
    paint.set_color_rgba8(fg[0], fg[1], fg[2], alpha);
    paint.anti_alias = true;

    match st.shape {
        TrayIcon::PoliceBox => police_box(&mut pm, &paint, t),
        TrayIcon::Fishbone => fishbone(&mut pm, &paint, t, size),
    }

    if recording {
        // Knock a hole out of the shape, then put the dot in it.
        let clear = Paint {
            blend_mode: BlendMode::Clear,
            anti_alias: true,
            ..Default::default()
        };
        fill_circle(&mut pm, &clear, t, DOT_C, DOT_R + DOT_GAP);
        let mut red = Paint::default();
        let a = st.accent.bytes();
        red.set_color_rgba8(a[0], a[1], a[2], 255);
        red.anti_alias = true;
        fill_circle(&mut pm, &red, t, DOT_C, DOT_R);
    }
    pm
}

fn fill_circle(pm: &mut Pixmap, paint: &Paint, t: Transform, c: (f32, f32), r: f32) {
    if let Some(path) = PathBuilder::from_circle(c.0, c.1, r) {
        pm.fill_path(&path, paint, FillRule::Winding, t, None);
    }
}

/// A plain police-box silhouette: lamp, stepped roof, sign band, two door
/// windows, a centre seam and a plinth. Openings are cut out (even-odd).
fn police_box(pm: &mut Pixmap, paint: &Paint, t: Transform) {
    let mut pb = PathBuilder::new();
    // Outline, clockwise from the lamp.
    for (i, &(x, y)) in [
        (14.6, 1.5),
        (17.4, 1.5),
        (17.4, 4.5),
        (21.5, 4.5),
        (21.5, 6.2),
        (23.0, 6.2),
        (23.0, 8.0),
        (22.3, 8.0),
        (22.3, 28.0),
        (23.4, 28.0),
        (23.4, 30.5),
        (8.6, 30.5),
        (8.6, 28.0),
        (9.7, 28.0),
        (9.7, 8.0),
        (9.0, 8.0),
        (9.0, 6.2),
        (10.5, 6.2),
        (10.5, 4.5),
        (14.6, 4.5),
    ]
    .iter()
    .enumerate()
    {
        if i == 0 {
            pb.move_to(x, y);
        } else {
            pb.line_to(x, y);
        }
    }
    pb.close();
    // Openings: the sign slit, a window per door, and the seam between doors.
    for r in [
        Rect::from_ltrb(11.2, 9.3, 20.8, 10.9),
        Rect::from_ltrb(11.2, 12.6, 15.2, 16.8),
        Rect::from_ltrb(16.8, 12.6, 20.8, 16.8),
        Rect::from_ltrb(15.4, 18.4, 16.6, 28.0),
    ]
    .into_iter()
    .flatten()
    {
        pb.push_rect(r);
    }
    // Drawn a touch narrow; widen it around the centre line.
    let t = t.pre_concat(
        Transform::from_translate(16.0, 0.0)
            .pre_scale(1.15, 1.0)
            .pre_translate(-16.0, 0.0),
    );
    if let Some(path) = pb.finish() {
        pm.fill_path(&path, paint, FillRule::EvenOdd, t, None);
    }
}

/// A minimal fishbone, head down-left and tail up-right.
fn fishbone(pm: &mut Pixmap, paint: &Paint, t: Transform, size: u32) {
    // Drawn horizontally (head left), then turned 45° and shrunk a little so
    // the diagonal fits, leaving the bottom-right corner for the dot.
    let t = t
        .pre_concat(Transform::from_rotate_at(-45.0, 16.0, 16.0))
        .pre_concat(
            Transform::from_translate(16.0, 16.0)
                .pre_scale(0.94, 0.94)
                .pre_translate(-16.0, -16.0),
        );

    let mut head = PathBuilder::new();
    head.move_to(3.5, 16.0);
    head.quad_to(6.5, 10.5, 11.0, 9.5);
    head.line_to(11.0, 22.5);
    head.quad_to(6.5, 21.5, 3.5, 16.0);
    head.close();
    if let Some(path) = head.finish() {
        pm.fill_path(&path, paint, FillRule::Winding, t, None);
    }

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
        pm.stroke_path(&path, paint, &stroke, t, None);
    }
}

/// StatusNotifierItem wants ARGB32 in network byte order, non-premultiplied.
pub fn to_sni(pm: &Pixmap) -> ksni::Icon {
    let mut data = Vec::with_capacity(pm.pixels().len() * 4);
    for p in pm.pixels() {
        let c = p.demultiply();
        data.extend_from_slice(&[c.alpha(), c.red(), c.green(), c.blue()]);
    }
    ksni::Icon {
        width: pm.width() as i32,
        height: pm.height() as i32,
        data,
    }
}

pub fn sni_icons(st: &IconState) -> Vec<ksni::Icon> {
    SIZES.iter().map(|&s| to_sni(&render(s, st))).collect()
}
