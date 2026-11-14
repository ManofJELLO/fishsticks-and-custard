//! The visualiser: chunky LED-segment bars rising from the bottom, like an
//! old car-stereo spectrum analyser.
//!
//! Two modes (`ui.visualiser`):
//! - spectrum: one bar per frequency band, low to high
//! - loudness: level over time, newest on the right, scrolling left
//!
//! Each bar is split into blocks (`ui.block_percent` tall). Bars use the
//! accent while listening and the foreground colour otherwise.
//!
//! Real audio arrives through `set_spectrum`: mic input while listening,
//! synthesized speech while speaking. Phases without audio get made-up
//! motion (a sweep while thinking, a babble while text streams in).

use gtk::cairo::Context;
use std::collections::VecDeque;

use fsc_core::Phase;
use fsc_core::config::{UiConfig, Visualiser};

use super::panel::rounded_rect;
use crate::style::Palette;

pub const HEIGHT: i32 = 88;
/// Vertical gap between blocks.
const GAP_Y: f64 = 2.0;
/// Loudness mode: bar width and spacing.
const LOUD_BAR: f64 = 8.0;
const LOUD_PITCH: f64 = 12.0;
/// Below this a bar shows no lit blocks at all.
const FLOOR: f32 = 0.04;

pub struct Viz {
    pub phase: Phase,
    mode: Visualiser,
    bands: usize,
    blocks: usize,
    levels: Vec<f32>,
    /// Latest real band levels (0..1, low to high frequency).
    spectrum: Option<Vec<f32>>,
    /// Loudness mode: recent levels, newest last.
    history: VecDeque<f32>,
    energy: f32,
    t: f32,
    rng: u32,
}

impl Viz {
    pub fn new(cfg: &UiConfig) -> Self {
        Self {
            phase: Phase::Idle,
            mode: cfg.visualiser,
            bands: cfg.bands.clamp(4, 64) as usize,
            blocks: (100.0 / cfg.block_percent.clamp(4, 50) as f32).round() as usize,
            levels: Vec::new(),
            spectrum: None,
            history: VecDeque::new(),
            energy: 0.0,
            t: 0.0,
            rng: 0x9E37_79B9,
        }
    }

    fn columns(&self, width: f64) -> usize {
        match self.mode {
            Visualiser::Spectrum => self.bands,
            Visualiser::Loudness => ((width + LOUD_PITCH - LOUD_BAR) / LOUD_PITCH)
                .floor()
                .max(1.0) as usize,
        }
    }

    fn rand(&mut self) -> f32 {
        // xorshift32
        let mut x = self.rng;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.rng = x;
        (x as f32) / (u32::MAX as f32)
    }

    /// Text/audio arrived: push some energy into the "speaking" animation.
    pub fn kick(&mut self, amount: f32) {
        self.energy = (self.energy + amount).min(1.0);
    }

    /// Feed real FFT band levels (0..1, low to high). `None` reverts to the
    /// built-in animation.
    pub fn set_spectrum(&mut self, bins: Option<Vec<f32>>) {
        match &bins {
            Some(b) if self.mode == Visualiser::Loudness => {
                self.history.push_back(loudness(b));
                // Bounded by the widest popup we'd ever draw.
                while self.history.len() > 512 {
                    self.history.pop_front();
                }
            }
            None => self.history.clear(),
            _ => {}
        }
        self.spectrum = bins;
    }

    fn live(&self) -> bool {
        self.spectrum.is_some() && matches!(self.phase, Phase::Listening | Phase::Speaking)
    }

    pub fn tick(&mut self, dt: f32, width: f64) {
        let n = self.columns(width);
        if self.levels.len() != n {
            self.levels = vec![0.0; n];
        }
        self.t += dt;
        self.energy *= (-dt * 2.2).exp();
        let t = self.t;
        let live = self.live();

        for c in 0..n {
            let x = if n > 1 {
                c as f32 / (n - 1) as f32
            } else {
                0.0
            };
            let target = if live {
                match self.mode {
                    Visualiser::Spectrum => self
                        .spectrum
                        .as_deref()
                        .map(|s| band(s, c, n))
                        .unwrap_or(0.0),
                    Visualiser::Loudness => {
                        // Right-aligned: the newest level is the last column.
                        let back = n - 1 - c;
                        let len = self.history.len();
                        if back < len {
                            self.history[len - 1 - back]
                        } else {
                            0.0
                        }
                    }
                }
            } else {
                match self.phase {
                    Phase::Idle | Phase::Error | Phase::Asking => 0.0,
                    Phase::Listening => {
                        // Speech-ish: energy in the low-mids, at syllable rate.
                        let env = speech_envelope(x, 0.30);
                        let syl = 0.5 + 0.5 * (t * 7.0 + (t * 1.3).sin() * 2.0).sin();
                        let r = self.rand();
                        env * (0.30 + 0.70 * syl) * (0.45 + 0.55 * r)
                    }
                    Phase::Thinking | Phase::Working | Phase::Transcribing => {
                        // A soft blob sweeping left to right.
                        let speed = match self.phase {
                            Phase::Working => 1.4,
                            Phase::Transcribing => 2.0,
                            _ => 0.8,
                        };
                        let pos = (t * speed).fract() * 1.5 - 0.25;
                        let d = (x - pos) / 0.09;
                        0.65 * (-d * d).exp()
                    }
                    Phase::Responding | Phase::Speaking => {
                        let env = speech_envelope(x, 0.36);
                        let r = self.rand();
                        self.energy * env * (0.35 + 0.65 * r)
                    }
                }
            };

            let lvl = &mut self.levels[c];
            if live && self.mode == Visualiser::Loudness {
                // History is already a trace over time; smoothing would smear it.
                *lvl = target;
            } else {
                let rate = if target > *lvl { 24.0 } else { 6.0 };
                *lvl += (target - *lvl) * (1.0 - (-rate * dt).exp());
            }
        }
    }

    pub fn draw(&self, cr: &Context, w: f64, h: f64, p: &Palette) {
        let n = self.columns(w);
        let rows = self.blocks;
        let (bar_w, pitch) = match self.mode {
            Visualiser::Spectrum => {
                let pitch = w / n as f64;
                let gap = (pitch * 0.22).clamp(4.0, 12.0);
                (pitch - gap, pitch)
            }
            Visualiser::Loudness => (LOUD_BAR, LOUD_PITCH),
        };
        let used = (n as f64 - 1.0) * pitch + bar_w;
        let x0 = ((w - used) / 2.0).max(0.0);
        let block_h = ((h - (rows as f64 - 1.0) * GAP_Y) / rows as f64).max(1.0);
        let radius = (block_h / 3.0).min(bar_w / 3.0).min(2.0);
        let on = if matches!(self.phase, Phase::Listening) {
            p.accent
        } else {
            p.fg
        };

        // Waiting on you: just the bottom row, breathing slowly in the accent.
        let asking = self.phase == Phase::Asking;
        let breath = 0.5 - 0.5 * (self.t as f64 * 2.4).cos();

        for c in 0..n {
            let lvl = self.levels.get(c).copied().unwrap_or(0.0);
            let lit = if lvl < FLOOR {
                0
            } else {
                ((lvl * rows as f32).round() as usize).clamp(1, rows)
            };
            let x = x0 + c as f64 * pitch;
            for r in 0..rows {
                let y = h - (r as f64 + 1.0) * block_h - r as f64 * GAP_Y;
                let (col, a) = if asking && r == 0 {
                    (p.accent, 0.30 + 0.60 * breath)
                } else if r < lit {
                    (on, 0.95)
                } else {
                    (p.fg, 0.08)
                };
                rounded_rect(cr, x, y, bar_w, block_h, radius);
                cr.set_source_rgba(col.0, col.1, col.2, a);
                cr.fill().ok();
            }
        }
    }
}

fn speech_envelope(x: f32, centre: f32) -> f32 {
    let d = (x - centre) / 0.28;
    0.85 * (-d * d).exp() + 0.08
}

/// Column `c` of `n`: the loudest of the fine bands that fall in it.
fn band(s: &[f32], c: usize, n: usize) -> f32 {
    if s.is_empty() {
        return 0.0;
    }
    let lo = c * s.len() / n;
    let hi = ((c + 1) * s.len() / n).max(lo + 1).min(s.len());
    s[lo..hi]
        .iter()
        .copied()
        .fold(0.0, f32::max)
        .clamp(0.0, 1.0)
}

/// One loudness value from band levels: the mean of the loudest quarter,
/// so dark bands above the mic's range don't drag it down.
fn loudness(s: &[f32]) -> f32 {
    if s.is_empty() {
        return 0.0;
    }
    let mut v = s.to_vec();
    v.sort_by(|a, b| b.total_cmp(a));
    let k = (v.len() / 4).max(1);
    (v[..k].iter().sum::<f32>() / k as f32).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bands_group_fine_bins() {
        let s: Vec<f32> = (0..48).map(|i| i as f32 / 47.0).collect();
        assert_eq!(band(&s, 0, 16), 2.0 / 47.0);
        assert_eq!(band(&s, 15, 16), 1.0);
        // More columns than bins still reads something for each.
        assert!(band(&s[..4], 7, 16) > 0.0);
    }

    #[test]
    fn loudness_ignores_dark_bands() {
        let mut s = vec![0.0; 48];
        for v in s.iter_mut().take(12) {
            *v = 0.8;
        }
        assert!((loudness(&s) - 0.8).abs() < 1e-6);
    }
}

/// `FSC_VIZ_PNG=dir cargo test -p fsc-app viz_preview -- --ignored`: render
/// a few frames to raw BGRA files for a look without running the app.
#[cfg(test)]
mod preview {
    use super::*;
    use crate::style::{NOTHING_RED, Palette, ThemeMode};
    use gtk::cairo::{Format, ImageSurface};

    #[test]
    #[ignore]
    fn viz_preview() {
        let Ok(dir) = std::env::var("FSC_VIZ_PNG") else {
            return;
        };
        let (w, h) = (632.0, HEIGHT as f64);
        let speechy: Vec<f32> = (0..48)
            .map(|i| {
                let x = i as f32 / 47.0;
                (0.9 * (-((x - 0.3) / 0.25).powi(2)).exp() + 0.05 * (i % 3) as f32).min(1.0)
            })
            .collect();
        for (mode, name) in [
            (Visualiser::Spectrum, "spectrum"),
            (Visualiser::Loudness, "loudness"),
        ] {
            for (phase, pname) in [
                (Phase::Listening, "listening"),
                (Phase::Speaking, "speaking"),
                (Phase::Asking, "asking"),
            ] {
                let cfg = UiConfig {
                    visualiser: mode,
                    ..Default::default()
                };
                let mut v = Viz::new(&cfg);
                v.phase = phase;
                for i in 0..120 {
                    let k = 0.5 + 0.5 * (i as f32 * 0.4).sin();
                    v.set_spectrum(Some(speechy.iter().map(|s| s * k).collect()));
                    v.tick(1.0 / 30.0, w);
                }
                let surf =
                    ImageSurface::create(Format::ARgb32, w as i32 + 48, h as i32 + 48).unwrap();
                let cr = Context::new(&surf).unwrap();
                let p = Palette::new(ThemeMode::Dark, NOTHING_RED);
                cr.set_source_rgb(0.08, 0.08, 0.08);
                cr.paint().unwrap();
                cr.translate(24.0, 24.0);
                v.draw(&cr, w, h, &p);
                drop(cr);
                // Raw BGRA; convert with `magick -size WxH -depth 8 bgra:FILE out.png`.
                let (sw, sh) = (surf.width(), surf.height());
                let mut surf = surf;
                let data = surf.data().unwrap().to_vec();
                std::fs::write(format!("{dir}/{name}-{pname}-{sw}x{sh}.bgra"), data).unwrap();
            }
        }
    }
}
