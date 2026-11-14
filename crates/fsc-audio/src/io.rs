//! Microphone capture and speaker playback via cpal (native PipeWire).
//!
//! We ask PipeWire for exactly the format each stage wants (16 kHz mono in,
//! the voice's rate mono out) and let it resample. If a device refuses, we
//! fall back to its default format and convert here.

use anyhow::{Context, Result, anyhow};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{BufferSize, Device, FromSample, Sample, SampleFormat, SizedSample, Stream, StreamConfig, SupportedStreamConfig};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const MIC_RATE: u32 = 16_000;

fn find_input(host: &cpal::Host, name: &str) -> Option<Device> {
    if !name.is_empty() {
        let needle = name.to_lowercase();
        if let Ok(devs) = host.input_devices() {
            for d in devs {
                if let Ok(desc) = d.description()
                    && desc.name().to_lowercase().contains(&needle)
                {
                    return Some(d);
                }
            }
        }
        eprintln!("fsc: input device matching {name:?} not found; using default");
    }
    host.default_input_device()
}

/// Linear resampler for the fallback path (fine for speech/visuals).
pub(crate) struct Resampler {
    step: f64,
    pos: f64,
    last: f32,
}

impl Resampler {
    pub(crate) fn new(from: u32, to: u32) -> Self {
        Self { step: from as f64 / to as f64, pos: 0.0, last: 0.0 }
    }

    pub(crate) fn push(&mut self, input: &[f32], out: &mut impl Extend<f32>) {
        for &s in input {
            while self.pos < 1.0 {
                out.extend([self.last + (s - self.last) * self.pos as f32]);
                self.pos += self.step;
            }
            self.pos -= 1.0;
            self.last = s;
        }
    }
}

/// Call `$f::<T>(args)` with `T` the Rust type of sample format `$fmt`.
macro_rules! with_sample_type {
    ($fmt:expr, $f:ident($($arg:expr),*)) => {
        match $fmt {
            SampleFormat::I8 => $f::<i8>($($arg),*),
            SampleFormat::I16 => $f::<i16>($($arg),*),
            SampleFormat::I24 => $f::<cpal::I24>($($arg),*),
            SampleFormat::I32 => $f::<i32>($($arg),*),
            SampleFormat::I64 => $f::<i64>($($arg),*),
            SampleFormat::U8 => $f::<u8>($($arg),*),
            SampleFormat::U16 => $f::<u16>($($arg),*),
            SampleFormat::U24 => $f::<cpal::U24>($($arg),*),
            SampleFormat::U32 => $f::<u32>($($arg),*),
            SampleFormat::U64 => $f::<u64>($($arg),*),
            SampleFormat::F32 => $f::<f32>($($arg),*),
            SampleFormat::F64 => $f::<f64>($($arg),*),
            other => Err(anyhow!("unsupported sample format {other:?}")),
        }
    };
}

/// Capture in the device's own format, mixing to mono and resampling here.
fn mic_fallback<T>(
    dev: &Device,
    def: &SupportedStreamConfig,
    sink: Arc<Mutex<Vec<f32>>>,
    on_err: impl FnMut(cpal::Error) + Send + 'static,
) -> Result<Stream>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    let ch = def.channels() as usize;
    let mut rs = Resampler::new(def.sample_rate(), MIC_RATE);
    let mut mono = Vec::new();
    let mut out = Vec::new();
    Ok(dev.build_input_stream::<T, _, _>(
        def.config(),
        move |data: &[T], _| {
            mono.clear();
            mono.extend(data.chunks(ch).map(|f| f.iter().map(|&s| f32::from_sample(s)).sum::<f32>() / ch as f32));
            out.clear();
            rs.push(&mono, &mut out);
            sink.lock().unwrap().extend_from_slice(&out);
        },
        on_err,
        None,
    )?)
}

/// A mic that has delivered nothing for this long is treated as gone. Long
/// enough for a Bluetooth headset's profile switch, during which it still
/// delivers (silent) audio anyway.
const MIC_STALL: Duration = Duration::from_secs(10);

/// An open microphone. Captured 16 kHz mono samples accumulate in `buf`.
pub struct Mic {
    _stream: Stream,
    buf: Arc<Mutex<Vec<f32>>>,
    /// The stream reported an error (e.g. the device was unplugged).
    dead: Arc<AtomicBool>,
    last_data: std::cell::Cell<Instant>,
}

impl Mic {
    pub fn open(device_name: &str) -> Result<Self> {
        let host = cpal::default_host();
        let dev = find_input(&host, device_name).ok_or_else(|| anyhow!("no microphone found"))?;
        let buf: Arc<Mutex<Vec<f32>>> = Arc::default();

        let dead = Arc::new(AtomicBool::new(false));
        let on_err = {
            let dead = dead.clone();
            move |e: cpal::Error| {
                eprintln!("fsc: mic error: {e}");
                dead.store(true, Ordering::Relaxed);
            }
        };

        let wanted = StreamConfig { channels: 1, sample_rate: MIC_RATE, buffer_size: BufferSize::Default };
        let sink = buf.clone();
        let direct = dev.build_input_stream::<f32, _, _>(
            wanted,
            move |data: &[f32], _| sink.lock().unwrap().extend_from_slice(data),
            on_err.clone(),
            None,
        );

        let stream = match direct {
            Ok(s) => s,
            Err(e) => {
                eprintln!("fsc: mic refused 16 kHz mono ({e}); converting locally");
                let def = dev.default_input_config().context("mic default config")?;
                with_sample_type!(def.sample_format(), mic_fallback(&dev, &def, buf.clone(), on_err))?
            }
        };
        stream.play()?;
        Ok(Self { _stream: stream, buf, dead, last_data: std::cell::Cell::new(Instant::now()) })
    }

    /// Take everything captured since the last call.
    pub fn drain(&self) -> Vec<f32> {
        let data = std::mem::take(&mut *self.buf.lock().unwrap());
        if !data.is_empty() {
            self.last_data.set(Instant::now());
        }
        data
    }

    /// The device went away or stopped delivering audio: reopen it.
    pub fn failed(&self) -> bool {
        self.dead.load(Ordering::Relaxed) || self.last_data.get().elapsed() > MIC_STALL
    }
}

#[derive(Default)]
struct PlayState {
    queue: VecDeque<f32>,
    /// Most recently played samples, for the visualiser.
    recent: VecDeque<f32>,
    /// Device latency reported by the backend (callback to speaker).
    latency: Duration,
    /// Wall-clock time the last queued sample handed to the device will
    /// actually be heard. Bluetooth adds a lot here; releasing the mic (and
    /// so switching the headset profile) before this cuts the reply off.
    audible_until: Option<Instant>,
}

impl PlayState {
    /// Bookkeeping after a callback that consumed `real` queued samples.
    fn played(&mut self, info: &cpal::OutputCallbackInfo, real: usize, rate: u32) {
        let ts = info.timestamp();
        self.latency = ts.playback.duration_since(ts.callback);
        if real > 0 {
            self.audible_until = Some(Instant::now() + self.latency + Duration::from_secs_f64(real as f64 / rate as f64));
        }
        let excess = self.recent.len().saturating_sub(RECENT);
        self.recent.drain(..excess);
    }
}

const RECENT: usize = 2048;

/// An open output stream playing mono audio at `rate`.
///
/// If the device disappears mid-playback (e.g. Bluetooth switching
/// profiles), `revive_if_dead` reopens on the current default device and
/// carries on with whatever hadn't been played yet.
pub struct Player {
    stream: Stream,
    st: Arc<Mutex<PlayState>>,
    dead: Arc<AtomicBool>,
    keep_alive: Arc<AtomicBool>,
    pub rate: u32,
}

impl Player {
    pub fn open(rate: u32) -> Result<Self> {
        let st: Arc<Mutex<PlayState>> = Arc::default();
        let dead = Arc::new(AtomicBool::new(false));
        let keep_alive = Arc::new(AtomicBool::new(false));
        let stream = build_output(rate, &st, &dead, &keep_alive)?;
        Ok(Self { stream, st, dead, keep_alive, rate })
    }

    /// Reopen the output if the device went away. Returns true if it did.
    pub fn revive_if_dead(&mut self) -> bool {
        if !self.dead.load(Ordering::Relaxed) {
            return false;
        }
        self.dead.store(false, Ordering::Relaxed);
        match build_output(self.rate, &self.st, &self.dead, &self.keep_alive) {
            Ok(s) => {
                self.stream = s;
                eprintln!("fsc: audio output reopened");
            }
            Err(e) => {
                eprintln!("fsc: could not reopen audio output: {e:#}");
                self.dead.store(true, Ordering::Relaxed);
            }
        }
        true
    }

    /// Between utterances, play inaudible dither (~-80 dBFS) instead of
    /// digital silence. Some Bluetooth earbuds power down their audio path
    /// on exact zeros and clip the start of the next sound while waking.
    pub fn set_keep_alive(&self, on: bool) {
        self.keep_alive.store(on, Ordering::Relaxed);
    }

    pub fn enqueue(&self, pcm: &[f32]) {
        self.st.lock().unwrap().queue.extend(pcm);
    }

    /// Seconds of audio still waiting to be played.
    pub fn queued_secs(&self) -> f32 {
        self.st.lock().unwrap().queue.len() as f32 / self.rate as f32
    }

    pub fn clear(&self) {
        let mut s = self.st.lock().unwrap();
        s.queue.clear();
        s.recent.clear();
        s.audible_until = None;
    }

    /// Audio is still queued or still on its way to the listener's ears.
    pub fn busy(&self, margin: Duration) -> bool {
        let s = self.st.lock().unwrap();
        !s.queue.is_empty() || s.audible_until.is_some_and(|t| Instant::now() < t + margin)
    }

    pub fn latency(&self) -> Duration {
        self.st.lock().unwrap().latency
    }

    pub fn recent(&self) -> Vec<f32> {
        self.st.lock().unwrap().recent.iter().copied().collect()
    }
}

/// Tiny triangular-ish noise for keep-alive; xorshift so the callback stays allocation-free.
fn dither(seed: &mut u32) -> f32 {
    let mut x = *seed;
    x ^= x << 13;
    x ^= x >> 17;
    x ^= x << 5;
    *seed = x;
    ((x as f32 / u32::MAX as f32) - 0.5) * 2.0e-4
}

fn build_output(
    rate: u32,
    st: &Arc<Mutex<PlayState>>,
    dead: &Arc<AtomicBool>,
    keep_alive: &Arc<AtomicBool>,
) -> Result<Stream> {
    let host = cpal::default_host();
    let dev = host.default_output_device().ok_or_else(|| anyhow!("no audio output found"))?;
    let on_err = {
        let dead = dead.clone();
        move |e: cpal::Error| {
            eprintln!("fsc: output error: {e}");
            dead.store(true, Ordering::Relaxed);
        }
    };

    // Stereo with the same signal in both channels: a 1-channel stream gets
    // positioned front-left by PipeWire, i.e. only the left ear.
    let wanted = StreamConfig { channels: 2, sample_rate: rate, buffer_size: BufferSize::Default };
    let shared = st.clone();
    let alive = keep_alive.clone();
    let mut seed = 0x1234_5678u32;
    let direct = dev.build_output_stream::<f32, _, _>(
        wanted,
        move |out: &mut [f32], info: &cpal::OutputCallbackInfo| {
            let mut s = shared.lock().unwrap();
            let keep = alive.load(Ordering::Relaxed);
            let mut real = 0;
            for frame in out.chunks_mut(2) {
                let v = match s.queue.pop_front() {
                    Some(v) => {
                        real += 1;
                        v
                    }
                    None if keep => dither(&mut seed),
                    None => 0.0,
                };
                frame.iter_mut().for_each(|o| *o = v);
                s.recent.push_back(v);
            }
            s.played(info, real, rate);
        },
        on_err.clone(),
        None,
    );

    let stream = match direct {
        Ok(s) => s,
        Err(e) => {
            eprintln!("fsc: output refused {rate} Hz stereo ({e}); converting locally");
            let def = dev.default_output_config().context("output default config")?;
            with_sample_type!(def.sample_format(), output_fallback(&dev, &def, rate, st.clone(), keep_alive.clone(), on_err))?
        }
    };
    stream.play()?;
    Ok(stream)
}

/// Play in the device's own format and rate, converting here.
fn output_fallback<T>(
    dev: &Device,
    def: &SupportedStreamConfig,
    rate: u32,
    shared: Arc<Mutex<PlayState>>,
    alive: Arc<AtomicBool>,
    on_err: impl FnMut(cpal::Error) + Send + 'static,
) -> Result<Stream>
where
    T: SizedSample + FromSample<f32>,
{
    let ch = def.channels() as usize;
    let mut rs = Resampler::new(rate, def.sample_rate());
    // Resampled samples not yet played (at most a few).
    let mut ready = VecDeque::new();
    let mut seed = 0x1234_5678u32;
    Ok(dev.build_output_stream::<T, _, _>(
        def.config(),
        move |out: &mut [T], info: &cpal::OutputCallbackInfo| {
            let mut s = shared.lock().unwrap();
            let keep = alive.load(Ordering::Relaxed);
            let mut real = 0;
            for frame in out.chunks_mut(ch) {
                while ready.is_empty() {
                    let v = match s.queue.pop_front() {
                        Some(v) => {
                            real += 1;
                            v
                        }
                        None if keep => dither(&mut seed),
                        None => 0.0,
                    };
                    s.recent.push_back(v);
                    rs.push(&[v], &mut ready);
                }
                frame.fill(T::from_sample(ready.pop_front().unwrap_or(0.0)));
            }
            s.played(info, real, rate);
        },
        on_err,
        None,
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resampler_rates() {
        let input: Vec<f32> = (0..2205).map(|i| i as f32).collect();
        for (from, to) in [(22_050, 48_000), (48_000, 16_000), (16_000, 16_000)] {
            let mut out = Vec::new();
            Resampler::new(from, to).push(&input, &mut out);
            let want = input.len() as f64 * to as f64 / from as f64;
            assert!((out.len() as f64 - want).abs() <= 1.0, "{from}->{to}: {} vs {want}", out.len());
            // A ramp stays a ramp: never decreasing.
            assert!(out.windows(2).all(|w| w[1] >= w[0]));
        }
    }

    #[test]
    fn resampler_one_sample_at_a_time_matches_bulk() {
        let input: Vec<f32> = (0..500).map(|i| (i as f32 * 0.1).sin()).collect();
        let mut bulk = Vec::new();
        Resampler::new(22_050, 44_100).push(&input, &mut bulk);
        let mut rs = Resampler::new(22_050, 44_100);
        let mut pulled = VecDeque::new();
        for &s in &input {
            rs.push(&[s], &mut pulled);
        }
        assert_eq!(bulk, Vec::from(pulled));
    }
}
