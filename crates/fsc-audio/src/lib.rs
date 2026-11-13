//! The voice pipeline, run on one dedicated thread:
//!
//! ```text
//! watch:   mic ─► VAD segments ─► whisper ─► wake phrase? ─► Wake
//! listen:  mic (16 kHz) ─► Silero VAD endpointing ─► whisper ─► Transcript
//! speak:   sentence ─► espeak-ng + Piper ─► speaker
//! both:    recent samples ─► FFT ─► Spectrum (for the visualiser)
//! ```
//!
//! The UI talks to it through `AudioHandle` and hears back through the
//! `emit` callback.
//!
//! There is one microphone, handed between three owners: `Watching` (idle,
//! waiting for the wake phrase), `Listening` (capturing a prompt) and `Held`
//! (open while a reply is pending or a conversation is open; it only
//! listens for the wake phrase, so you can talk over a reply).
//! `take_mic` and `park_mic` move it between them.

mod io;
mod spectrum;
mod stt;
mod tts;
mod vad;
mod wake;

use std::collections::VecDeque;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

use fsc_core::config::VoiceConfig;
use fsc_core::models;

use io::{MIC_RATE, Mic, Player};
use spectrum::Analyzer;
use stt::Stt;
use tts::Tts;
use vad::{CHUNK, Vad};

pub use spectrum::BANDS;

#[derive(Debug, Clone)]
pub enum AudioEvent {
    /// Models finished loading. `stt`/`tts` say which halves work.
    Ready { stt: bool, tts: bool, missing_models: bool },
    /// The mic is open but delivering silence (e.g. Bluetooth earbuds still
    /// switching to their headset profile, which can take several seconds).
    MicWaiting,
    /// Real audio is now arriving: speak.
    MicLive,
    /// Band levels 0..1, low to high frequency.
    Spectrum(Vec<f32>),
    /// The VAD heard speech start.
    SpeechStarted,
    /// Capture ended; whisper is running.
    Transcribing,
    /// Final text. Empty if nothing intelligible was heard.
    Transcript(String),
    /// A conversation follow-up waited `hang_up_after_secs` and nobody
    /// spoke. The mic is held until the UI ends the conversation.
    NobodySpoke,
    SpeakingStarted,
    SpeakingDone,
    /// The wake phrase was heard. `command` is anything said right after it
    /// in the same breath ("fishsticks and custard, what time is it?").
    Wake { command: Option<String> },
    Error(String),
}

enum Cmd {
    /// `follow_up`: part of an ongoing conversation; wait for speech
    /// indefinitely instead of giving up after `no_speech_timeout_ms`.
    Listen { follow_up: bool },
    /// While on, the mic stays open between utterances (headset stays in
    /// its two-way profile), like a phone call.
    Conversation(bool),
    /// End the utterance now (push-to-talk released) and transcribe it.
    Finish,
    /// Abandon the utterance.
    Cancel,
    Speak(String),
    StopSpeaking,
    /// The reply won't be spoken; don't keep the mic open for it.
    ReleaseMic,
    /// Enable/disable wake-phrase watching (the tray's mute).
    WakeEnabled(bool),
}

#[derive(Clone)]
pub struct AudioHandle {
    tx: mpsc::Sender<Cmd>,
}

impl AudioHandle {
    pub fn listen(&self) {
        let _ = self.tx.send(Cmd::Listen { follow_up: false });
    }
    pub fn listen_follow_up(&self) {
        let _ = self.tx.send(Cmd::Listen { follow_up: true });
    }
    pub fn set_conversation(&self, on: bool) {
        let _ = self.tx.send(Cmd::Conversation(on));
    }
    pub fn finish(&self) {
        let _ = self.tx.send(Cmd::Finish);
    }
    pub fn cancel(&self) {
        let _ = self.tx.send(Cmd::Cancel);
    }
    pub fn speak(&self, text: impl Into<String>) {
        let _ = self.tx.send(Cmd::Speak(text.into()));
    }
    pub fn stop_speaking(&self) {
        let _ = self.tx.send(Cmd::StopSpeaking);
    }
    pub fn release_mic(&self) {
        let _ = self.tx.send(Cmd::ReleaseMic);
    }
    pub fn set_wake_enabled(&self, on: bool) {
        let _ = self.tx.send(Cmd::WakeEnabled(on));
    }
}

/// Must be called at the very start of `main`, before any threads exist:
/// points the bundled espeak-ng at the system's phoneme data.
pub fn prepare_env() {
    const VAR: &str = "PIPER_ESPEAKNG_DATA_DIRECTORY";
    if std::env::var_os(VAR).is_none() {
        for dir in ["/usr/share", "/usr/lib/x86_64-linux-gnu", "/usr/local/share"] {
            if std::path::Path::new(dir).join("espeak-ng-data").exists() {
                // SAFETY: documented to run before any other threads start.
                unsafe { std::env::set_var(VAR, dir) };
                break;
            }
        }
    }
}

pub fn spawn<F>(cfg: VoiceConfig, emit: F) -> AudioHandle
where
    F: Fn(AudioEvent) + Send + 'static,
{
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("fsc-audio".into())
        .spawn(move || Engine::new(cfg, emit).run(rx))
        .expect("spawn audio thread");
    AudioHandle { tx }
}

struct Listening {
    mic: Mic,
    follow_up: bool,
    /// When real (non-zero) audio started arriving.
    live: Option<Instant>,
    pcm: Vec<f32>,
    pending: Vec<f32>,
    speech_start: Option<usize>,
    last_speech: usize,
    run: u32,
    started: Instant,
}

/// The mic kept open after an utterance until the spoken reply has
/// finished. On Bluetooth headsets, closing the mic flips the headset back
/// to its playback-only profile, which removes the audio device the reply
/// is about to play on. Holding the mic also makes a follow-up question
/// start instantly. Meanwhile it listens for the wake phrase (barge-in).
struct Held {
    mic: Mic,
    since: Instant,
    det: WakeDetector,
}

impl Held {
    fn new(mic: Mic) -> Self {
        Self { mic, since: Instant::now(), det: WakeDetector::default() }
    }
}

/// Idle mic, listening for the wake phrase.
struct Watching {
    mic: Mic,
    det: WakeDetector,
}

impl Watching {
    fn new(mic: Mic) -> Self {
        Self { mic, det: WakeDetector::default() }
    }
}

/// Cuts mic audio into speech segments with the VAD; each segment is
/// transcribed and checked for the wake phrase.
#[derive(Default)]
struct WakeDetector {
    /// Recent audio before speech starts, so the first syllable isn't lost.
    preroll: VecDeque<f32>,
    pending: Vec<f32>,
    /// The current speech segment, if one is in progress.
    seg: Option<Vec<f32>>,
    run: u32,
    silence: usize,
    /// Early check said "not the wake phrase": ignore the rest of this segment.
    rejected: bool,
    checked_early: bool,
}

enum Segment {
    /// The first `EARLY_CHECK` of a segment still in progress.
    Early(Vec<f32>),
    /// A finished segment.
    Done(Vec<f32>),
}

impl WakeDetector {
    fn feed(&mut self, fresh: &[f32], vad: &mut Vad, end_silence: usize, max_len: usize) -> Option<Segment> {
        self.pending.extend_from_slice(fresh);
        while self.pending.len() >= CHUNK {
            let chunk: Vec<f32> = self.pending.drain(..CHUNK).collect();
            let p = vad.prob(&chunk).unwrap_or(0.0);
            match self.seg.as_mut() {
                None => {
                    self.preroll.extend(&chunk);
                    let excess = self.preroll.len().saturating_sub(PREROLL);
                    self.preroll.drain(..excess);
                    if p >= VAD_ON {
                        self.run += 1;
                    } else if p < VAD_OFF {
                        self.run = 0;
                    }
                    if self.run >= 3 {
                        self.seg = Some(self.preroll.drain(..).collect());
                        self.silence = 0;
                        self.rejected = false;
                        self.checked_early = false;
                    }
                }
                Some(seg) => {
                    if !self.rejected {
                        seg.extend_from_slice(&chunk);
                    }
                    if p >= VAD_ON {
                        self.silence = 0;
                    } else if p < VAD_OFF {
                        self.silence += CHUNK;
                    }
                    if self.silence >= end_silence || seg.len() >= max_len {
                        let seg = self.seg.take().unwrap_or_default();
                        self.run = 0;
                        if !self.rejected {
                            return Some(Segment::Done(seg));
                        }
                    } else if !self.rejected && !self.checked_early && seg.len() >= EARLY_CHECK {
                        self.checked_early = true;
                        return Some(Segment::Early(seg.clone()));
                    }
                }
            }
        }
        None
    }
}

/// Feed fresh mic audio to `det`. Returns (what was heard, what followed the
/// phrase) once a finished segment starts with the wake phrase.
fn detect_wake(
    det: &mut WakeDetector,
    fresh: &[f32],
    vad: &mut Vad,
    stt: &Stt,
    cfg: &VoiceConfig,
) -> Option<(String, String)> {
    let end_silence = cfg.end_silence_ms as usize * MIC_RATE as usize / 1000;
    let max_len = cfg.max_utterance_secs as usize * MIC_RATE as usize;
    match det.feed(fresh, vad, end_silence, max_len)? {
        Segment::Early(audio) => {
            let heard = stt.transcribe(&audio).unwrap_or_default();
            if wake::match_wake(&heard, &cfg.wake_phrase).is_none() {
                det.rejected = true;
            }
            None
        }
        Segment::Done(audio) => {
            let heard = stt.transcribe(&audio).unwrap_or_default();
            let rest = wake::match_wake(&heard, &cfg.wake_phrase)?;
            Some((heard, rest))
        }
    }
}

/// Audio kept from before speech starts.
const PREROLL: usize = MIC_RATE as usize / 2;
/// Transcribe a long segment's first part this early to reject non-wake
/// speech (a TV, a conversation) without transcribing all of it.
const EARLY_CHECK: usize = MIC_RATE as usize * 5 / 2;
/// Don't retry opening a failing mic more often than this.
const WATCH_RETRY: Duration = Duration::from_secs(10);

/// Extra time after the last sample is due at the speaker before we call
/// speech finished.
const SPEECH_TAIL: Duration = Duration::from_millis(400);
/// A held mic is normally released by the UI (`release_mic`) once the reply
/// and its speech are finished. This is only the safety net if that never
/// comes: no activity (speech) for this long.
const HOLD_MAX_WAIT: Duration = Duration::from_secs(120);
/// A wake phrase heard this soon after we said it ourselves is probably our
/// own voice coming back through the mic (speakers, not headphones).
const OWN_VOICE_WINDOW: Duration = Duration::from_secs(30);

struct Speaking {
    player: Player,
    queue: VecDeque<String>,
    active: bool,
    idle_since: Option<Instant>,
}

struct Engine<F> {
    cfg: VoiceConfig,
    emit: F,
    vad: Option<Vad>,
    stt: Option<Stt>,
    tts: Option<Tts>,
    listening: Option<Listening>,
    held: Option<Held>,
    watching: Option<Watching>,
    wake_enabled: bool,
    watch_failed_at: Option<Instant>,
    conversation: bool,
    speaking: Option<Speaking>,
    /// When a spoken sentence last contained the wake phrase.
    said_phrase_at: Option<Instant>,
    analyzer: Analyzer,
    last_spectrum: Instant,
}

const SPECTRUM_EVERY: Duration = Duration::from_millis(33);
/// Give up if the mic delivers nothing but silence for this long.
const MIC_START_TIMEOUT: Duration = Duration::from_secs(12);
const VAD_ON: f32 = 0.5;
const VAD_OFF: f32 = 0.35;
const PAD: usize = (MIC_RATE as usize * 3) / 10; // 300 ms either side of speech

impl<F: Fn(AudioEvent)> Engine<F> {
    fn new(cfg: VoiceConfig, emit: F) -> Self {
        Self {
            cfg,
            emit,
            vad: None,
            stt: None,
            tts: None,
            listening: None,
            held: None,
            watching: None,
            wake_enabled: true,
            watch_failed_at: None,
            conversation: false,
            speaking: None,
            said_phrase_at: None,
            analyzer: Analyzer::default(),
            last_spectrum: Instant::now(),
        }
    }

    fn load(&mut self) {
        let missing = !models::missing(&self.cfg).is_empty();
        let env = match ort::init().with_name("fsc").build() {
            Ok(e) => Some(e),
            Err(e) => {
                (self.emit)(AudioEvent::Error(format!("onnxruntime: {e}")));
                None
            }
        };
        if let Some(env) = &env {
            match Vad::load(env, &models::vad_path()) {
                Ok(v) => self.vad = Some(v),
                Err(e) => eprintln!("fsc: VAD unavailable: {e:#}"),
            }
            match Tts::load(
                env,
                &models::piper_path(&self.cfg),
                &models::piper_config_path(&self.cfg),
                self.cfg.speech_rate,
            ) {
                Ok(t) => self.tts = Some(t),
                Err(e) => eprintln!("fsc: TTS unavailable: {e:#}"),
            }
        }
        let t0 = Instant::now();
        match Stt::load(&models::whisper_path(&self.cfg), &self.cfg.language, self.cfg.gpu) {
            Ok(s) => {
                // Warm up: the first run compiles GPU shaders.
                let _ = s.transcribe(&[0.0; MIC_RATE as usize]);
                eprintln!("fsc: whisper ready in {:.1}s", t0.elapsed().as_secs_f32());
                self.stt = Some(s);
            }
            Err(e) => eprintln!("fsc: speech recognition unavailable: {e:#}"),
        }
        (self.emit)(AudioEvent::Ready {
            stt: self.stt.is_some() && self.vad.is_some(),
            tts: self.tts.is_some(),
            missing_models: missing,
        });
    }

    fn run(mut self, rx: mpsc::Receiver<Cmd>) {
        self.load();
        loop {
            let busy = self.listening.is_some()
                || self.watching.is_some()
                || self.held.is_some()
                || self.speaking.as_ref().is_some_and(|s| s.active);
            let wait = if busy { Duration::from_millis(15) } else { Duration::from_millis(250) };
            match rx.recv_timeout(wait) {
                Ok(cmd) => self.command(cmd),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
            self.step_watch();
            self.step_listen();
            self.step_hold();
            self.step_speak();
            self.step_spectrum();
        }
    }

    fn command(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::Conversation(on) => {
                self.conversation = on;
                if on {
                    // Warm, continuously running output for the whole call.
                    self.open_player();
                }
                if let Some(s) = self.speaking.as_ref() {
                    s.player.set_keep_alive(on);
                }
                if !on {
                    if self.listening.is_none()
                        && let Some(h) = self.held.take()
                    {
                        eprintln!("fsc: conversation ended");
                        self.park_mic(h.mic);
                    }
                    self.idle_player();
                }
            }
            Cmd::Listen { follow_up } => {
                self.stop_speaking();
                if self.listening.is_some() {
                    return;
                }
                if self.vad.is_none() || self.stt.is_none() {
                    (self.emit)(AudioEvent::Error("speech models not loaded (run `fsc fetch-models`)".into()));
                    (self.emit)(AudioEvent::Transcript(String::new()));
                    return;
                }
                // Reuse an open mic: it's already live (no Bluetooth switch wait).
                let held = self.take_mic();
                let reused = held.is_some();
                match held.map(Ok).unwrap_or_else(|| Mic::open(&self.cfg.input_device)) {
                    Ok(mic) => {
                        if let Some(v) = self.vad.as_mut() {
                            v.reset();
                        }
                        self.listening = Some(Listening {
                            mic,
                            follow_up,
                            live: reused.then(Instant::now),
                            pcm: Vec::with_capacity(MIC_RATE as usize * 10),
                            pending: Vec::new(),
                            speech_start: None,
                            last_speech: 0,
                            run: 0,
                            started: Instant::now(),
                        });
                        (self.emit)(if reused { AudioEvent::MicLive } else { AudioEvent::MicWaiting });
                        self.analyzer.reset();
                    }
                    Err(e) => {
                        (self.emit)(AudioEvent::Error(format!("microphone: {e:#}")));
                        (self.emit)(AudioEvent::Transcript(String::new()));
                    }
                }
            }
            Cmd::Finish => {
                if let Some(l) = self.listening.take() {
                    self.finish(l);
                }
            }
            Cmd::Cancel => {
                if let Some(l) = self.listening.take() {
                    self.park_mic(l.mic);
                } else if !self.conversation
                    && let Some(h) = self.held.take()
                {
                    self.park_mic(h.mic);
                }
            }
            Cmd::WakeEnabled(on) => {
                self.wake_enabled = on;
                eprintln!("fsc: wake word {}", if on { "on" } else { "muted" });
                if !on {
                    self.watching = None;
                }
            }
            Cmd::ReleaseMic => {
                if !self.conversation
                    && self.held.is_some()
                    && self.speaking.as_ref().is_none_or(|s| !s.active && s.queue.is_empty())
                    && let Some(h) = self.held.take()
                {
                    eprintln!("fsc: released held mic (reply not spoken)");
                    self.park_mic(h.mic);
                    self.idle_player();
                }
            }
            Cmd::Speak(text) => self.queue_speech(text),
            Cmd::StopSpeaking => self.stop_speaking(),
        }
    }

    fn step_listen(&mut self) {
        let Some(l) = self.listening.as_mut() else { return };
        let fresh = l.mic.drain();
        if l.live.is_none() {
            // A Bluetooth placeholder source yields exact zeros until the
            // headset profile is up; any real mic has a noise floor.
            if !fresh.iter().any(|x| x.abs() > 1e-5) {
                if l.started.elapsed() > MIC_START_TIMEOUT {
                    self.listening = None;
                    (self.emit)(AudioEvent::Error("microphone sent no audio (muted?)".into()));
                    (self.emit)(AudioEvent::Transcript(String::new()));
                }
                return;
            }
            l.live = Some(Instant::now());
            if let Some(v) = self.vad.as_mut() {
                v.reset();
            }
            eprintln!("fsc: mic live after {:.1}s", l.started.elapsed().as_secs_f32());
            (self.emit)(AudioEvent::MicLive);
        }
        let Some(vad) = self.vad.as_mut() else { return };
        l.pcm.extend_from_slice(&fresh);
        l.pending.extend_from_slice(&fresh);

        while l.pending.len() >= CHUNK {
            let chunk: Vec<f32> = l.pending.drain(..CHUNK).collect();
            let end = l.pcm.len() - l.pending.len();
            let p = vad.prob(&chunk).unwrap_or(0.0);
            if p >= VAD_ON {
                l.run += 1;
                if l.run >= 3 && l.speech_start.is_none() {
                    l.speech_start = Some(end.saturating_sub(3 * CHUNK));
                    (self.emit)(AudioEvent::SpeechStarted);
                }
                l.last_speech = end;
            } else if p < VAD_OFF {
                l.run = 0;
            }
        }

        let rate = MIC_RATE as usize;
        let silence = l.pcm.len().saturating_sub(l.last_speech);
        let done = match l.speech_start {
            // The length cap counts from when speech started, not from when
            // listening started (a follow-up may wait a long time first).
            Some(start) => {
                silence > self.cfg.end_silence_ms as usize * rate / 1000
                    || l.pcm.len() - start > self.cfg.max_utterance_secs as usize * rate
            }
            // A follow-up waits for the other side of the "call" to speak,
            // but not forever.
            None => {
                let limit = if l.follow_up {
                    (self.cfg.hang_up_after_secs > 0).then(|| Duration::from_secs(self.cfg.hang_up_after_secs as u64))
                } else {
                    Some(Duration::from_millis(self.cfg.no_speech_timeout_ms as u64))
                };
                limit.is_some_and(|d| l.live.is_some_and(|t| t.elapsed() > d))
            }
        };
        if l.speech_start.is_none() && l.pcm.len() > 2 * PREROLL {
            // Waiting (possibly indefinitely) for speech: keep only recent audio.
            // Safe because no index into `pcm` is held until speech starts.
            let drop = l.pcm.len() - PREROLL;
            l.pcm.drain(..drop);
        }

        if done && let Some(l) = self.listening.take() {
            if l.follow_up && l.speech_start.is_none() {
                eprintln!("fsc: nobody spoke for {}s, hanging up", self.cfg.hang_up_after_secs);
                if self.conversation {
                    self.held = Some(Held::new(l.mic));
                } else {
                    self.park_mic(l.mic);
                }
                (self.emit)(AudioEvent::NobodySpoke);
            } else {
                self.finish(l);
            }
        }
    }

    fn finish(&mut self, l: Listening) {
        let Listening { mic, mut pcm, speech_start, last_speech, .. } = l;
        pcm.extend(mic.drain());
        if self.conversation || (self.cfg.hold_mic_while_speaking && speech_start.is_some()) {
            self.held = Some(Held::new(mic));
            if speech_start.is_some() {
                // Open the output now, while the headset is already in its
                // two-way profile, so the reply doesn't wait for it.
                self.open_player();
            }
        } else {
            self.park_mic(mic);
        }
        let Some(start) = speech_start else {
            (self.emit)(AudioEvent::Transcript(String::new()));
            return;
        };
        let Some(stt) = self.stt.as_ref() else { return };
        (self.emit)(AudioEvent::Transcribing);
        let a = start.saturating_sub(PAD);
        let b = (last_speech + PAD).min(pcm.len()).max(a);
        let t0 = Instant::now();
        match stt.transcribe(&pcm[a..b]) {
            Ok(text) => {
                eprintln!(
                    "fsc: transcribed {:.1}s of audio in {:.2}s: {text:?}",
                    (b - a) as f32 / MIC_RATE as f32,
                    t0.elapsed().as_secs_f32()
                );
                (self.emit)(AudioEvent::Transcript(text));
            }
            Err(e) => {
                (self.emit)(AudioEvent::Error(format!("transcription: {e:#}")));
                (self.emit)(AudioEvent::Transcript(String::new()));
            }
        }
    }

    fn wake_active(&self) -> bool {
        self.cfg.wake_word && self.wake_enabled && self.vad.is_some() && self.stt.is_some()
    }

    /// Take the open mic from whoever is idling with it.
    fn take_mic(&mut self) -> Option<Mic> {
        let mic = self.held.take().map(|h| h.mic).or_else(|| self.watching.take().map(|w| w.mic))?;
        mic.drain();
        Some(mic)
    }

    /// A mode is done with the mic: watch for the wake phrase with it, or close it.
    fn park_mic(&mut self, mic: Mic) {
        if self.wake_active() {
            mic.drain();
            if let Some(v) = self.vad.as_mut() {
                v.reset();
            }
            self.watching = Some(Watching::new(mic));
        }
    }

    /// Keep a mic open for the wake phrase whenever nothing else is using it.
    fn step_watch(&mut self) {
        if !self.wake_active() {
            self.watching = None;
            return;
        }
        if self.watching.is_none() && self.listening.is_none() && self.held.is_none() {
            if self.watch_failed_at.is_some_and(|t| t.elapsed() < WATCH_RETRY) {
                return;
            }
            match Mic::open(&self.cfg.input_device) {
                Ok(mic) => {
                    eprintln!("fsc: watching for the wake phrase");
                    self.watch_failed_at = None;
                    if let Some(v) = self.vad.as_mut() {
                        v.reset();
                    }
                    self.watching = Some(Watching::new(mic));
                }
                Err(e) => {
                    eprintln!("fsc: wake word mic unavailable: {e:#}");
                    self.watch_failed_at = Some(Instant::now());
                    return;
                }
            }
        }
        let Some(w) = self.watching.as_mut() else { return };
        let fresh = w.mic.drain();
        let (Some(vad), Some(stt)) = (self.vad.as_mut(), self.stt.as_ref()) else { return };
        let Some(hit) = detect_wake(&mut w.det, &fresh, vad, stt, &self.cfg) else { return };
        if self.wake_heard(hit) {
            // Keep the mic for what comes next, like after any utterance.
            if let Some(w) = self.watching.take() {
                self.held = Some(Held::new(w.mic));
            }
            self.open_player();
        }
    }

    /// The wake phrase was heard: tell the UI, unless it was our own voice.
    fn wake_heard(&mut self, (heard, rest): (String, String)) -> bool {
        if self.said_phrase_at.is_some_and(|t| t.elapsed() < OWN_VOICE_WINDOW) {
            eprintln!("fsc: ignored wake phrase (probably our own voice): {heard:?}");
            return false;
        }
        eprintln!("fsc: wake phrase heard: {heard:?}");
        (self.emit)(AudioEvent::Wake { command: (!rest.is_empty()).then_some(rest) });
        true
    }

    /// Start the idle countdown on an open-but-silent player.
    fn idle_player(&mut self) {
        if let Some(s) = self.speaking.as_mut()
            && !s.active
            && s.queue.is_empty()
            && s.idle_since.is_none()
        {
            s.idle_since = Some(Instant::now());
        }
    }

    fn step_hold(&mut self) {
        let wake = self.wake_active();
        let Some(h) = self.held.as_mut() else { return };
        let fresh = h.mic.drain();
        // Talking over a reply: the UI stops it and takes the mic.
        let hit = match (wake, self.vad.as_mut(), self.stt.as_ref()) {
            (true, Some(vad), Some(stt)) => detect_wake(&mut h.det, &fresh, vad, stt, &self.cfg),
            _ => None,
        };
        if let Some(hit) = hit
            && self.wake_heard(hit)
        {
            if let Some(h) = self.held.as_mut() {
                h.since = Instant::now();
            }
            return;
        }
        let Some(h) = self.held.as_mut() else { return };
        if self.conversation {
            return; // the line stays open until the conversation ends
        }
        if self.speaking.as_ref().is_some_and(|s| s.active || !s.queue.is_empty()) {
            h.since = Instant::now();
        }
        if h.since.elapsed() > HOLD_MAX_WAIT {
            eprintln!("fsc: released held mic (nothing happened for {}s)", HOLD_MAX_WAIT.as_secs());
            if let Some(h) = self.held.take() {
                self.park_mic(h.mic);
            }
            self.idle_player();
        }
    }

    /// Make sure an output stream is open (idle until speech is queued).
    fn open_player(&mut self) {
        let Some(tts) = self.tts.as_ref() else { return };
        if self.speaking.is_some() {
            return;
        }
        match Player::open(tts.sample_rate()) {
            Ok(player) => {
                player.set_keep_alive(self.conversation);
                self.speaking = Some(Speaking { player, queue: VecDeque::new(), active: false, idle_since: None })
            }
            Err(e) => (self.emit)(AudioEvent::Error(format!("audio output: {e:#}"))),
        }
    }

    fn queue_speech(&mut self, text: String) {
        if self.tts.is_some() && !text.trim().is_empty() {
            self.open_player();
            if let Some(s) = self.speaking.as_mut() {
                s.idle_since = None;
                s.queue.push_back(text);
                return;
            }
        }
        // Couldn't take it (no voice, no output, nothing to say). Still
        // answer, so the UI isn't left waiting for speech that never comes.
        if !self.speaking.as_ref().is_some_and(|s| s.active || !s.queue.is_empty()) {
            (self.emit)(AudioEvent::SpeakingDone);
        }
    }

    fn step_speak(&mut self) {
        let Some(s) = self.speaking.as_mut() else { return };
        s.player.revive_if_dead();
        // Keep ~half a second buffered ahead; synthesize the next sentence when low.
        if s.player.queued_secs() < 0.5
            && let Some(text) = s.queue.pop_front()
            && let Some(tts) = self.tts.as_mut()
        {
            if wake::mentions(&text, &self.cfg.wake_phrase) {
                self.said_phrase_at = Some(Instant::now());
            }
            match tts.synth(&text) {
                Ok(pcm) => {
                    s.player.enqueue(&pcm);
                    s.idle_since = None;
                    if !s.active {
                        s.active = true;
                        eprintln!(
                            "fsc: speaking (mic held: {}, output latency {} ms)",
                            self.held.is_some(),
                            s.player.latency().as_millis()
                        );
                        self.analyzer.reset();
                        (self.emit)(AudioEvent::SpeakingStarted);
                    }
                }
                Err(e) => {
                    (self.emit)(AudioEvent::Error(format!("speech: {e:#}")));
                    // Nothing playing and nothing left: report done so the UI moves on.
                    if !s.active && s.queue.is_empty() {
                        s.idle_since = Some(Instant::now());
                        (self.emit)(AudioEvent::SpeakingDone);
                    }
                }
            }
        }
        if s.active && s.queue.is_empty() && !s.player.busy(SPEECH_TAIL) {
            eprintln!("fsc: finished speaking (output latency {} ms)", s.player.latency().as_millis());
            s.active = false;
            s.idle_since = Some(Instant::now());
            (self.emit)(AudioEvent::SpeakingDone);
        }
        // Release the output device after a few idle seconds (never mid-conversation).
        if !self.conversation
            && s.idle_since.is_some_and(|t| t.elapsed() > Duration::from_secs(3))
            && s.queue.is_empty()
        {
            self.speaking = None;
        }
    }

    fn stop_speaking(&mut self) {
        // Only an actual interruption ends the hold. A precautionary stop
        // (e.g. when a new prompt is sent) must not release the mic while
        // the reply that needs it hasn't started yet.
        let was_speaking = self.speaking.as_ref().is_some_and(|s| s.active || !s.queue.is_empty());
        if was_speaking && let Some(h) = self.held.as_mut() {
            h.since = Instant::now();
        }
        if let Some(s) = self.speaking.as_mut() {
            s.queue.clear();
            s.player.clear();
            if s.active {
                s.active = false;
                s.idle_since = Some(Instant::now());
                (self.emit)(AudioEvent::SpeakingDone);
            }
        }
    }

    fn step_spectrum(&mut self) {
        if self.last_spectrum.elapsed() < SPECTRUM_EVERY {
            return;
        }
        let bands = if let Some(l) = &self.listening {
            Some(self.analyzer.analyze(&l.pcm[l.pcm.len().saturating_sub(1024)..], MIC_RATE))
        } else if let Some(s) = self.speaking.as_ref().filter(|s| s.active) {
            Some(self.analyzer.analyze(&s.player.recent(), s.player.rate))
        } else {
            None
        };
        if let Some(b) = bands {
            self.last_spectrum = Instant::now();
            (self.emit)(AudioEvent::Spectrum(b));
        }
    }
}

/// `fscd --self-test`: speak a sentence with Piper, transcribe it back with
/// whisper, and report timings. Exercises the models, espeak-ng data and
/// the GPU without touching the microphone or speakers.
pub fn self_test(cfg: &VoiceConfig) -> anyhow::Result<()> {
    use anyhow::Context;
    let missing = models::missing(cfg);
    if !missing.is_empty() {
        anyhow::bail!("missing models (run `fsc fetch-models`): {}", missing.iter().map(|m| m.label).collect::<Vec<_>>().join(", "));
    }
    let env = ort::init().with_name("fsc-selftest").build()?;

    let t = Instant::now();
    let mut vad = Vad::load(&env, &models::vad_path())?;
    println!("vad      loaded in {:.2}s", t.elapsed().as_secs_f32());

    let t = Instant::now();
    let mut tts = Tts::load(&env, &models::piper_path(cfg), &models::piper_config_path(cfg), cfg.speech_rate)?;
    println!("piper    loaded in {:.2}s", t.elapsed().as_secs_f32());

    let t = Instant::now();
    let stt = Stt::load(&models::whisper_path(cfg), &cfg.language, cfg.gpu)?;
    println!("whisper  loaded in {:.2}s (gpu: {})", t.elapsed().as_secs_f32(), cfg.gpu);

    let sentence = "Fishsticks and custard. What is the weather like in London today?";
    let t = Instant::now();
    let pcm = tts.synth(sentence).context("synthesis")?;
    let secs = pcm.len() as f32 / tts.sample_rate() as f32;
    println!("piper    {secs:.1}s of speech in {:.2}s", t.elapsed().as_secs_f32());

    // Visualiser check: walk the speech in 33 ms hops like playback does.
    let mut an = Analyzer::default();
    let hop = tts.sample_rate() as usize / 30;
    let (mut sum, mut sat, mut n) = (0.0f32, 0usize, 0usize);
    for end in (1024..pcm.len()).step_by(hop) {
        for v in an.analyze(&pcm[..end], tts.sample_rate()) {
            sum += v;
            sat += (v >= 0.99) as usize;
            n += 1;
        }
    }
    println!(
        "spectrum mean level {:.2}, {:.1}% of bands pinned at the top (want ~0.3-0.5, <5%)",
        sum / n.max(1) as f32,
        100.0 * sat as f32 / n.max(1) as f32
    );

    // Resample to 16 kHz for VAD + whisper.
    let mut rs = io::Resampler::new(tts.sample_rate(), MIC_RATE);
    let mut pcm16 = Vec::new();
    rs.push(&pcm, &mut pcm16);

    let speechy = pcm16.as_chunks::<CHUNK>().0.iter().filter(|c| vad.prob(c.as_slice()).unwrap_or(0.0) >= VAD_ON).count();
    println!("vad      {speechy} of {} chunks flagged as speech", pcm16.len() / CHUNK);

    let _ = stt.transcribe(&[0.0; MIC_RATE as usize]); // warm-up (shader compile)
    let t = Instant::now();
    let text = stt.transcribe(&pcm16)?;
    println!("whisper  transcribed in {:.2}s", t.elapsed().as_secs_f32());
    println!("said:    {sentence}");
    println!("heard:   {text}");

    // Wake phrase: a hit with a command, a bare hit, and two decoys.
    for line in [
        "Fishsticks and custard, what time is it?",
        "Fishsticks and custard.",
        "I had fish and chips for dinner.",
        "The fish sticks were good with custard.",
    ] {
        let pcm = tts.synth(line)?;
        let mut rs = io::Resampler::new(tts.sample_rate(), MIC_RATE);
        let mut pcm16 = Vec::new();
        rs.push(&pcm, &mut pcm16);
        let t = Instant::now();
        let heard = stt.transcribe(&pcm16)?;
        let verdict = match wake::match_wake(&heard, &cfg.wake_phrase) {
            Some(rest) if rest.is_empty() => "WAKE".to_string(),
            Some(rest) => format!("WAKE + command {rest:?}"),
            None => "no wake".to_string(),
        };
        println!("wake     {:.2}s  {heard:?} -> {verdict}", t.elapsed().as_secs_f32());
    }
    Ok(())
}
