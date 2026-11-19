//! Microphone capture and speaker playback via cpal (native PipeWire).
//!
//! We ask PipeWire for exactly the format each stage wants (16 kHz mono in,
//! the voice's rate mono out) and let it resample. If a device refuses, we
//! fall back to its default format and convert here.

use anyhow::{Context, Result, anyhow};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{BufferSize, Device, DeviceDirection, FromSample, Sample, SampleFormat, SizedSample, Stream, StreamConfig, SupportedStreamConfig};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const MIC_RATE: u32 = 16_000;

/// The mic named in `voice.input_device` isn't there (unplugged, switched
/// away, a headset gone). It's waited for rather than replaced: the system's
/// default could be a Bluetooth headset, whose mic forces it into
/// call-quality audio.
#[derive(Debug)]
pub struct MicMissing(pub String);

impl std::fmt::Display for MicMissing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "microphone {:?} isn't connected", self.0)
    }
}

impl std::error::Error for MicMissing {}

/// PipeWire's id for a node ("alsa_input.usb-046d_HD_Pro_Webcam_C920_…"),
/// as the tray saves a mic: matched exactly, never in part, so a missing
/// mic is never taken to mean another whose name contains it. An id
/// without a dot ("easyeffects_source") is saved as "pipewire:<id>"
/// (`device_setting`), as it can't be told from a name typed in part.
///
/// Elsewhere a mic is saved by cpal's id, "<host>:<device>" ("wasapi:…",
/// "coreaudio:…"), and matched exactly too.
pub fn is_device_id(name: &str) -> bool {
    let name = name.trim();
    host_prefixed(name) || (!name.is_empty() && !name.contains(char::is_whitespace) && name.contains('.'))
}

/// cpal's hosts whose ids a setting can carry.
const HOSTS: [&str; 5] = ["pipewire:", "wasapi:", "coreaudio:", "asio:", "jack:"];

fn host_prefixed(name: &str) -> bool {
    let low = name.to_lowercase();
    HOSTS.iter().any(|h| low.starts_with(h))
}

/// What to save for the mic with PipeWire id `id`, so it's matched exactly.
pub fn device_setting(id: &str) -> String {
    if is_device_id(id) { id.to_string() } else { format!("pipewire:{id}") }
}

/// The PipeWire id a device setting names (other hosts' ids as they are).
fn node_name(setting: &str) -> &str {
    setting.trim().strip_prefix("pipewire:").unwrap_or(setting.trim())
}

/// The full cpal id (lowercase) a device setting names.
fn cpal_id(setting: &str) -> String {
    let low = setting.trim().to_lowercase();
    if host_prefixed(&low) { low } else { format!("pipewire:{low}") }
}

/// The mic `name` names isn't there. Not when `name` means the default,
/// even with no mic at all: that isn't a named mic gone missing.
pub fn mic_missing(name: &str) -> bool {
    find_input(&cpal::default_host(), name).is_err_and(|e| e.is::<MicMissing>())
}

/// The mic to open: the one `name` names (`Err` if it isn't there), or the
/// system's default when `name` is empty or "default". A device id
/// (`is_device_id`) must match exactly; any other name is a mic's name, or
/// part of it, as typed in config.toml ("C920").
fn find_input(host: &cpal::Host, name: &str) -> Result<Device> {
    let name = name.trim();
    if !name.is_empty() && !name.eq_ignore_ascii_case("default") {
        // Only real inputs: PipeWire's host also lists every speaker (as a
        // capture of what it plays), and a headset's speaker and mic often
        // share a name.
        let inputs: Vec<(Device, String, String)> = host
            .input_devices()
            .map(|devs| {
                devs.filter_map(|d| {
                    let desc = d.description().ok()?;
                    (desc.direction() == DeviceDirection::Input).then(|| {
                        let id = d.id().map(|i| i.to_string()).unwrap_or_default();
                        (d, desc.name().to_lowercase(), id.to_lowercase())
                    })
                })
                .collect()
            })
            .unwrap_or_default();
        let needle = name.to_lowercase();
        let found = if is_device_id(name) {
            let id = cpal_id(&needle);
            inputs.iter().position(|(_, _, i)| *i == id)
        } else {
            let exact = inputs.iter().position(|(_, n, _)| *n == needle);
            exact.or_else(|| inputs.iter().position(|(_, n, id)| n.contains(&needle) || id.contains(&needle)))
        };
        return found
            .and_then(|i| inputs.into_iter().nth(i))
            .map(|(d, _, _)| d)
            .ok_or_else(|| MicMissing(name.to_string()).into());
    }
    host.default_input_device().ok_or_else(|| anyhow!("no microphone found"))
}

/// A microphone PipeWire knows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Microphone {
    /// `node.name`: what's saved (see `is_device_id`).
    pub id: String,
    /// `node.description`: what's shown.
    pub name: String,
}

/// Which of `mics` the setting `voice.input_device` names, matched as
/// `Mic::open` matches it. `None` for the default, or a mic not connected.
pub fn which_mic<'a>(setting: &str, mics: &'a [Microphone]) -> Option<&'a Microphone> {
    let s = setting.trim();
    if s.is_empty() || s.eq_ignore_ascii_case("default") {
        return None;
    }
    let low = s.to_lowercase();
    if is_device_id(s) {
        return mics.iter().find(|m| m.id.to_lowercase() == node_name(&low));
    }
    mics.iter()
        .find(|m| m.name.to_lowercase() == low)
        .or_else(|| mics.iter().find(|m| m.name.to_lowercase().contains(&low) || m.id.to_lowercase().contains(&low)))
}

/// The microphones PipeWire knows, as the tray lists them. Asks `pw-dump`
/// (part of PipeWire), which says what each node is: cpal's list also has
/// speakers and other programs' recordings.
#[cfg(target_os = "linux")]
pub fn microphones() -> Vec<Microphone> {
    let out = match std::process::Command::new("pw-dump").stderr(std::process::Stdio::null()).output() {
        Ok(o) if o.status.success() => o.stdout,
        Ok(o) => {
            eprintln!("fsc: pw-dump failed ({})", o.status);
            return Vec::new();
        }
        Err(e) => {
            eprintln!("fsc: could not run pw-dump: {e}");
            return Vec::new();
        }
    };
    parse_microphones(&out)
}

/// The system's microphones, as cpal lists them: on Windows and macOS its
/// input devices are inputs only. Each is saved by its cpal id.
#[cfg(not(target_os = "linux"))]
pub fn microphones() -> Vec<Microphone> {
    let host = cpal::default_host();
    let Ok(devs) = host.input_devices() else { return Vec::new() };
    let mut mics: Vec<Microphone> = Vec::new();
    for d in devs {
        let Ok(desc) = d.description() else { continue };
        if desc.direction() != DeviceDirection::Input {
            continue;
        }
        let Ok(id) = d.id().map(|i| i.to_string()) else { continue };
        if !mics.iter().any(|m| m.id == id) {
            mics.push(Microphone { id, name: desc.name().to_string() });
        }
    }
    mics
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn parse_microphones(json: &[u8]) -> Vec<Microphone> {
    let Ok(serde_json::Value::Array(objects)) = serde_json::from_slice(json) else { return Vec::new() };
    let mut mics: Vec<Microphone> = Vec::new();
    for o in &objects {
        let props = &o["info"]["props"];
        let class = props["media.class"].as_str().unwrap_or("");
        if !(class == "Audio/Source" || class == "Audio/Source/Virtual" || class == "Audio/Duplex") {
            continue;
        }
        let Some(id) = props["node.name"].as_str().filter(|s| !s.trim().is_empty()) else { continue };
        let name = ["node.description", "node.nick", "node.name"]
            .iter()
            .find_map(|k| props[*k].as_str().filter(|s| !s.trim().is_empty()))
            .unwrap_or(id);
        if !mics.iter().any(|m| m.id == id) {
            mics.push(Microphone { id: id.to_string(), name: name.to_string() });
        }
    }
    mics
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
    /// `voice.input_device` when it was opened.
    pub device: String,
    _stream: Stream,
    buf: Arc<Mutex<Vec<f32>>>,
    /// The stream reported an error (e.g. the device was unplugged).
    dead: Arc<AtomicBool>,
    last_data: std::cell::Cell<Instant>,
}

impl Mic {
    pub fn open(device_name: &str) -> Result<Self> {
        let host = cpal::default_host();
        let dev = find_input(&host, device_name)?;
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
        Ok(Self { device: device_name.to_string(), _stream: stream, buf, dead, last_data: std::cell::Cell::new(Instant::now()) })
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

    #[test]
    fn microphones_from_pw_dump() {
        let props = |class: &str, name: &str, desc: Option<&str>| {
            let mut p = serde_json::json!({ "media.class": class, "node.name": name });
            if let Some(d) = desc {
                p["node.description"] = d.into();
            }
            serde_json::json!({ "type": "PipeWire:Interface:Node", "info": { "props": p } })
        };
        let dump = serde_json::json!([
            { "type": "PipeWire:Interface:Core", "info": { "props": {} } },
            props("Audio/Source", "alsa_input.usb-C920", Some("HD Pro Webcam C920 Analog Stereo")),
            props("Stream/Input/Audio", "fscd", None),
            props("Audio/Sink", "bluez_output.94_DB", Some("WH-XB900N")),
            props("Audio/Source", "bluez_input.94:DB", Some("WH-XB900N")),
            props("Audio/Source/Virtual", "noise_cancel", None),
            props("Audio/Source", "alsa_input.usb-C920", Some("HD Pro Webcam C920 Analog Stereo")),
        ]);
        let names: Vec<(String, String)> =
            parse_microphones(dump.to_string().as_bytes()).into_iter().map(|m| (m.id, m.name)).collect();
        let want = [
            ("alsa_input.usb-C920", "HD Pro Webcam C920 Analog Stereo"),
            ("bluez_input.94:DB", "WH-XB900N"),
            ("noise_cancel", "noise_cancel"),
        ];
        assert_eq!(names, want.map(|(a, b)| (a.to_string(), b.to_string())));
        assert!(parse_microphones(b"not json").is_empty());
    }

    #[test]
    fn device_ids() {
        assert!(is_device_id("alsa_input.usb-046d_HD_Pro_Webcam_C920_98793E2F-02.analog-stereo"));
        assert!(is_device_id("bluez_input.94:DB:56:40:85:61"));
        assert!(!is_device_id("C920"));
        assert!(!is_device_id("USB Audio"));
        assert!(!is_device_id("HD Pro Webcam C920 Analog Stereo"));
        assert!(!is_device_id(""));
        assert!(is_device_id("pipewire:easyeffects_source"));
        assert!(!is_device_id("easyeffects_source"));
        assert_eq!(device_setting("easyeffects_source"), "pipewire:easyeffects_source");
        assert_eq!(device_setting("alsa_input.x.analog-stereo"), "alsa_input.x.analog-stereo");
        let mics = vec![
            Microphone { id: "easyeffects_source_2".into(), name: "Easy Effects 2".into() },
            Microphone { id: "easyeffects_source".into(), name: "Easy Effects".into() },
        ];
        assert_eq!(which_mic("pipewire:easyeffects_source", &mics).map(|m| m.name.as_str()), Some("Easy Effects"));
        assert!(which_mic("pipewire:easyeffects_source", &mics[..1]).is_none());
    }

    #[test]
    fn other_hosts_device_ids() {
        // Windows and macOS mics are saved by cpal's "<host>:<device>" id.
        let wasapi = "wasapi:{0.0.1.00000000}.{6a1c9c0e-0000-4b36-9d3e-1f0c2a7b9e11}";
        assert!(is_device_id(wasapi));
        assert!(is_device_id("CoreAudio:BuiltInMicrophoneDevice"));
        assert_eq!(device_setting(wasapi), wasapi);
        assert_eq!(cpal_id("CoreAudio:BuiltIn"), "coreaudio:builtin");
        assert_eq!(cpal_id("alsa_input.x"), "pipewire:alsa_input.x");
        let mics = vec![Microphone { id: wasapi.into(), name: "Microphone (USB)".into() }];
        assert_eq!(which_mic(wasapi, &mics).map(|m| m.name.as_str()), Some("Microphone (USB)"));
    }
}



