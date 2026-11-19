//! The popup's drawn parts: the panel behind everything, the dot-matrix
//! status line and the visualiser. Each is rasterised here with tiny-skia
//! into a `Frame`, which the Qt side shows as is (`qt::Raster`), so the
//! drawing is the same on every platform and testable without a display.

pub mod dotfont;
pub mod panel;
pub mod viz;

use std::sync::Mutex;
use tiny_skia::{FillRule, Paint, Path, PathBuilder, Pixmap, Shader, Stroke, Transform};

use crate::style::Rgb;

/// Which drawn part a frame is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Part {
    Panel = 0,
    Status = 1,
    Viz = 2,
}

impl Part {
    pub fn from_name(name: &str) -> Option<Part> {
        match name {
            "panel" => Some(Part::Panel),
            "status" => Some(Part::Status),
            "viz" => Some(Part::Viz),
            _ => None,
        }
    }
}

/// A finished picture: premultiplied RGBA, `w` x `h` device pixels, shown
/// at `logical` size.
pub struct Frame {
    pub data: Vec<u8>,
    pub w: u32,
    pub h: u32,
    pub logical: (f64, f64),
}

/// The latest frame of each part. Drawn on the UI thread, painted by Qt's
/// render thread (while the UI thread waits for it).
static FRAMES: Mutex<[Option<Frame>; 3]> = Mutex::new([None, None, None]);

pub fn put(part: Part, frame: Frame) {
    FRAMES.lock().unwrap()[part as usize] = Some(frame);
}

/// Run `f` on `part`'s latest frame, if there is one.
pub fn with<R>(part: Part, f: impl FnOnce(&Frame) -> R) -> Option<R> {
    FRAMES.lock().unwrap()[part as usize].as_ref().map(f)
}

/// Something to draw on in logical pixels (`w` x `h`), backed by a pixmap
/// at the screen's scale so it stays sharp on HiDPI screens.
pub struct Canvas {
    pm: Pixmap,
    t: Transform,
    logical: (f64, f64),
}

impl Canvas {
    pub fn new(w: f64, h: f64, scale: f64) -> Option<Canvas> {
        let scale = if scale.is_finite() && scale > 0.0 { scale } else { 1.0 };
        let pw = (w * scale).ceil().max(1.0) as u32;
        let ph = (h * scale).ceil().max(1.0) as u32;
        let pm = Pixmap::new(pw, ph)?;
        Some(Canvas { pm, t: Transform::from_scale(scale as f32, scale as f32), logical: (w, h) })
    }

    pub fn frame(self) -> Frame {
        let (w, h) = (self.pm.width(), self.pm.height());
        Frame { data: self.pm.take(), w, h, logical: self.logical }
    }

    #[cfg(test)]
    pub fn pixmap(&self) -> &Pixmap {
        &self.pm
    }

    pub fn fill(&mut self, path: &Path, c: Rgb, a: f64) {
        self.fill_with(path, solid(c, a));
    }

    pub fn fill_with(&mut self, path: &Path, shader: Shader<'_>) {
        let paint = Paint { shader, anti_alias: true, ..Default::default() };
        self.pm.fill_path(path, &paint, FillRule::Winding, self.t, None);
    }

    pub fn stroke(&mut self, path: &Path, c: Rgb, a: f64, width: f64) {
        let paint = Paint { shader: solid(c, a), anti_alias: true, ..Default::default() };
        let stroke = Stroke { width: width as f32, ..Default::default() };
        self.pm.stroke_path(path, &paint, &stroke, self.t, None);
    }

    pub fn fill_circle(&mut self, x: f64, y: f64, r: f64, c: Rgb, a: f64) {
        if let Some(p) = PathBuilder::from_circle(x as f32, y as f32, r as f32) {
            self.fill(&p, c, a);
        }
    }

    /// Fill the rectangle `(x, y, w, h)` with corners of radius `r`.
    pub fn fill_rounded(&mut self, (x, y, w, h): (f64, f64, f64, f64), r: f64, c: Rgb, a: f64) {
        if let Some(p) = rounded_rect(x, y, w, h, r) {
            self.fill(&p, c, a);
        }
    }

    /// Fill everything with `c` at `a`.
    pub fn paint(&mut self, c: Rgb, a: f64) {
        let [r, g, b] = c.bytes();
        self.pm.fill(tiny_skia::Color::from_rgba8(r, g, b, alpha8(a)));
    }
}

pub fn color(c: Rgb, a: f64) -> tiny_skia::Color {
    tiny_skia::Color::from_rgba(c.0 as f32, c.1 as f32, c.2 as f32, a.clamp(0.0, 1.0) as f32)
        .unwrap_or(tiny_skia::Color::TRANSPARENT)
}

fn solid(c: Rgb, a: f64) -> Shader<'static> {
    Shader::SolidColor(color(c, a))
}

fn alpha8(a: f64) -> u8 {
    (a.clamp(0.0, 1.0) * 255.0).round() as u8
}

/// A rectangle with circular corners of radius `r` (capped at half a side).
pub fn rounded_rect(x: f64, y: f64, w: f64, h: f64, r: f64) -> Option<Path> {
    if w <= 0.0 || h <= 0.0 {
        return None;
    }
    let r = r.min(w / 2.0).min(h / 2.0).max(0.0) as f32;
    let (x, y, w, h) = (x as f32, y as f32, w as f32, h as f32);
    if r <= 0.0 {
        return Some(PathBuilder::from_rect(tiny_skia::Rect::from_xywh(x, y, w, h)?));
    }
    // A quarter circle as one cubic: control points at 0.5523 of the radius.
    let k = r * 0.552_284_8;
    let mut pb = PathBuilder::new();
    pb.move_to(x + r, y);
    pb.line_to(x + w - r, y);
    pb.cubic_to(x + w - r + k, y, x + w, y + r - k, x + w, y + r);
    pb.line_to(x + w, y + h - r);
    pb.cubic_to(x + w, y + h - r + k, x + w - r + k, y + h, x + w - r, y + h);
    pb.line_to(x + r, y + h);
    pb.cubic_to(x + r - k, y + h, x, y + h - r + k, x, y + h - r);
    pb.line_to(x, y + r);
    pb.cubic_to(x, y + r - k, x + r - k, y, x + r, y);
    pb.close();
    pb.finish()
}
