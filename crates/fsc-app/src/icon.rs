//! The tray icon, drawn on a 32-unit grid and rasterised at whatever
//! sizes the tray asks for. Three shapes: a police box (the default on
//! Linux and Windows), a fishbone at 45°, or a fez (the default on macOS).
//!
//! - police box: blue with white lamp and windows, on any theme
//! - fishbone and fez: white on dark themes, black on light themes (the
//!   macOS menu bar tints them itself, from their shape alone)
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

/// The sizes a StatusNotifierItem is offered (Linux).
#[cfg(target_os = "linux")]
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
    let alpha: u8 = if st.muted { 150 } else { 255 };
    let mut paint = Paint::default();
    paint.set_color_rgba8(fg[0], fg[1], fg[2], alpha);
    paint.anti_alias = true;

    match st.shape {
        TrayIcon::PoliceBox => police_box(&mut pm, alpha, t, size),
        TrayIcon::Fishbone => fishbone(&mut pm, &paint, t, size),
        TrayIcon::Fez => fez(&mut pm, &paint, t, size),
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

/// Police-box colours: blue body, white lamp and windows, dark sign band.
const BOX_BLUE: [u8; 3] = [0x2c, 0x5a, 0xa0];
const BOX_DARK: [u8; 3] = [0x12, 0x26, 0x4a];
const BOX_WHITE: [u8; 3] = [0xf4, 0xf6, 0xfa];

/// A police box in blue with white lamp and windows: lamp, stepped roof,
/// dark sign band, two door windows, a centre seam and a plinth. Its own
/// colours read on light and dark bars alike, so it ignores the theme.
fn police_box(pm: &mut Pixmap, alpha: u8, t: Transform, size: u32) {
    let paint = |rgb: [u8; 3]| {
        let mut p = Paint::default();
        p.set_color_rgba8(rgb[0], rgb[1], rgb[2], alpha);
        p.anti_alias = true;
        p
    };
    let fill_rects = |pm: &mut Pixmap, rgb: [u8; 3], rects: &[(f32, f32, f32, f32)]| {
        let mut pb = PathBuilder::new();
        for r in rects.iter().filter_map(|&(l, t, r, b)| Rect::from_ltrb(l, t, r, b)) {
            pb.push_rect(r);
        }
        if let Some(path) = pb.finish() {
            pm.fill_path(&path, &paint(rgb), FillRule::Winding, t, None);
        }
    };

    // Lamp (white) on a small blue cap.
    fill_rects(pm, BOX_WHITE, &[(14.4, 1.0, 17.6, 3.8)]);
    // Roof tiers, body and plinth: one solid blue block, wider than tall-and-thin.
    fill_rects(
        pm,
        BOX_BLUE,
        &[
            (13.6, 3.6, 18.4, 4.8), // lamp cap
            (9.6, 4.6, 22.4, 6.4),  // upper roof
            (7.8, 6.2, 24.2, 8.2),  // lower roof
            (8.8, 8.0, 23.2, 28.4), // body
            (7.2, 28.2, 24.8, 31.0), // plinth
        ],
    );
    // Sign band across the top and the seam between the doors.
    fill_rects(pm, BOX_DARK, &[(9.8, 9.0, 22.2, 11.0), (15.4, 18.2, 16.6, 27.6)]);
    // Door windows; split into panes once there are enough pixels for it.
    let windows = [(10.4, 12.4, 15.0, 16.8), (17.0, 12.4, 21.6, 16.8)];
    fill_rects(pm, BOX_WHITE, &windows);
    if size >= 32 {
        let mut bars = Vec::new();
        for (l, t, r, b) in windows {
            let (cx, cy) = ((l + r) / 2.0, (t + b) / 2.0);
            bars.push((cx - 0.4, t, cx + 0.4, b));
            bars.push((l, cy - 0.4, r, cy + 0.4));
        }
        fill_rects(pm, BOX_BLUE, &bars);
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

/// A fez, its tassel hanging off the left (the bottom-right corner is the
/// recording dot's). Drawn by its outline alone, with a band cut out above
/// the brim, so it reads when the menu bar keeps only its shape.
fn fez(pm: &mut Pixmap, paint: &Paint, t: Transform, size: u32) {
    // The body: a cone cut flat, its top slightly domed, its brim curved.
    let mut body = PathBuilder::new();
    body.move_to(11.0, 9.6);
    body.quad_to(17.0, 8.2, 23.0, 9.6);
    body.line_to(26.2, 26.6);
    body.quad_to(16.6, 29.4, 7.0, 26.6);
    body.close();
    if let Some(path) = body.finish() {
        pm.fill_path(&path, paint, FillRule::Winding, t, None);
    }

    // The band above the brim, cut out of the body.
    let clear = Paint {
        blend_mode: BlendMode::Clear,
        anti_alias: true,
        ..Default::default()
    };
    let mut band = PathBuilder::new();
    band.move_to(5.0, 21.4);
    band.quad_to(16.6, 24.4, 28.0, 21.4);
    let cut = Stroke {
        width: if size <= 22 { 1.9 } else { 1.5 },
        ..Default::default()
    };
    if let Some(path) = band.finish() {
        pm.stroke_path(&path, &clear, &cut, t, None);
    }

    // The tassel: a cord from the button on top, over to the left and down,
    // ending in a tuft.
    let mut cord = PathBuilder::new();
    cord.move_to(17.0, 8.6);
    cord.quad_to(9.0, 3.0, 5.6, 9.4);
    cord.line_to(5.0, 14.6);
    let stroke = Stroke {
        width: if size <= 22 { 2.0 } else { 1.6 },
        line_cap: LineCap::Round,
        line_join: LineJoin::Round,
        ..Default::default()
    };
    if let Some(path) = cord.finish() {
        pm.stroke_path(&path, paint, &stroke, t, None);
    }
    fill_circle(pm, paint, t, (17.0, 8.4), 1.7);
    let mut tuft = PathBuilder::new();
    tuft.move_to(5.0, 13.4);
    tuft.line_to(7.0, 20.2);
    tuft.quad_to(5.0, 21.2, 3.0, 20.2);
    tuft.close();
    if let Some(path) = tuft.finish() {
        pm.fill_path(&path, paint, FillRule::Winding, t, None);
    }
}

/// StatusNotifierItem wants ARGB32 in network byte order, non-premultiplied.
#[cfg(target_os = "linux")]
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

#[cfg(target_os = "linux")]
pub fn sni_icons(st: &IconState) -> Vec<ksni::Icon> {
    SIZES.iter().map(|&s| to_sni(&render(s, st))).collect()
}
