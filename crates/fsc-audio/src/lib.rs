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
use std::sync::Arc;
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
pub use io::microphones;
pub use wake::check_phrase as check_wake_phrase;

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
    /// After `set_language`: whether `voice` loaded (`false`: it isn't
    /// downloaded, or is broken; replies aren't spoken meanwhile).
    Voice { voice: String, ok: bool },
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
    /// Listen for another phrase (already checked with `check_wake_phrase`).
    WakePhrase(String),
    /// Use another microphone (`voice.input_device`) from now on.
    InputDevice(String),
    /// Hear `whisper` (a language code or "auto") and speak with Piper
    /// voice `voice` from now on.
    Language { whisper: String, voice: String },
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
    pub fn set_wake_phrase(&self, phrase: impl Into<String>) {
        let _ = self.tx.send(Cmd::WakePhrase(phrase.into()));
    }
    /// Use another microphone: a substring of its name, "" for the system's.
    pub fn set_input_device(&self, name: impl Into<String>) {
        let _ = self.tx.send(Cmd::InputDevice(name.into()));
    }
    /// Recognise `whisper` and speak with `voice` (reloaded if it changed,
    /// or if it failed to load before: say once it's downloaded). Answered
    /// with `AudioEvent::Voice`.
    pub fn set_language(&self, whisper: impl Into<String>, voice: impl Into<String>) {
        let _ = self.tx.send(Cmd::Language { whisper: whisper.into(), voice: voice.into() });
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
    onset: Onset,
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
/// transcribed and checked for the wake phrase. Transcription runs on its
/// own thread, so the audio thread keeps feeding the speaker meanwhile
/// (on speakers, the mic hears our own reply as speech).
#[derive(Default)]
struct WakeDetector {
    /// Recent audio before speech starts, so the first syllable isn't lost.
    preroll: VecDeque<f32>,
    pending: Vec<f32>,
    /// The current speech segment, if one is in progress.
    seg: Option<Vec<f32>>,
    onset: Onset,
    silence: usize,
    /// Early checks of the segment in progress.
    check: Check,
    /// Words allowed before the phrase in this segment: more when it carries
    /// on from a rejected one, which ends with whatever was being said.
    lead: wake::Lead,
    /// Counts segments, so a late early-check result for an abandoned one is ignored.
    generation: u64,
    /// Transcriptions in flight, oldest first.
    jobs: VecDeque<Job>,
    /// An early check heard the phrase at the start of this segment.
    heard: bool,
}

#[derive(Default, PartialEq)]
enum Check {
    /// Transcribe what there is once the segment is this long.
    Due(usize),
    /// An early transcription is running.
    Waiting,
    /// The phrase was heard (or there's nothing to check): wait for the end.
    #[default]
    Done,
}

struct Job {
    generation: u64,
    /// The finished segment, rather than an early look at one in progress.
    done: bool,
    lead: wake::Lead,
    /// An early check of this segment had already heard the phrase.
    heard: bool,
    /// What was heard in the wake phrase's language, and, for a finished
    /// segment with a command after the phrase in another language, in that.
    rx: mpsc::Receiver<(String, Option<String>)>,
}

impl WakeDetector {
    fn feed(&mut self, fresh: &[f32], vad: &mut Vad, stt: &Arc<Stt>, end_silence: usize, max_len: usize, phrase: &str) {
        self.pending.extend_from_slice(fresh);
        while self.pending.len() >= CHUNK {
            let chunk: Vec<f32> = self.pending.drain(..CHUNK).collect();
            let p = vad.prob(&chunk).unwrap_or(0.0);
            match self.seg.as_mut() {
                None => {
                    self.preroll.extend(&chunk);
                    let excess = self.preroll.len().saturating_sub(PREROLL);
                    self.preroll.drain(..excess);
                    if self.onset.step(p) {
                        let seg = self.preroll.drain(..).collect();
                        self.start(seg, EARLY_CHECK, wake::Lead::Fillers);
                    }
                }
                Some(seg) => {
                    seg.extend_from_slice(&chunk);
                    if p >= VAD_ON {
                        self.silence = 0;
                    } else if p < VAD_OFF {
                        self.silence += CHUNK;
                    }
                    if self.silence >= end_silence || seg.len() >= max_len {
                        let seg = self.seg.take().unwrap_or_default();
                        self.onset.reset();
                        self.check = Check::Done;
                        self.transcribe(stt, seg, true, phrase);
                    } else if let Check::Due(at) = self.check
                        && seg.len() >= at
                        && self.jobs.is_empty()
                    {
                        // Only when nothing else is being transcribed: on a slow
                        // (CPU) whisper, early checks wait rather than pile up.
                        let seg = seg.clone();
                        self.check = Check::Waiting;
                        self.transcribe(stt, seg, false, phrase);
                    }
                }
            }
        }
    }

    fn start(&mut self, seg: Vec<f32>, check_at: usize, lead: wake::Lead) {
        self.seg = Some(seg);
        self.silence = 0;
        self.check = Check::Due(check_at);
        self.lead = lead;
        self.heard = false;
        self.generation += 1;
    }

    fn transcribe(&mut self, stt: &Arc<Stt>, audio: Vec<f32>, done: bool, phrase: &str) {
        let (tx, rx) = mpsc::channel();
        let stt = stt.clone();
        let (lead, heard_early, phrase) = (self.lead, self.heard, phrase.to_string());
        let spawned = std::thread::Builder::new()
            .name("fsc-wake-stt".into())
            .spawn(move || {
                let text = stt.transcribe_wake(&audio).unwrap_or_default();
                // Woken with a command in another language than the phrase:
                // hear the command in its own (here, off the audio thread).
                let command = done
                    && stt.wake_differs()
                    && (heard_early
                        || wake::match_wake_after(&text, &phrase, lead, &stt.language())
                            .is_some_and(|r| !r.trim().is_empty()));
                let main = command.then(|| stt.transcribe(&audio).unwrap_or_default());
                let _ = tx.send((text, main));
            });
        match spawned {
            Ok(_) => self.jobs.push_back(Job { generation: self.generation, done, lead: self.lead, heard: self.heard, rx }),
            Err(e) => eprintln!("fsc: could not start wake transcription: {e}"),
        }
    }

    /// Collect finished transcriptions. Returns (what was heard, what followed
    /// the phrase) once a finished segment starts with the wake phrase.
    fn poll(&mut self, phrase: &str, language: &str) -> Option<(String, String)> {
        while let Some(job) = self.jobs.front() {
            let (heard, main) = match job.rx.try_recv() {
                Ok(r) => r,
                Err(mpsc::TryRecvError::Empty) => return None,
                Err(mpsc::TryRecvError::Disconnected) => (String::new(), None),
            };
            let Some(job) = self.jobs.pop_front() else { break };
            let hit = wake::match_wake_after(&heard, phrase, job.lead, language);
            if std::env::var_os("FSC_TRACE_WAKE").is_some() {
                eprintln!("TRACE done={} gen={}/{} lead={:?} {heard:?} -> {hit:?}", job.done, job.generation, self.generation, job.lead);
            }
            if job.done {
                // The command as said, when it was heard again in its own
                // language: after the phrase if whisper kept that, else all.
                let command = |rest: String| match &main {
                    Some(m) => wake::match_wake_after(m, phrase, wake::Lead::Any(usize::MAX), language)
                        .unwrap_or_else(|| m.clone()),
                    None => rest,
                };
                if let Some(rest) = hit {
                    return Some((heard, command(rest)));
                }
                if job.heard && let Some(m) = main {
                    // The early look heard the phrase but the whole segment's
                    // transcript lost it: whisper can leave out a phrase in
                    // another language than it's set to. Only then (`main`
                    // is heard only when the languages differ); otherwise the
                    // finished transcript has the last word, as a late
                    // correction of a loose early match.
                    return Some((heard, m));
                }
                continue;
            }
            if job.generation != self.generation || self.check != Check::Waiting {
                continue; // that segment is over
            }
            let Some(seg) = self.seg.as_mut() else { continue };
            if hit.is_some() {
                // Heard. The finished segment's transcript also has whatever
                // came before (other speech carried over), so it may sit
                // further in by then.
                self.check = Check::Done;
                self.lead = wake::Lead::Any(usize::MAX);
                self.heard = true;
            } else if wake::could_start(&heard, phrase) {
                // Maybe mid-phrase ("hey, uh... fishsticks and"): look again later.
                self.check = Check::Due(seg.len() + RECHECK);
            } else {
                // Not the wake phrase (a TV, a conversation). Carry on with
                // just the end of it, so the phrase said over continuing
                // background speech is still caught.
                let keep = seg.split_off(seg.len().saturating_sub(OVERLAP));
                self.start(keep, OVERLAP + RECHECK, wake::Lead::Any(OVERLAP_LEADING_WORDS));
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
    stt: &Arc<Stt>,
    cfg: &VoiceConfig,
) -> Option<(String, String)> {
    let end_silence = cfg.end_silence_ms as usize * MIC_RATE as usize / 1000;
    let max_len = cfg.max_utterance_secs as usize * MIC_RATE as usize;
    det.feed(fresh, vad, stt, end_silence, max_len, &cfg.wake_phrase);
    det.poll(&cfg.wake_phrase, &stt.language())
}

/// Audio kept from before speech starts.
const PREROLL: usize = MIC_RATE as usize / 2;
/// Transcribe a long segment's first part this early to reject non-wake
/// speech (a TV, a conversation) without transcribing all of it.
const EARLY_CHECK: usize = MIC_RATE as usize * 5 / 2;
/// After an early check, look again once this much more has been said.
const RECHECK: usize = MIC_RATE as usize * 2;
/// What's kept of a rejected segment that's still going: room for the wake
/// phrase to have started just before the cut.
const OVERLAP: usize = MIC_RATE as usize * 2;
/// Words of other speech that can come before the phrase in that overlap.
const OVERLAP_LEADING_WORDS: usize = 6;
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
    stt: Option<Arc<Stt>>,
    tts: Option<Tts>,
    /// The Piper voice `tts` is (or was meant to be).
    voice: String,
    /// Kept to load another voice later.
    env: Option<ort::environment::Environment>,
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
/// Speechy chunks in a row (a dip between `VAD_OFF` and `VAD_ON` doesn't
/// break the run) before speech counts as started.
const ONSET_CHUNKS: u32 = 3;

/// Decides when speech starts, from per-chunk VAD probabilities.
#[derive(Default)]
struct Onset {
    run: u32,
}

impl Onset {
    /// Feed one chunk's probability; true while a run of speech is long
    /// enough to count.
    fn step(&mut self, p: f32) -> bool {
        if p >= VAD_ON {
            self.run += 1;
        } else if p < VAD_OFF {
            self.run = 0;
        }
        self.run >= ONSET_CHUNKS
    }

    fn reset(&mut self) {
        self.run = 0;
    }
}
const PAD: usize = (MIC_RATE as usize * 3) / 10; // 300 ms either side of speech

impl<F: Fn(AudioEvent)> Engine<F> {
    fn new(cfg: VoiceConfig, emit: F) -> Self {
        Self {
            cfg,
            emit,
            vad: None,
            stt: None,
            tts: None,
            voice: String::new(),
            env: None,
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
        if let Err(e) = wake::check_phrase(&self.cfg.wake_phrase) {
            eprintln!("fsc: voice.wake_phrase: {e}; it may not wake, or wake too often");
        }
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
        }
        self.env = env;
        self.voice = self.cfg.voice();
        self.load_voice();
        let t0 = Instant::now();
        match Stt::load(&models::whisper_path(&self.cfg), &self.cfg.spoken().whisper, self.cfg.gpu, &self.cfg.wake_phrase) {
            Ok(s) => {
                // Warm up: the first run compiles GPU shaders.
                let _ = s.transcribe(&[0.0; MIC_RATE as usize]);
                eprintln!("fsc: whisper ready in {:.1}s", t0.elapsed().as_secs_f32());
                self.stt = Some(Arc::new(s));
            }
            Err(e) => eprintln!("fsc: speech recognition unavailable: {e:#}"),
        }
        (self.emit)(AudioEvent::Ready {
            stt: self.stt.is_some() && self.vad.is_some(),
            tts: self.tts.is_some(),
            missing_models: missing,
        });
    }

    /// Load `self.voice` (none if it isn't downloaded).
    fn load_voice(&mut self) {
        self.tts = None;
        let Some(env) = &self.env else { return };
        if !models::has_voice(&self.voice) {
            eprintln!("fsc: voice {} isn't downloaded", self.voice);
            return;
        }
        let t0 = Instant::now();
        match Tts::load(env, &models::piper_path(&self.voice), &models::piper_config_path(&self.voice), self.cfg.speech_rate) {
            Ok(t) => {
                eprintln!("fsc: voice {} ready in {:.1}s", self.voice, t0.elapsed().as_secs_f32());
                self.tts = Some(t);
            }
            Err(e) => eprintln!("fsc: TTS unavailable: {e:#}"),
        }
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
                            onset: Onset::default(),
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
            Cmd::WakePhrase(phrase) => {
                eprintln!("fsc: wake phrase is now {phrase:?}");
                if let Some(stt) = &self.stt {
                    stt.set_wake_phrase(&phrase);
                }
                self.cfg.wake_phrase = phrase;
            }
            Cmd::Language { whisper, voice } => {
                eprintln!("fsc: language {whisper}, voice {voice}");
                if let Some(stt) = &self.stt {
                    stt.set_language(&whisper);
                }
                if voice != self.voice || self.tts.is_none() {
                    self.stop_speaking();
                    self.voice = voice;
                    self.load_voice();
                }
                (self.emit)(AudioEvent::Voice { voice: self.voice.clone(), ok: self.tts.is_some() });
            }
            Cmd::InputDevice(name) => {
                eprintln!("fsc: microphone is now {:?}", if name.is_empty() { "the system default" } else { &name });
                self.cfg.input_device = name;
                // The wake watch moves over now (step_watch reopens it). A
                // recording in progress finishes on the old one, and a mic
                // held for a reply or a conversation is let go when next
                // handed over (take_mic / park_mic).
                self.watching = None;
                self.watch_failed_at = None;
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
        if l.live.is_some() && l.mic.failed() {
            // Transcribe what was said before it went.
            (self.emit)(AudioEvent::Error("microphone stopped".into()));
            if let Some(mut l) = self.listening.take() {
                l.pcm.extend(fresh);
                self.finish(l);
            }
            return;
        }
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
            let started = l.onset.step(p);
            if p >= VAD_ON {
                if started && l.speech_start.is_none() {
                    l.speech_start = Some(end.saturating_sub(ONSET_CHUNKS as usize * CHUNK));
                    (self.emit)(AudioEvent::SpeechStarted);
                }
                l.last_speech = end;
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
        if mic.device != self.cfg.input_device {
            return None; // another mic was chosen since: open that one
        }
        mic.drain();
        Some(mic)
    }

    /// A mode is done with the mic: watch for the wake phrase with it, or close it.
    fn park_mic(&mut self, mic: Mic) {
        if self.wake_active() && mic.device == self.cfg.input_device {
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
        if w.mic.failed() {
            // Unplugged, or the stream died: reopen it (after WATCH_RETRY,
            // so a device that keeps failing isn't hammered).
            eprintln!("fsc: wake word mic stopped; reopening");
            self.watching = None;
            self.watch_failed_at = Some(Instant::now());
            return;
        }
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
        if h.mic.failed() {
            // A follow-up opens a fresh mic; otherwise wake watching does.
            eprintln!("fsc: held mic stopped; dropping it");
            self.held = None;
            return;
        }
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
    let voice = cfg.voice();
    let mut tts = Tts::load(&env, &models::piper_path(&voice), &models::piper_config_path(&voice), cfg.speech_rate)?;
    println!("piper    loaded in {:.2}s", t.elapsed().as_secs_f32());

    let t = Instant::now();
    let stt = Stt::load(&models::whisper_path(cfg), &cfg.spoken().whisper, cfg.gpu, &cfg.wake_phrase)?;
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

#[cfg(test)]
mod tests {
    use super::*;

    /// ONNX Runtime is set up once per process, so the tests share it.
    fn test_env() -> &'static ort::environment::Environment {
        static ENV: std::sync::OnceLock<ort::environment::Environment> = std::sync::OnceLock::new();
        ENV.get_or_init(|| ort::init().with_name("fsc-test").build().unwrap())
    }

    /// Each language's voice says a few things; whisper, set to that
    /// language, must hear them back, and the answers must read right. Then
    /// the English wake phrase with a German question. Downloads the voices.
    /// `cargo test -p fsc-audio --release -- --ignored --nocapture languages_`
    #[test]
    #[ignore]
    fn languages_round_trip() {
        use fsc_core::lang;
        use fsc_core::permission::{Answer, parse_answer};
        let env = test_env();
        let base = VoiceConfig::default();
        let stt = Arc::new(Stt::load(&models::whisper_path(&base), "en", base.gpu, &base.wake_phrase).unwrap());
        let fetch = |voice: &str| {
            for m in models::voice_files(voice) {
                if !m.path.exists() {
                    models::download(&m, |_, _| {}).unwrap_or_else(|e| panic!("{voice}: {e:#}"));
                }
            }
        };
        let say = |tts: &mut Tts, text: &str| {
            let pcm = tts.synth(text).unwrap();
            let mut rs = io::Resampler::new(tts.sample_rate(), MIC_RATE);
            let mut out = Vec::new();
            rs.push(&pcm, &mut out);
            out
        };
        let only: Option<String> = std::env::var("FSC_TEST_LANGS").ok();
        let mut failures: Vec<String> = Vec::new();
        for l in &lang::LANGUAGES {
            if only.as_ref().is_some_and(|o| !o.split(',').any(|c| c == l.code)) {
                continue;
            }
            let voice = l.voice(if l.code == "pt" { "PT" } else { "" });
            fetch(voice);
            let mut tts = Tts::load(env, &models::piper_path(voice), &models::piper_config_path(voice), 1.0).unwrap();
            stt.set_language(l.code);
            let heard = |tts: &mut Tts, text: &str| stt.transcribe(&say(tts, text)).unwrap();
            let sample = heard(&mut tts, l.phrases.sample);
            let yes = heard(&mut tts, &format!("{}.", l.answers.consent[0]));
            let always = heard(&mut tts, &format!("{}, {}.", l.answers.consent[0], l.answers.always.last().unwrap()));
            let no = heard(&mut tts, &format!("{}.", l.answers.deny[0]));
            let q = heard(&mut tts, &l.question(lang::AskKind::Command, ""));
            eprintln!("{} {voice}\n  sample {sample:?}\n  yes {yes:?} always {always:?} no {no:?}\n  question {q:?}", l.code);
            // One synthetic word is a harsh test (Piper varies each take, and
            // whisper invents things for very short clips), so a miss is
            // reported, not failed: the question is asked again in real use.
            for (what, text, want) in [("yes", &yes, Answer::Once), ("always", &always, Answer::Always), ("no", &no, Answer::Deny)] {
                let got = parse_answer(text, l);
                if got != Some(want) {
                    eprintln!("  WEAK {what}: heard {text:?} -> {got:?}");
                }
            }
        }
        // The English wake phrase, then German, heard as German.
        if only.as_ref().is_none_or(|o| o.contains("wake")) {
            let de = lang::find("de").unwrap();
            fetch(de.voice(""));
            let mut alan = Tts::load(env, &models::piper_path("en_GB-alan-medium"), &models::piper_config_path("en_GB-alan-medium"), 1.0).unwrap();
            let mut thorsten = Tts::load(env, &models::piper_path(de.voice("")), &models::piper_config_path(de.voice("")), 1.0).unwrap();
            stt.set_language("de");
            let mut cfg = base.clone();
            cfg.language = "de".into();
            let mut audio = say(&mut alan, "Fishsticks and custard,");
            audio.extend(say(&mut thorsten, "wie spät ist es?"));
            audio.extend(vec![0.0f32; MIC_RATE as usize * 2]);
            let mut vad = Vad::load(env, &models::vad_path()).unwrap();
            let mut det = WakeDetector::default();
            let mut hit = None;
            for c in audio.chunks(MIC_RATE as usize / 50) {
                std::thread::sleep(Duration::from_millis(20));
                if let Some(h) = detect_wake(&mut det, c, &mut vad, &stt, &cfg) {
                    hit = Some(h);
                    break;
                }
            }
            let t = Instant::now();
            while hit.is_none() && t.elapsed() < Duration::from_secs(20) && !det.jobs.is_empty() {
                std::thread::sleep(Duration::from_millis(20));
                hit = det.poll(&cfg.wake_phrase, "de");
            }
            eprintln!("wake (de): {hit:?}");
            if hit.is_none() {
                failures.push("the English wake phrase wasn't heard with language de".into());
            }
        }
        assert!(failures.is_empty(), "{failures:#?}");
    }

    /// Speech from Piper through the wake detector, as the mic would deliver
    /// it. Needs the models: `cargo test -p fsc-audio --release -- --ignored wake_`.
    #[test]
    #[ignore]
    fn wake_detector_end_to_end() {
        let cfg = VoiceConfig::default();
        let env = test_env();
        let mut vad = Vad::load(env, &models::vad_path()).unwrap();
        let voice = cfg.voice();
        let mut tts = Tts::load(env, &models::piper_path(&voice), &models::piper_config_path(&voice), 1.0).unwrap();
        let stt = Arc::new(Stt::load(&models::whisper_path(&cfg), &cfg.spoken().whisper, cfg.gpu, &cfg.wake_phrase).unwrap());
        let say = |tts: &mut Tts, text: &str| {
            let pcm = tts.synth(text).unwrap();
            let mut rs = io::Resampler::new(tts.sample_rate(), MIC_RATE);
            let mut out = Vec::new();
            rs.push(&pcm, &mut out);
            out
        };
        let silence = vec![0.0f32; MIC_RATE as usize * 2];
        let run = |vad: &mut Vad, audio: &[f32]| {
            vad.reset();
            let mut det = WakeDetector::default();
            let t = Instant::now();
            // In real time, as a mic delivers it, so transcriptions keep pace.
            for c in audio.chunks(MIC_RATE as usize / 50) {
                std::thread::sleep(Duration::from_millis(20));
                if let Some(hit) = detect_wake(&mut det, c, vad, &stt, &cfg) {
                    return Some(hit);
                }
            }
            // Let the last transcriptions finish.
            while t.elapsed() < Duration::from_secs(20) && !det.jobs.is_empty() {
                std::thread::sleep(Duration::from_millis(20));
                if let Some(hit) = det.poll(&cfg.wake_phrase, "en") {
                    return Some(hit);
                }
            }
            None
        };

        // A plain wake-up with a command.
        let mut a = say(&mut tts, "Fishsticks and custard, what time is it?");
        a.extend(&silence);
        let hit = run(&mut vad, &a).expect("plain wake");
        eprintln!("plain: {hit:?}");

        // The phrase said over other speech that never pauses (#7) and after
        // more than the early check's worth of it (#6).
        let mut b = say(
            &mut tts,
            "and then we walked all the way down to the harbour to look at the boats, and the weather was lovely, \
             Fishsticks and custard, what time is it?",
        );
        b.extend(&silence);
        let hit = run(&mut vad, &b).expect("wake after continuous speech");
        eprintln!("continuous: {hit:?}");

        // A slow start: the early check lands partway into the phrase (#6).
        let mut h = say(&mut tts, "Hey, um... so... hmm, okay.");
        h.extend(say(&mut tts, "Fishsticks and custard, what time is it?"));
        h.extend(&silence);
        let hit = run(&mut vad, &h).expect("wake after a hesitation");
        eprintln!("hesitation: {hit:?}");

        // No phrase: nothing.
        let mut c = say(&mut tts, "and then we walked all the way down to the harbour to look at the boats.");
        c.extend(&silence);
        assert!(run(&mut vad, &c).is_none());
    }

    #[test]
    fn onset_needs_a_run_and_tolerates_dips() {
        let mut o = Onset::default();
        assert!(!o.step(0.9));
        assert!(!o.step(0.9));
        assert!(!o.step(0.4)); // between OFF and ON: neither counts nor resets
        assert!(o.step(0.9));
        assert!(o.step(0.4));
        assert!(!o.step(0.1)); // below OFF resets
        assert!(!o.step(0.9));
        o.reset();
        assert!(!o.step(0.9));
    }
}
