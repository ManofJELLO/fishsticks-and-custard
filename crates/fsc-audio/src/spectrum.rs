//! Turns the most recent audio into log-spaced band levels (0..1) for the
//! dot-matrix visualiser.

use rustfft::num_complex::Complex32;
use rustfft::{Fft, FftPlanner};
use std::sync::Arc;

pub const BANDS: usize = 48;
const N: usize = 1024;
const F_LO: f32 = 90.0;
const F_HI: f32 = 7500.0;
/// Visible dynamic range below the running peak.
const RANGE_DB: f32 = 60.0;
/// Headroom kept above the running peak so loud frames don't pin to the top.
const HEADROOM_DB: f32 = 2.0;
/// How fast the running peak falls back after loud passages.
const RELEASE_DB_PER_SEC: f32 = 9.0;
/// Never auto-gain quieter than this (so silence stays dark).
const MIN_REF_DB: f32 = -48.0;

pub struct Analyzer {
    fft: Arc<dyn Fft<f32>>,
    window: Vec<f32>,
    buf: Vec<Complex32>,
    /// Running peak (dBFS) for auto-gain; mic and TTS differ by ~30 dB.
    ref_db: f32,
    last: Option<std::time::Instant>,
}

impl Default for Analyzer {
    fn default() -> Self {
        let fft = FftPlanner::new().plan_fft_forward(N);
        let window = (0..N)
            .map(|i| 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / (N - 1) as f32).cos())
            .collect();
        Self { fft, window, buf: vec![Complex32::default(); N], ref_db: MIN_REF_DB, last: None }
    }
}

impl Analyzer {
    /// Forget the auto-gain level (call when switching between mic and speech).
    pub fn reset(&mut self) {
        self.ref_db = MIN_REF_DB;
        self.last = None;
    }

    /// `recent`: the latest samples (any length; the last N are used).
    pub fn analyze(&mut self, recent: &[f32], rate: u32) -> Vec<f32> {
        let start = recent.len().saturating_sub(N);
        let src = &recent[start..];
        for (i, c) in self.buf.iter_mut().enumerate() {
            let s = src.get(i).copied().unwrap_or(0.0);
            *c = Complex32::new(s * self.window[i], 0.0);
        }
        self.fft.process(&mut self.buf);

        let bin_hz = rate as f32 / N as f32;
        let hi = F_HI.min(rate as f32 / 2.0 - bin_hz);
        let ratio = (hi / F_LO).powf(1.0 / BANDS as f32);
        let norm = 2.0 / (N as f32 * 0.5); // window coherent gain ≈ 0.5

        let dbs: Vec<f32> = (0..BANDS)
            .map(|b| {
                let f0 = F_LO * ratio.powi(b as i32);
                let f1 = f0 * ratio;
                let i0 = ((f0 / bin_hz).floor() as usize).max(1);
                let i1 = ((f1 / bin_hz).ceil() as usize).max(i0 + 1).min(N / 2);
                let peak = self.buf[i0..i1].iter().map(|c| c.norm()).fold(0.0f32, f32::max) * norm;
                20.0 * peak.max(1e-9).log10()
            })
            .collect();

        // Auto-gain: follow the loudest band up instantly, release slowly.
        let now = std::time::Instant::now();
        let dt = self.last.map(|t| now.duration_since(t).as_secs_f32()).unwrap_or(0.0).min(0.5);
        self.last = Some(now);
        let frame_max = dbs.iter().copied().fold(f32::MIN, f32::max);
        self.ref_db = (self.ref_db - RELEASE_DB_PER_SEC * dt).max(frame_max).max(MIN_REF_DB);
        let top = self.ref_db + HEADROOM_DB;
        dbs.iter().map(|db| ((db - (top - RANGE_DB)) / RANGE_DB).clamp(0.0, 1.0)).collect()
    }
}
