//! Dot-matrix spectrum visualiser.
//!
//! Each column is one frequency band, mirrored above and below a centre
//! baseline. Peaks hold briefly in the accent colour.
//!
//! Real FFT band levels arrive through `set_spectrum`: mic input while
//! listening, synthesized speech while speaking. Phases without audio get
//! made-up motion (a sweep while thinking, a babble while text streams in).

use gtk::cairo::Context;
use std::f64::consts::TAU;

use fsc_core::Phase;

use crate::style::Palette;

pub const ROWS: usize = 11;
pub const PITCH: f64 = 8.0;
const DOT: f64 = 2.3;
pub const HEIGHT: i32 = (ROWS as f64 * PITCH) as i32;

pub struct Viz {
    pub phase: Phase,
    levels: Vec<f32>,
    peaks: Vec<f32>,
    hold: Vec<f32>,
    /// Real magnitudes (0..1, low to high frequency) when audio is wired up.
    spectrum: Option<Vec<f32>>,
    energy: f32,
    t: f32,
    rng: u32,
}

impl Default for Viz {
    fn default() -> Self {
        Self {
            phase: Phase::Idle,
            levels: Vec::new(),
            peaks: Vec::new(),
            hold: Vec::new(),
            spectrum: None,
            energy: 0.0,
            t: 0.0,
            rng: 0x9E37_79B9,
        }
    }
}

impl Viz {
    pub fn columns_for(width: f64) -> usize {
        (width / PITCH).floor().max(1.0) as usize
    }

    fn ensure(&mut self, n: usize) {
        if self.levels.len() != n {
            self.levels = vec![0.0; n];
            self.peaks = vec![0.0; n];
            self.hold = vec![0.0; n];
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

    /// Feed real FFT magnitudes (0..1, low to high). `None` reverts to the
    /// built-in animation.
    pub fn set_spectrum(&mut self, bins: Option<Vec<f32>>) {
        self.spectrum = bins;
    }

    pub fn tick(&mut self, dt: f32, columns: usize) {
        self.ensure(columns);
        self.t += dt;
        self.energy *= (-dt * 2.2).exp();
        let n = columns;
        let t = self.t;

        for c in 0..n {
            let x = if n > 1 { c as f32 / (n - 1) as f32 } else { 0.0 };
            let target = if let (Some(s), Phase::Listening | Phase::Speaking) = (&self.spectrum, self.phase) {
                sample(s, x)
            } else {
                match self.phase {
                    Phase::Idle | Phase::Error => 0.0,
                    Phase::Listening => {
                        // Speech-ish: energy concentrated in the low-mids,
                        // modulated at syllable rate.
                        let env = speech_envelope(x, 0.30);
                        let syl = 0.5 + 0.5 * (t * 7.0 + (t * 1.3).sin() * 2.0).sin();
                        let n = self.rand();
                        env * (0.30 + 0.70 * syl) * (0.45 + 0.55 * n)
                    }
                    Phase::Thinking | Phase::Working | Phase::Transcribing => {
                        // A soft blob sweeping left to right.
                        let speed = match self.phase {
                            Phase::Working => 1.4,
                            Phase::Transcribing => 2.0,
                            _ => 0.8,
                        };
                        let pos = (t * speed).fract() * 1.5 - 0.25;
                        let d = (x - pos) / 0.07;
                        0.70 * (-d * d).exp() + 0.05
                    }
                    Phase::Responding | Phase::Speaking => {
                        let env = speech_envelope(x, 0.36);
                        let n = self.rand();
                        self.energy * env * (0.35 + 0.65 * n)
                    }
                }
            };

            let lvl = &mut self.levels[c];
            let rate = if target > *lvl { 22.0 } else { 7.0 };
            *lvl += (target - *lvl) * (1.0 - (-rate * dt).exp());

            if *lvl >= self.peaks[c] {
                self.peaks[c] = *lvl;
                self.hold[c] = 0.35;
            } else if self.hold[c] > 0.0 {
                self.hold[c] -= dt;
            } else {
                self.peaks[c] = (self.peaks[c] - dt * 0.7).max(*lvl);
            }
        }
    }

    pub fn draw(&self, cr: &Context, w: f64, h: f64, p: &Palette) {
        let n = Viz::columns_for(w);
        let half = (ROWS / 2) as i32;
        let x0 = (w - n as f64 * PITCH) / 2.0 + PITCH / 2.0;
        let cy = h / 2.0;
        let bar = if self.phase == Phase::Listening { p.accent } else { p.fg };

        for c in 0..n {
            let lvl = self.levels.get(c).copied().unwrap_or(0.0);
            let peak = self.peaks.get(c).copied().unwrap_or(0.0);
            let lit = (lvl * (half as f32 + 1.0)).floor() as i32;
            let peak_row = (peak * (half as f32 + 1.0)).floor() as i32;
            let x = x0 + c as f64 * PITCH;

            for d in -half..=half {
                let dist = d.abs();
                let y = cy + d as f64 * PITCH;
                let (col, a) = if dist < lit {
                    (bar, 1.0 - dist as f64 * 0.07)
                } else if peak > 0.04 && dist == peak_row && dist >= lit && dist <= half {
                    (p.accent, 0.95)
                } else if dist == 0 {
                    (p.fg, 0.22)
                } else {
                    (p.fg, 0.10)
                };
                if a > 0.5 {
                    // Halo in the panel colour keeps lit dots readable over
                    // whatever shows through the translucent panel.
                    cr.set_source_rgba(p.bg.0, p.bg.1, p.bg.2, 0.55);
                    cr.arc(x, y, DOT + 1.6, 0.0, TAU);
                    cr.fill().ok();
                }
                cr.set_source_rgba(col.0, col.1, col.2, a);
                cr.arc(x, y, DOT, 0.0, TAU);
                cr.fill().ok();
            }
        }
    }
}

fn speech_envelope(x: f32, centre: f32) -> f32 {
    let d = (x - centre) / 0.28;
    0.85 * (-d * d).exp() + 0.12
}

/// Linear-interpolated lookup into `s` at position `x` (0..1).
fn sample(s: &[f32], x: f32) -> f32 {
    if s.is_empty() {
        return 0.0;
    }
    let f = x * (s.len() - 1) as f32;
    let i = f.floor() as usize;
    let j = (i + 1).min(s.len() - 1);
    let k = f - i as f32;
    (s[i] * (1.0 - k) + s[j] * k).clamp(0.0, 1.0)
}
