//! What the popup does, without any toolkit: the assistant's state, the
//! commands, and the agent's and audio engine's events. The Qt side
//! (`qt::bridge`) shows the result: it mirrors `history` through its ops,
//! carries out the `Effect`s, and draws the parts in `draw`.
//!
//! What the popup shows comes from three independent activities, combined by
//! `State::phase()`: voice capture (listening, then transcribing), the agent's
//! turn (thinking, working, responding), and speech playback (speaking).

use std::time::{Duration, Instant};
use tokio::sync::mpsc;

use fsc_audio::{AudioEvent, AudioHandle};
use fsc_core::Phase;
use fsc_core::agent::{AgentEvent, AgentHandle, Choices, Setting, pretty_model, pretty_tool};
use fsc_core::config::{self as config, AgentKind, Config, SpeakMode, Surface};
use fsc_core::ipc::{Command, Reply};
use fsc_core::lang::{self, Language, Spoken};
use fsc_core::permission::{self, Answer, PermissionAsk};

use crate::AppEvent;
use crate::draw::{Canvas, Frame, dotfont, panel, viz::Viz};
use crate::history::{Card, History, Kind};
use crate::icon::IconState;
use crate::platform::notify;
use crate::speech_text::SentenceBuffer;
use crate::style::{self, Palette, Rgb, SystemColors, ThemeMode};
use crate::tray::{self, TrayUpdate};

const MAX_EXCHANGES: usize = 20;
pub const STATUS_PITCH: f64 = 3.0;
const STATUS_DOT: f64 = 1.15;
/// A push-to-talk release sooner than this after the press counts as a
/// tap: keep listening and let the VAD decide when you've finished.
const TAP: Duration = Duration::from_millis(350);
/// Follow-ups in a row that hear nothing before a conversation hangs up.
const MAX_EMPTY_FOLLOW_UPS: u32 = 3;
/// Spoken answers to a permission question that can't be understood before
/// we stop listening for one (the buttons still work).
const MAX_ANSWER_TRIES: u32 = 3;
/// How long "CONVERSATION ENDED" shows before the popup closes itself.
const HANG_UP_CLOSE: Duration = Duration::from_secs(3);
/// The longest a quit waits for the memory save.
const QUIT_WAIT: Duration = Duration::from_secs(30);

/// The status line's height: the dot font's rows, plus a pixel each side.
pub fn status_height() -> f64 {
    ((dotfont::ROWS as f64 * STATUS_PITCH).ceil() + 2.0).round()
}

/// Something for the view to do.
#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    /// Put the popup up; `focus`: and type in its text field.
    Show { focus: bool },
    Hide,
    Quit,
    /// The status line's text.
    Meta(String),
    /// The colours changed.
    Palette,
    /// Whether the conversation area shows (it doesn't until there's one).
    HistoryShown(bool),
    /// Show the newest text. `force`: even if you've scrolled up to read.
    Scroll { force: bool },
    /// Drop the highlight in the line last clicked.
    ClearPick,
    Copy(String),
}

/// A timer the controller set, come due (`AppEvent::Timer`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Timer {
    /// A flashed status message's time is up (unless a newer one replaced it).
    Meta(u64),
    /// "CONVERSATION ENDED" has shown long enough.
    HangUp,
    /// Quit even though the memory save hasn't reported back.
    QuitAnyway,
}

/// A permission question on screen.
struct OpenAsk {
    ask: PermissionAsk,
    /// Its card in the history.
    card: u64,
}

struct Exchange {
    /// Its group in the history, and its reply line.
    group: u64,
    reply: u64,
    text: String,
    /// The most recent "⚙ TOOL" line, so a permission card for the same
    /// tool can replace it instead of repeating it.
    last_tool: Option<(String, u64)>,
    /// `text` has grown since it was last put in the reply line. Streamed
    /// text is shown at most once per pass of the event loop rather than on
    /// every chunk, since each change lays out the whole reply again.
    stale: bool,
}

#[derive(Default, Clone, Copy)]
struct Voice {
    stt: bool,
    tts: bool,
    loaded: bool,
}

struct State {
    /// The agent side: Idle / Thinking / Working / Responding / Error.
    claude: Phase,
    listening: Option<Instant>,
    transcribing: bool,
    speaking: bool,

    palette: Palette,
    /// Light/dark from the desktop (tray icon, and the palette fallback).
    theme: ThemeMode,
    /// The desktop's accent colour, when it sets one.
    desktop_accent: Option<Rgb>,
    /// The toolkit palette's colours.
    system: SystemColors,
    muted: bool,
    /// `voice.wake_phrase`, as changed by `fsc wake`.
    wake_phrase: String,
    /// `voice.input_device` and `voice.speak`, as changed from the tray.
    mic: String,
    /// That mic is named but not connected (it's waited for).
    mic_missing: bool,
    speak: SpeakMode,
    /// `voice.language` as set (empty: the locale's) and what it comes to.
    language: String,
    spoken: Spoken,
    /// `voice.piper_voice` as set (empty: the language's usual one).
    piper_voice: String,
    /// The voice the audio engine speaks with (when the language changed
    /// and its voice is still downloading, that voice: silent meanwhile).
    voice_name: String,
    /// A voice being switched to once it's downloaded.
    wanted: Option<String>,
    /// A voice chosen that isn't in the list, saved once its download
    /// works (with the `piper_voice` to go back to if it doesn't).
    unsaved: Option<(String, String)>,
    /// The voice in use being downloaded, and how far (percent).
    fetching: Option<(String, u32)>,
    /// Every voice download running (one per voice: two would write the
    /// same file).
    downloads: Vec<String>,
    /// Say a sample when this voice has loaded (it was just chosen).
    sample_for: Option<String>,
    /// Tell the agent about a language change with the next prompt (its
    /// session started with the old one).
    language_note: Option<String>,
    model: String,
    /// The agent answering, and which are installed (`AgentKind::ALL` order).
    agent: AgentKind,
    agents: [bool; AgentKind::ALL.len()],
    /// The models and modes the agent offers.
    choices: Choices,
    /// The running session keeps long-term memory.
    memory_on: bool,
    menu_sent: Option<tray::MenuState>,
    voice: Voice,
    viz: Viz,
    /// Seconds of animation shown (the status line's blink keeps time by it).
    clock: f64,
    current: Option<Exchange>,
    /// The current prompt was spoken, so the reply should be too.
    voice_turn: bool,
    sentences: SentenceBuffer,
    /// A two-way voice conversation is open: the mic stays live (headset in
    /// its two-way profile) and we listen again after each reply, until the
    /// popup is closed.
    conversation: bool,
    /// Speech handed to the engine that hasn't finished playing yet.
    speech_pending: bool,
    /// The current listen was started automatically after a reply.
    follow_up: bool,
    /// Consecutive follow-ups that heard nothing (stops runaway re-listening).
    empty_follow_ups: u32,
    /// The agent session has exchanges since it started (worth saving).
    session_dirty: bool,
    last_activity: Option<Instant>,
    /// A session's memory update was requested and hasn't reported back.
    memory_saving: bool,
    /// Quit requested; waiting for the memory save to finish.
    quitting: bool,
    /// A prompt that arrived while the agent was still busy (e.g. you spoke
    /// over a reply that hadn't finished stopping). Sent when the turn ends.
    pending_prompt: Option<(String, bool)>,
    /// Permission questions waiting for an answer, oldest first.
    asks: Vec<OpenAsk>,
    /// Spoken replies to the current question that weren't yes/always/no.
    answer_tries: u32,
    /// Last state sent to the tray, to skip redundant re-renders.
    tray_sent: Option<IconState>,
    /// A flashed message is showing; its timer is the latest `meta_gen`.
    meta_flashing: bool,
    meta_gen: u64,
}

impl State {
    fn phase(&self) -> Phase {
        if self.listening.is_some() {
            Phase::Listening
        } else if self.transcribing {
            Phase::Transcribing
        } else if self.speaking {
            Phase::Speaking
        } else {
            self.claude
        }
    }
}

pub struct Controller {
    pub cfg: Config,
    claude: AgentHandle,
    audio: AudioHandle,
    tray_tx: mpsc::UnboundedSender<TrayUpdate>,
    /// For work done off the UI thread (timers, downloads) to report back.
    events: async_channel::Sender<AppEvent>,
    st: State,
    pub history: History,
    effects: Vec<Effect>,
    /// The accent pinned in the config (hex), overriding the desktop's.
    accent_pin: Option<Rgb>,
    /// The popup is up, or, headless, a voice session is running.
    open: bool,
    /// The text field is empty (the view says when that changes).
    entry_empty: bool,
    /// A flush of streamed text is on its way; `scroll_force`: and it should
    /// scroll even if you've scrolled up.
    flush_pending: bool,
    scroll_force: bool,
    /// The status line's text, to say again when a flash is over.
    meta: String,
}

impl Controller {
    pub fn new(
        cfg: Config,
        palette: Palette,
        accent_pin: Option<Rgb>,
        claude: AgentHandle,
        audio: AudioHandle,
        tray_tx: mpsc::UnboundedSender<TrayUpdate>,
        events: async_channel::Sender<AppEvent>,
    ) -> Self {
        let model = pretty_model(&cfg.model_hint());
        let (agent, agents) = (cfg.agent.backend, AgentKind::ALL.map(|k| cfg.available(k)));
        let memory_on = cfg.memory();
        let wake_phrase = cfg.voice.wake_phrase.clone();
        let (mic, speak) = (cfg.voice.input_device.clone(), cfg.voice.speak);
        let (language, spoken, piper_voice, voice_name) =
            (cfg.voice.language.clone(), cfg.voice.spoken(), cfg.voice.piper_voice.clone(), cfg.voice.voice());
        let mut sentences = SentenceBuffer::default();
        sentences.set_language(spoken.language());
        let viz = Viz::new(&cfg.ui);
        let mut c = Controller {
            cfg,
            claude,
            audio,
            tray_tx,
            events,
            st: State {
                claude: Phase::Idle,
                listening: None,
                transcribing: false,
                speaking: false,
                theme: palette.mode,
                palette,
                desktop_accent: None,
                system: SystemColors::default(),
                muted: false,
                wake_phrase,
                mic,
                mic_missing: false,
                speak,
                language,
                spoken,
                piper_voice,
                voice_name,
                fetching: None,
                wanted: None,
                unsaved: None,
                downloads: Vec::new(),
                sample_for: None,
                language_note: None,
                model,
                agent,
                agents,
                choices: Choices::default(),
                memory_on,
                menu_sent: None,
                voice: Voice::default(),
                viz,
                clock: 0.0,
                current: None,
                voice_turn: false,
                sentences,
                conversation: false,
                speech_pending: false,
                follow_up: false,
                empty_follow_ups: 0,
                session_dirty: false,
                last_activity: None,
                memory_saving: false,
                quitting: false,
                pending_prompt: None,
                asks: Vec::new(),
                answer_tries: 0,
                tray_sent: None,
                meta_flashing: false,
                meta_gen: 0,
            },
            history: History::default(),
            effects: Vec::new(),
            accent_pin,
            open: false,
            entry_empty: true,
            flush_pending: false,
            scroll_force: false,
            meta: String::new(),
        };
        c.restore_meta();
        c
    }

    /// What the view has to do since it last asked.
    pub fn take_effects(&mut self) -> Vec<Effect> {
        std::mem::take(&mut self.effects)
    }

    pub fn palette(&self) -> Palette {
        self.st.palette
    }

    pub fn meta(&self) -> &str {
        &self.meta
    }

    /// A prompt is out and its reply not yet finished.
    pub fn in_turn(&self) -> bool {
        self.st.current.is_some()
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    fn headless(&self) -> bool {
        self.cfg.ui.surface == Surface::Headless
    }

    /// Wait `after`, then come back with `t` (as `AppEvent::Timer`).
    fn timer(&self, after: Duration, t: Timer) {
        let tx = self.events.clone();
        tokio::spawn(async move {
            tokio::time::sleep(after).await;
            let _ = tx.send(AppEvent::Timer(t)).await;
        });
    }

    // --- events -----------------------------------------------------------

    pub fn handle(&mut self, ev: AppEvent) {
        match ev {
            AppEvent::Command(cmd, reply) => {
                let r = self.command(cmd);
                if let Some(tx) = reply {
                    let _ = tx.send(r);
                }
            }
            AppEvent::Agent(e) => self.agent_event(e),
            AppEvent::Audio(e) => self.audio_event(e),
            AppEvent::VoiceFetch { voice, progress } => self.voice_fetch(voice, progress),
            AppEvent::Theme(mode) => self.set_theme(mode),
            AppEvent::Accent(c) => {
                self.st.desktop_accent = c;
                self.refresh_palette();
            }
            AppEvent::Timer(t) => self.timer_due(t),
            AppEvent::IdleTick => self.tick_idle(),
            AppEvent::MicName(name) => self.flash_meta(&short_meta(&format!("MIC: {}", name.to_uppercase()))),
            AppEvent::Flush => self.flush(),
        }
    }

    fn timer_due(&mut self, t: Timer) {
        match t {
            Timer::Meta(n) => {
                if n == self.st.meta_gen {
                    self.st.meta_flashing = false;
                    self.restore_meta();
                }
            }
            Timer::HangUp => {
                let st = &self.st;
                let idle = !st.conversation
                    && st.listening.is_none()
                    && !st.transcribing
                    && !st.claude.busy()
                    && !st.speaking
                    && !st.speech_pending;
                if idle && self.entry_empty && self.is_open() {
                    self.hide();
                }
            }
            Timer::QuitAnyway => self.effects.push(Effect::Quit),
        }
    }

    pub fn command(&mut self, cmd: Command) -> Reply {
        match cmd {
            // Headless has nothing to show: the tray click talks or hangs up.
            Command::Toggle if self.headless() => {
                if self.is_open() { self.hide() } else { self.listen() }
            }
            Command::Toggle => {
                if self.is_open() { self.hide() } else { self.show(true) }
            }
            Command::Show => self.show(false),
            Command::Hide => self.hide(),
            Command::Type if self.headless() => {
                return Reply::err("headless mode has no text field: use `fsc ask \"…\"` or talk");
            }
            Command::Type => self.show(true),
            Command::Listen => self.listen(),
            Command::Stop => self.stop(),
            Command::Release => self.release(),
            Command::Ask { text } => {
                if !self.ask(&text, false) {
                    return Reply::err("busy: a reply is still in progress (fsc stop to interrupt)");
                }
            }
            Command::Allow { .. } | Command::Deny => {
                let Some(id) = self.st.asks.first().map(|a| a.ask.id.clone()) else {
                    return Reply::err("nothing is waiting for permission");
                };
                let answer = match cmd {
                    Command::Allow { always: true } => Answer::Always,
                    Command::Allow { always: false } => Answer::Once,
                    _ => Answer::Deny,
                };
                self.answer_ask(&id, answer);
            }
            Command::New => self.new_conversation(),
            Command::Mute { on } => {
                self.st.muted = on.unwrap_or(!self.st.muted);
                let muted = self.st.muted;
                self.audio.set_wake_enabled(!muted);
                self.flash_meta(if muted { "WAKE WORD MUTED" } else { "WAKE WORD ON" });
                self.push_tray();
            }
            Command::Wake { phrase: None } => return Reply::state(self.st.wake_phrase.clone()),
            Command::Wake { phrase: Some(phrase) } => return self.set_wake_phrase(&phrase),
            Command::Status => {
                let mut s = self.st.phase().label().to_lowercase();
                if self.st.muted {
                    s.push_str(" (muted)");
                }
                return Reply::state(s);
            }
            Command::Memory => {
                let home = self.cfg.home();
                let path = fsc_core::memory::path(&home);
                let facts = fsc_core::memory::load(&home);
                return Reply::state(if facts.is_empty() {
                    "(nothing remembered yet)".to_string()
                } else {
                    format!("{}\n({})", facts.join("\n"), path.display())
                });
            }
            Command::Forget => {
                self.claude.forget();
                self.add_divider("MEMORY CLEARED");
                let st = &mut self.st;
                st.session_dirty = false;
                st.last_activity = None;
                // Forgetting cancels any save in flight; it won't report back.
                st.memory_saving = false;
            }
            Command::Agent { backend: None } => return Reply::state(self.st.agent.key()),
            Command::Agent { backend: Some(name) } => {
                let Some(kind) = AgentKind::from_key(&name) else {
                    return Reply::err(format!("no agent called {name:?}: try {}", AgentKind::all_keys()));
                };
                return self.switch_agent(kind);
            }
            Command::Model { value } => return self.set_choice(Setting::Model, value),
            Command::Mode { value } => return self.set_choice(Setting::Mode, value),
            Command::Mic { name } => return self.set_mic(name),
            Command::Speak { mode } => return self.set_speak(mode),
            Command::Settings => return self.open_settings(),
            Command::Language { code } => return self.set_language(code),
            Command::Voice { name } => return self.set_voice(name),
            Command::Quit => self.quit(),
        }
        Reply::ok()
    }

    fn agent_event(&mut self, e: AgentEvent) {
        match e {
            AgentEvent::Ready { model, .. } => {
                if !model.is_empty() {
                    self.st.model = pretty_model(&model);
                    self.restore_meta();
                }
            }
            AgentEvent::TextDelta(t) => {
                if self.st.claude != Phase::Responding {
                    self.set_claude(Phase::Responding);
                }
                let speak = self.should_speak();
                let st = &mut self.st;
                st.viz.kick(0.25 + t.len() as f32 * 0.03);
                if let Some(cur) = st.current.as_mut() {
                    cur.text.push_str(&t);
                    cur.stale = true;
                }
                let ready = if speak { st.sentences.push(&t) } else { Vec::new() };
                for s in ready {
                    self.speak(s);
                }
                self.scroll_to_end(false);
            }
            AgentEvent::ToolUse { name } => {
                if self.st.asks.is_empty() {
                    self.set_claude(Phase::Working);
                }
                if let Some(reply) = self.st.current.as_ref().map(|c| c.reply) {
                    let tool = pretty_tool(&name);
                    let id = self.history.insert_before(reply, Kind::Tool, &format!("󰒓  {}", tool.to_uppercase()));
                    if let (Some(cur), Some(id)) = (self.st.current.as_mut(), id) {
                        cur.last_tool = Some((tool, id));
                    }
                }
                self.scroll_to_end(false);
            }
            AgentEvent::Permission(ask) => self.on_permission(ask),
            AgentEvent::PermissionClosed { id } => self.close_ask(&id, "— WITHDRAWN"),
            AgentEvent::TurnDone { ok, interrupted, error } => {
                // Questions still open when the turn ends were never answered.
                let open: Vec<String> = self.st.asks.iter().map(|a| a.ask.id.clone()).collect();
                for id in open {
                    self.close_ask(&id, "— NOT ANSWERED");
                }
                let rest = if ok && !interrupted { self.st.sentences.flush() } else { None };
                self.st.sentences.clear();
                self.show_current();
                let cur = self.st.current.take();
                if let Some(rest) = rest
                    && self.should_speak()
                {
                    self.speak(rest);
                }
                if let Some(cur) = &cur {
                    if cur.text.trim().is_empty() {
                        self.history.set_visible(cur.reply, false);
                    }
                    if interrupted {
                        self.history.append_to_group(cur.group, Kind::Note, "— STOPPED");
                    } else if ok && cur.text.trim().is_empty() {
                        // Some agents end a turn silently, e.g. after a refusal.
                        self.history.append_to_group(cur.group, Kind::Note, "— NO REPLY");
                    }
                    if let Some(err) = &error {
                        self.history.append_to_group(cur.group, Kind::Error, &format!("! {err}"));
                    }
                }
                self.set_claude(if ok { Phase::Idle } else { Phase::Error });
                // Not `session_dirty`: that's set when a prompt is sent, since
                // a rotation also ends (interrupts) the turn it cuts short.
                self.st.last_activity = Some(Instant::now());
                self.scroll_to_end(false);

                if let Some((text, voice)) = self.st.pending_prompt.take() {
                    self.ask(&text, voice);
                    return;
                }
                let spoken = self.st.voice_turn && self.should_speak();
                self.maybe_release_mic();
                if ok
                    && !interrupted
                    && !spoken
                    && (!self.is_open() || self.headless())
                    && self.cfg.ui.notify_when_hidden
                    && let Some(cur) = cur
                {
                    notify(&cur.text);
                }
                self.maybe_follow_up();
            }
            AgentEvent::Exited { reason } => {
                eprintln!("fsc: {reason}");
            }
            AgentEvent::MemorySaved { ok, facts, error } => {
                self.st.memory_saving = false;
                if ok {
                    self.flash_meta(&format!("MEMORY UPDATED · {facts} FACTS"));
                } else {
                    eprintln!("fsc: memory not updated: {}", error.unwrap_or_default());
                    self.flash_meta("MEMORY NOT UPDATED");
                }
                if self.st.quitting {
                    self.effects.push(Effect::Quit);
                }
            }
            AgentEvent::Forgotten => self.flash_meta("MEMORY CLEARED"),
            AgentEvent::Notice(text) => {
                eprintln!("fsc: {text}");
                self.flash_meta_for(&text.to_uppercase(), Duration::from_secs(6));
            }
            AgentEvent::Choices(c) => {
                if let Some(m) = &c.model
                    && !m.current.is_empty()
                {
                    let name = m.current_name();
                    self.st.model = if name == m.current { pretty_model(name) } else { name.to_uppercase() };
                }
                self.st.choices = c;
                if !self.st.meta_flashing {
                    self.restore_meta();
                }
                self.push_tray();
            }
        }
    }

    fn audio_event(&mut self, e: AudioEvent) {
        match e {
            AudioEvent::Ready { stt, tts, missing_models } => {
                self.st.voice = Voice { stt, tts, loaded: true };
                let voice = self.st.voice_name.clone();
                if missing_models && !fsc_core::models::has_voice(&voice) && stt {
                    // Only the voice is missing (a new language, or the
                    // locale's after an update): fetch it now.
                    self.fetch_voice(&voice);
                } else if missing_models {
                    self.flash_meta("MODELS MISSING · FSC FETCH-MODELS");
                }
            }
            AudioEvent::Voice { voice, ok } => {
                let st = &mut self.st;
                if voice != st.voice_name {
                    return; // superseded by another choice
                }
                st.voice.tts = ok;
                let sample = (ok && st.sample_for.as_deref() == Some(voice.as_str())).then(|| {
                    st.sample_for = None;
                    st.spoken.language().phrases.sample
                });
                if let Some(sample) = sample {
                    // Just chosen: let it say hello, whatever `speak` is set to.
                    self.audio.speak(sample);
                } else if !ok && self.st.fetching.is_none() {
                    self.flash_meta_for(&format!("VOICE {} UNAVAILABLE", voice.to_uppercase()), Duration::from_secs(6));
                }
                self.push_tray();
            }
            AudioEvent::Spectrum(bands) => {
                self.st.viz.set_spectrum(Some(bands));
            }
            AudioEvent::MicWaiting => self.flash_meta_for("WAITING FOR MIC…", Duration::from_secs(15)),
            AudioEvent::MicLive => {
                if !self.st.asks.is_empty() {
                    self.flash_meta_for(choices(lang::english()).1, Duration::from_secs(8));
                } else if self.st.follow_up {
                    self.flash_meta_for("GO AHEAD · ESC ENDS CONVERSATION", Duration::from_secs(5));
                } else {
                    self.flash_meta("SPEAK NOW");
                }
            }
            AudioEvent::SpeechStarted => {}
            AudioEvent::Transcribing => {
                let st = &mut self.st;
                st.listening = None;
                st.transcribing = true;
                st.viz.set_spectrum(None);
                self.refresh();
            }
            AudioEvent::Transcript(text) => {
                let st = &mut self.st;
                let was_capturing = st.listening.is_some() || st.transcribing;
                st.listening = None;
                st.transcribing = false;
                st.viz.set_spectrum(None);
                self.refresh();
                if !was_capturing {
                    return; // cancelled meanwhile
                }
                if let Some(id) = self.st.asks.first().map(|a| a.ask.id.clone()) {
                    self.spoken_answer(&id, &text);
                    return;
                }
                if text.trim().is_empty() {
                    self.st.empty_follow_ups += 1;
                    let gave_up = self.st.conversation && self.st.empty_follow_ups >= MAX_EMPTY_FOLLOW_UPS;
                    if gave_up {
                        // Nobody's talking: hang up so the wake word works again.
                        self.hang_up();
                    } else {
                        self.flash_meta("DIDN'T CATCH THAT");
                        self.maybe_follow_up();
                    }
                } else {
                    self.st.empty_follow_ups = 0;
                    self.ask_or_queue(text, true);
                }
            }
            AudioEvent::NobodySpoke => {
                let st = &mut self.st;
                st.listening = None;
                st.transcribing = false;
                st.viz.set_spectrum(None);
                self.refresh();
                self.hang_up();
            }
            AudioEvent::SpeakingStarted => {
                self.st.speaking = true;
                self.refresh();
            }
            AudioEvent::SpeakingDone => {
                let st = &mut self.st;
                st.speaking = false;
                st.speech_pending = false;
                st.viz.set_spectrum(None);
                self.refresh();
                self.maybe_release_mic();
                self.maybe_follow_up();
            }
            AudioEvent::Wake { command } => self.on_wake(command),
            AudioEvent::MicMissing(name) => {
                let was = std::mem::replace(&mut self.st.mic_missing, name.is_some());
                match name {
                    Some(n) => {
                        // By the name it's listed under, if it's an id.
                        let shown = if fsc_audio::is_device_id(&n) { "MIC".to_string() } else { format!("MIC {}", n.to_uppercase()) };
                        self.flash_meta_for(&short_meta(&format!("{shown} NOT CONNECTED")), Duration::from_secs(8));
                    }
                    None if was => self.flash_meta("MIC CONNECTED"),
                    None => {}
                }
                self.push_tray();
            }
            AudioEvent::Error(err) => {
                eprintln!("fsc: {err}");
                self.flash_meta(&short_meta(&err.to_uppercase()));
            }
        }
    }

    // --- from the view ------------------------------------------------------

    /// Enter in the text field. Returns whether to clear it.
    pub fn submit(&mut self, text: &str) -> bool {
        // "y" / "yes" / "always" / "no" answers an open permission question.
        let open = self.st.asks.first().map(|a| a.ask.id.clone());
        let lang = self.st.spoken.language();
        if let (Some(id), Some(answer)) = (open, permission::parse_answer(text, lang)) {
            self.answer_ask(&id, answer);
            return true;
        }
        self.ask(text, false)
    }

    /// A card's button.
    pub fn answer_button(&mut self, ask_id: &str, label: &str) {
        let answer = match label {
            "ALLOW" => Answer::Once,
            "ALWAYS" => Answer::Always,
            _ => Answer::Deny,
        };
        self.answer_ask(ask_id, answer);
    }

    /// Ctrl+C with nothing highlighted: stop everything, like a terminal.
    pub fn stop_all(&mut self) {
        self.cancel_listen();
        self.interrupt();
    }

    pub fn set_entry_empty(&mut self, empty: bool) {
        self.entry_empty = empty;
    }

    /// Ctrl+Shift+C: every question and reply shown, as plain text.
    pub fn copy_history(&mut self) {
        self.show_current();
        let text = self.history.text();
        if text.is_empty() {
            self.flash_meta("NOTHING TO COPY");
            return;
        }
        self.effects.push(Effect::Copy(text));
        self.flash_meta("CONVERSATION COPIED");
    }

    pub fn set_system_colors(&mut self, sys: SystemColors) {
        self.st.system = sys;
        self.refresh_palette();
    }

    // --- actions ----------------------------------------------------------

    /// Send a prompt. `voice`: it came from speech, so speak the reply.
    fn ask(&mut self, text: &str, voice: bool) -> bool {
        let text = text.trim();
        if text.is_empty() {
            return true;
        }
        if self.st.claude.busy() {
            self.flash_meta("BUSY · CTRL+C TO STOP");
            return false;
        }
        self.audio.stop_speaking();
        // A highlight from before is done with: Ctrl+C during this reply stops it.
        self.effects.push(Effect::ClearPick);
        // Been a while? Start a fresh session (saving memory) before this
        // prompt; the session actor holds the prompt until it's ready.
        if self.session_idle() {
            self.rotate_session();
        }
        self.st.session_dirty = true;

        self.show_current();
        let group = self.history.new_group();
        let prefix = if voice { "󰍬" } else { "›" };
        self.history.push(group, Kind::Prompt, &format!("{prefix} {text}"));
        let reply = self.history.push(group, Kind::Reply, "");
        self.history.set_visible(reply, false);
        self.history.trim(MAX_EXCHANGES);
        self.effects.push(Effect::HistoryShown(true));

        let st = &mut self.st;
        st.current = Some(Exchange { group, reply, text: String::new(), last_tool: None, stale: false });
        st.voice_turn = voice;
        st.sentences.clear();
        self.set_claude(Phase::Thinking);
        match self.st.language_note.take() {
            Some(note) => self.claude.ask(format!("({note})\n\n{text}")),
            None => self.claude.ask(text),
        }
        self.scroll_to_end(true);
        true
    }

    /// Bring the current reply line up to date with its text.
    fn show_current(&mut self) {
        if let Some(cur) = self.st.current.as_mut()
            && std::mem::take(&mut cur.stale)
        {
            let (id, text) = (cur.reply, cur.text.trim_start().to_string());
            self.history.set_text(id, &text);
            self.history.set_visible(id, true);
        }
    }

    fn should_speak(&self) -> bool {
        let st = &self.st;
        st.voice.tts
            && match st.speak {
                SpeakMode::Voice => st.voice_turn,
                SpeakMode::Always => true,
                SpeakMode::Never => false,
            }
    }

    fn session_idle(&self) -> bool {
        let mins = self.cfg.session_idle_minutes();
        let st = &self.st;
        mins > 0
            && st.session_dirty
            && st.last_activity.is_some_and(|t| t.elapsed() >= Duration::from_secs(mins as u64 * 60))
    }

    /// End the agent session (saving memory) and mark it in the history.
    fn rotate_session(&mut self) {
        eprintln!("fsc: session idle, rotating");
        self.end_session(false);
        self.add_divider("NEW SESSION");
    }

    /// End the agent session, saving memory from it if it had any
    /// exchanges. A fresh session is started unless `quitting`.
    fn end_session(&mut self, quitting: bool) {
        let st = &mut self.st;
        let save = st.session_dirty;
        if quitting {
            self.claude.end(save);
        } else {
            self.claude.rotate(save);
        }
        st.memory_saving |= save && st.memory_on;
        st.session_dirty = false;
        st.last_activity = None;
    }

    /// Listen for `phrase` from now on, and save it as `voice.wake_phrase`.
    fn set_wake_phrase(&mut self, phrase: &str) -> Reply {
        let phrase = match fsc_audio::check_wake_phrase(phrase) {
            Ok(p) => p,
            Err(e) => return Reply::err(e),
        };
        self.audio.set_wake_phrase(phrase.clone());
        self.st.wake_phrase = phrase.clone();
        self.flash_meta(&format!("WAKE PHRASE: {}", phrase.to_uppercase()));
        match Config::save_value("voice", "wake_phrase", &phrase) {
            Ok(()) => Reply::state(format!("listening for {phrase:?}")),
            Err(e) => Reply::state(format!("listening for {phrase:?} until fscd restarts (not saved: {e:#})")),
        }
    }

    /// `fsc mic` and the tray's Microphone menu: list the mics, or listen
    /// with another (saved as `voice.input_device`).
    fn set_mic(&mut self, name: Option<String>) -> Reply {
        let current = self.st.mic.clone();
        let is_default = |s: &str| s.trim().is_empty() || s.trim().eq_ignore_ascii_case("default");
        // A device id (what the tray sends) needs no lookup, so the mic list
        // isn't asked for here on the UI thread: a stuck PipeWire would
        // freeze the popup.
        if let Some(id) = name.as_deref().map(str::trim).filter(|n| fsc_audio::is_device_id(n)) {
            return self.use_mic(id.to_string(), id.to_string());
        }
        let mics = fsc_audio::microphones();
        let Some(name) = name else {
            let chosen = fsc_audio::which_mic(&current, &mics);
            let mark = |on: bool| if on { "*" } else { " " };
            let mut lines = vec![format!("{} default  (the system's)", mark(is_default(&current)))];
            lines.extend(mics.iter().map(|m| format!("{} {}  ({})", mark(chosen.is_some_and(|c| c.id == m.id)), m.name, m.id)));
            if !is_default(&current) && chosen.is_none() {
                lines.push(format!("* {current}  (not connected: waiting for it)"));
            }
            return Reply::state(lines.join("\n"));
        };
        let name = name.trim();
        if name.contains('\n') {
            return Reply::err("a microphone name is one line");
        }
        // A mic's full name, as listed, is saved as its id: that one mic and
        // no other. Part of a name, or one not connected, is saved as typed.
        let (setting, shown) = if is_default(name) {
            (String::new(), "the system's default microphone".to_string())
        } else {
            match mics.iter().find(|m| m.name.eq_ignore_ascii_case(name) || m.id == name) {
                Some(m) => (fsc_audio::device_setting(&m.id), m.name.clone()),
                None => (name.to_string(), name.to_string()),
            }
        };
        self.use_mic(setting, shown)
    }

    /// Listen with `setting` (saved as `voice.input_device`). `shown`: its
    /// name; a device id is looked up off the UI thread for the flash.
    fn use_mic(&mut self, setting: String, shown: String) -> Reply {
        if fsc_audio::is_device_id(&setting) {
            let tx = self.events.clone();
            let id = setting.clone();
            tokio::spawn(async move {
                let mics = tokio::task::spawn_blocking(fsc_audio::microphones).await.unwrap_or_default();
                if let Some(m) = fsc_audio::which_mic(&id, &mics) {
                    let _ = tx.send(AppEvent::MicName(m.name.clone())).await;
                }
            });
        }
        self.audio.set_input_device(setting.clone());
        self.st.mic = setting.clone();
        // The audio thread says if the new one is missing.
        self.st.mic_missing = false;
        self.push_tray();
        let flash = if setting.is_empty() { "SYSTEM DEFAULT".to_string() } else { shown.to_uppercase() };
        self.flash_meta(&short_meta(&format!("MIC: {flash}")));
        match Config::save_value("voice", "input_device", &setting) {
            Ok(()) => Reply::state(format!("listening with {shown}")),
            Err(e) => Reply::state(format!("listening with {shown} until fscd restarts (not saved: {e:#})")),
        }
    }

    /// `fsc speak` and the tray's Speak replies menu (saved as `voice.speak`).
    fn set_speak(&mut self, mode: Option<String>) -> Reply {
        let Some(mode) = mode else { return Reply::state(self.st.speak.key()) };
        let Some(mode) = SpeakMode::from_key(&mode) else {
            return Reply::err(format!("speak {mode:?}? Use voice (replies to spoken prompts), always or never"));
        };
        self.st.speak = mode;
        self.push_tray();
        if mode == SpeakMode::Never {
            // Quiet now, not after the reply being read.
            self.audio.stop_speaking();
        }
        self.flash_meta(&format!("SPEAK REPLIES: {}", mode.name().to_uppercase()));
        match Config::save_value("voice", "speak", mode.key()) {
            Ok(()) => Reply::state(format!("speaking replies: {}", mode.name().to_lowercase())),
            Err(e) => Reply::state(format!("speaking replies: {} until fscd restarts (not saved: {e:#})", mode.name().to_lowercase())),
        }
    }

    /// The tray's Settings…: HyprCustard (whose FSC page covers everything)
    /// when it's installed, else config.toml in the desktop's editor.
    fn open_settings(&self) -> Reply {
        let (prog, args, what) = if fsc_core::config::find_program("hyprcustard").is_some() {
            ("hyprcustard", vec![], "HyprCustard".to_string())
        } else {
            match Config::ensure_file() {
                Ok(path) => {
                    let (prog, args) = crate::platform::opener(&path);
                    (prog, args, path.display().to_string())
                }
                Err(e) => return Reply::err(format!("no config file: {e:#}")),
            }
        };
        match crate::platform::spawn_detached(prog, &args) {
            Ok(()) => Reply::state(format!("opened {what}")),
            Err(e) => {
                let msg = format!("could not run {prog}: {e}");
                notify(&format!("{msg}. Settings are in {}", Config::path().map(|p| p.display().to_string()).unwrap_or_default()));
                Reply::err(msg)
            }
        }
    }

    /// `fsc language` and the tray's Language menu: list the languages, or
    /// speak and listen in another (saved as `voice.language`). Its usual
    /// voice comes with it unless the chosen one is already for it.
    fn set_language(&mut self, code: Option<String>) -> Reply {
        let Some(code) = code else {
            let st = &self.st;
            let mut lines: Vec<String> = lang::LANGUAGES
                .iter()
                .map(|l| {
                    let mark = if l.code == st.spoken.code && st.spoken.whisper == l.code { "*" } else { " " };
                    format!("{mark} {}  {} ({})", l.code, l.native, l.name)
                })
                .collect();
            lines.push(format!(
                "{} default  your desktop's language{}",
                if st.language.trim().is_empty() { "*" } else { " " },
                if st.language.trim().is_empty() { format!(" ({})", st.spoken.language().name) } else { String::new() }
            ));
            if st.spoken.whisper != st.spoken.code {
                lines.push(format!("* {}  (heard as is; spoken prompts in English)", st.spoken.whisper));
            }
            return Reply::state(lines.join("\n"));
        };
        let code = code.trim().to_lowercase();
        let setting = if code.is_empty() || code == "default" { String::new() } else { code };
        if !setting.is_empty() && lang::find(&setting).is_none() && setting != "auto" {
            return Reply::err(format!(
                "no language {setting:?} in the list (`fsc language` shows it); set `voice.language` in config.toml for whisper's others"
            ));
        }
        let spoken = lang::resolve(&setting);
        let lang = spoken.language();
        // A voice chosen for the old language doesn't carry over. Saved by
        // name (not left empty) so tools reading the config, like
        // HyprCustard, see which files it needs.
        let piper_voice = config::voice_for(&spoken, &self.st.piper_voice);
        let saved = Config::save_value("voice", "language", &setting)
            .and_then(|()| Config::save_value("voice", "piper_voice", &piper_voice));
        let st = &mut self.st;
        let changed = st.spoken != spoken;
        st.language = setting;
        st.spoken = spoken.clone();
        st.piper_voice = piper_voice;
        st.sentences.set_language(lang);
        if changed {
            let instruction = spoken.instruction();
            let note = instruction.clone().unwrap_or_else(|| "The user now speaks English. Reply in English.".into());
            // Sessions from now on are told in their instructions; the one
            // running started with the old language, so it's told with the
            // next prompt.
            self.claude.set_language(instruction);
            self.st.language_note = Some(note);
        }
        let what = if spoken.whisper == spoken.code {
            format!("speaking {}", lang.name)
        } else {
            format!("listening for {} (its own questions in English)", if spoken.whisper == "auto" { "any language" } else { &spoken.whisper })
        };
        self.flash_meta(&format!("LANGUAGE: {}", if spoken.whisper == spoken.code { lang.native.to_uppercase() } else { spoken.whisper.to_uppercase() }));
        self.apply_voice(true);
        match saved {
            Ok(()) => Reply::state(what),
            Err(e) => Reply::state(format!("{what} until fscd restarts (not saved: {e:#})")),
        }
    }

    /// `fsc voice` and the tray's Voice menu: list the language's voices, or
    /// speak with another (saved as `voice.piper_voice`; downloaded first
    /// if need be).
    fn set_voice(&mut self, name: Option<String>) -> Reply {
        let (code, current) = (self.st.spoken.code, self.st.voice_name.clone());
        let Some(name) = name else {
            let mut lines: Vec<String> = fsc_core::voices::for_language(code)
                .map(|v| {
                    let mark = if v.key == current { "*" } else { " " };
                    let have = if fsc_core::models::has_voice(v.key) { String::new() } else { format!(", {} MB to download", v.mb) };
                    format!("{mark} {}  ({}{have})", v.key, v.label)
                })
                .collect();
            if fsc_core::voices::find(&current).is_none() {
                lines.push(format!("* {current}"));
            }
            return Reply::state(lines.join("\n"));
        };
        let name = name.trim().to_string();
        let setting = if name.is_empty() || name == "default" {
            let st = &self.st;
            st.spoken.language().voice(&st.spoken.region).to_string()
        } else {
            name
        };
        if !setting.is_empty() {
            if setting.split('-').count() != 3 {
                return Reply::err(format!("{setting:?} isn't a Piper voice name (like de_DE-thorsten-medium; `fsc voice` lists them)"));
            }
            if config::voice_language(&setting) != code && self.st.spoken.whisper == code {
                return Reply::err(format!(
                    "{setting} speaks another language; change the language first (`fsc language {}`)",
                    config::voice_language(&setting)
                ));
            }
        }
        // A voice from the list exists; any other is saved only once its
        // download shows it does, so a misspelt name isn't kept.
        let known = fsc_core::voices::find(&setting).is_some() || fsc_core::models::has_voice(&setting);
        let saved = {
            let st = &mut self.st;
            let previous = std::mem::replace(&mut st.piper_voice, setting.clone());
            if known {
                st.unsaved = None;
                Some(Config::save_value("voice", "piper_voice", &setting))
            } else {
                st.unsaved = Some((setting.clone(), previous));
                None
            }
        };
        let voice = self.apply_voice(true);
        let label = fsc_core::voices::find(&voice).map(|v| v.label).unwrap_or(&voice);
        let coming = self.st.wanted.is_some();
        match saved {
            None => Reply::state(format!("downloading {label}; it's saved as your voice once it's here")),
            Some(Ok(())) if coming => Reply::state(format!("downloading {label}; the current voice speaks until then")),
            Some(Ok(())) => Reply::state(format!("speaking with {label}")),
            Some(Err(e)) => Reply::state(format!("speaking with {label} until fscd restarts (not saved: {e:#})")),
        }
    }

    /// Point the audio engine at the language and voice now set, fetching
    /// the voice first if it isn't here. `sample`: say hello in it once
    /// it's loaded. Returns the voice.
    fn apply_voice(&mut self, sample: bool) -> String {
        let here = |v: &str| fsc_core::models::has_voice(v);
        let (whisper, voice, speak_with) = {
            let st = &mut self.st;
            let voice = config::voice_for(&st.spoken, &st.piper_voice);
            if sample && voice != st.voice_name {
                st.sample_for = Some(voice.clone());
            }
            if here(&voice) {
                st.wanted = None;
                st.voice_name = voice.clone();
            } else {
                // Until it's here, the voice in use carries on if it speaks
                // this language; otherwise speech pauses.
                st.wanted = Some(voice.clone());
                let fits = config::voice_language(&st.voice_name) == st.spoken.code && here(&st.voice_name);
                if !fits {
                    st.voice_name = voice.clone();
                }
            }
            (st.spoken.whisper.clone(), voice, st.voice_name.clone())
        };
        self.audio.set_language(whisper, speak_with);
        if here(&voice) {
            let label = fsc_core::voices::find(&voice).map(|v| v.label).unwrap_or(&voice);
            self.flash_meta(&format!("VOICE: {}", label.to_uppercase()));
        } else {
            self.fetch_voice(&voice);
        }
        self.push_tray();
        voice
    }

    /// Download `voice` in the background (once at a time per voice).
    fn fetch_voice(&mut self, voice: &str) {
        {
            let st = &mut self.st;
            if st.downloads.iter().any(|v| v == voice) {
                // Already coming: just show its progress again.
                if st.fetching.as_ref().is_none_or(|(v, _)| v != voice) {
                    st.fetching = Some((voice.to_string(), 0));
                }
                return;
            }
            st.downloads.push(voice.to_string());
            st.fetching = Some((voice.to_string(), 0));
        }
        self.flash_meta_for("DOWNLOADING VOICE…", Duration::from_secs(60));
        self.push_tray();
        let tx = self.events.clone();
        let voice = voice.to_string();
        std::thread::spawn(move || {
            let files = fsc_core::models::voice_files(&voice);
            let mut last = u32::MAX;
            let mut result = Ok(None);
            for (i, m) in files.iter().enumerate() {
                if m.path.exists() {
                    continue;
                }
                let r = fsc_core::models::download(m, |done, total| {
                    // The model is nearly all of it; the config is tiny.
                    let pct = match total {
                        Some(t) if i == 0 => (done * 100 / t.max(1)).min(99) as u32,
                        _ => 99,
                    };
                    if pct != last && pct % 5 == 0 {
                        last = pct;
                        let _ = tx.send_blocking(AppEvent::VoiceFetch { voice: voice.clone(), progress: Ok(Some(pct)) });
                    }
                });
                if let Err(e) = r {
                    result = Err(format!("{e:#}"));
                    break;
                }
            }
            let _ = tx.send_blocking(AppEvent::VoiceFetch { voice, progress: result });
        });
    }

    fn voice_fetch(&mut self, voice: String, progress: Result<Option<u32>, String>) {
        let current = self.st.wanted.as_deref() == Some(voice.as_str()) || self.st.voice_name == voice;
        match progress {
            Ok(Some(pct)) => {
                let st = &mut self.st;
                if current && st.fetching.is_none() {
                    st.fetching = Some((voice.clone(), pct));
                }
                if let Some(f) = st.fetching.as_mut().filter(|(v, _)| *v == voice) {
                    f.1 = pct;
                }
                if current {
                    self.flash_meta_for(&format!("DOWNLOADING VOICE · {pct}%"), Duration::from_secs(60));
                }
            }
            Ok(None) => {
                eprintln!("fsc: downloaded voice {voice}");
                self.end_fetch(&voice);
                if current {
                    let st = &mut self.st;
                    st.wanted = None;
                    st.voice_name = voice.clone();
                    let unsaved = st.unsaved.take_if(|(v, _)| *v == voice);
                    if unsaved.is_some()
                        && let Err(e) = Config::save_value("voice", "piper_voice", &voice)
                    {
                        eprintln!("fsc: could not save the voice: {e:#}");
                    }
                    let whisper = self.st.spoken.whisper.clone();
                    self.audio.set_language(whisper, voice);
                    self.restore_meta();
                }
            }
            Err(e) => {
                eprintln!("fsc: could not download voice {voice}: {e}");
                self.end_fetch(&voice);
                // Back to what was there: the voice in use speaks on.
                let st = &mut self.st;
                if st.wanted.as_deref() == Some(voice.as_str()) {
                    st.wanted = None;
                }
                if let Some((_, previous)) = st.unsaved.take_if(|(v, _)| *v == voice) {
                    st.piper_voice = previous;
                }
                if current {
                    self.flash_meta_for("VOICE DOWNLOAD FAILED", Duration::from_secs(8));
                    notify(&format!("Couldn't download the voice {voice}: {e}"));
                }
            }
        }
        self.push_tray();
    }

    /// `voice`'s download is over (another may have started since).
    fn end_fetch(&mut self, voice: &str) {
        let st = &mut self.st;
        st.downloads.retain(|v| v != voice);
        if st.fetching.as_ref().is_some_and(|(v, _)| v == voice) {
            st.fetching = None;
        }
    }

    /// Hand the conversation to another agent: this session ends (saving
    /// memory) and the next prompt goes to `kind`. Remembered in the config.
    fn switch_agent(&mut self, kind: AgentKind) -> Reply {
        // Reread the config so edits to the agent's own table count.
        let mut cfg = Config::load().unwrap_or_else(|e| {
            eprintln!("fsc: {e:#}; using the settings fscd started with");
            self.cfg.clone()
        });
        self.st.agents = AgentKind::ALL.map(|k| cfg.available(k));
        if kind == self.st.agent {
            self.push_tray();
            return Reply::state(format!("already using {}", kind.name()));
        }
        if !cfg.available(kind) {
            self.push_tray();
            let prog = cfg.program(kind);
            return Reply::err(if prog.is_empty() {
                "set `command` in the [acp] section of config.toml first".to_string()
            } else {
                format!("{} isn't installed (no `{prog}` found)", kind.name())
            });
        }
        cfg.agent.backend = kind;
        let saved = Config::save_backend(kind);
        if let Err(e) = &saved {
            eprintln!("fsc: could not save the agent choice: {e:#}");
        }
        {
            let st = &mut self.st;
            self.claude.switch(&cfg, st.session_dirty);
            // Whether this session saves follows the setting it started
            // with; the next one goes by the config as it is now.
            st.memory_saving |= st.session_dirty && st.memory_on;
            st.memory_on = cfg.memory();
            st.session_dirty = false;
            st.last_activity = None;
            st.agent = kind;
            st.model = pretty_model(&cfg.model_hint());
            // The new agent says what it offers once it's up.
            st.choices = Choices::default();
        }
        self.add_divider(&format!("NOW {}", kind.name().to_uppercase()));
        self.flash_meta(&format!("AGENT: {}", kind.name().to_uppercase()));
        self.push_tray();
        match saved {
            Ok(()) => Reply::state(format!("switched to {}", kind.name())),
            Err(e) => Reply::state(format!("switched to {} for now (not saved: {e:#})", kind.name())),
        }
    }

    /// Called every minute: rotate an idle session in the background so the
    /// next question doesn't wait for the memory save.
    pub fn tick_idle(&mut self) {
        let st = &self.st;
        let quiet = !st.claude.busy() && !st.conversation && st.listening.is_none() && !st.speaking && !st.transcribing;
        if quiet && self.session_idle() {
            self.rotate_session();
        }
    }

    fn add_divider(&mut self, text: &str) {
        if self.history.is_empty() {
            return;
        }
        let time = crate::log::now("%H:%M");
        self.history.push_item(Kind::Note, &format!("— {text} · {time} —"));
        self.history.trim(MAX_EXCHANGES);
    }

    /// Quit, saving memory first if this session had any exchanges (or
    /// waiting for a save already under way).
    fn quit(&mut self) {
        let st = &mut self.st;
        let wait = (st.memory_on && st.session_dirty || st.memory_saving) && !st.quitting;
        st.quitting = true;
        if !wait {
            self.effects.push(Effect::Quit);
            return;
        }
        // Also when a save is already running (from an idle rotation), so
        // no new session is started after it.
        self.end_session(true);
        self.flash_meta_for("SAVING MEMORY…", QUIT_WAIT);
        // Don't hang forever if the save stalls.
        self.timer(QUIT_WAIT, Timer::QuitAnyway);
    }

    /// The wake phrase was heard ("fishsticks[, <command>]").
    fn on_wake(&mut self, command: Option<String>) {
        self.show(false);
        let start_conversation = self.cfg.voice.conversation && !self.st.conversation;
        self.st.empty_follow_ups = 0;
        if start_conversation {
            self.st.conversation = true;
            self.audio.set_conversation(true);
        }
        match command {
            // Busy? Saying the name is talking over the reply: stop it and
            // send this once it has stopped.
            Some(c) => self.ask_or_queue(c, true),
            None => self.listen_inner(false),
        }
    }

    /// Send now, or, if a turn is still winding down, interrupt it and send
    /// when it ends. Spoken prompts are never thrown away as "busy".
    fn ask_or_queue(&mut self, text: String, voice: bool) {
        if self.st.claude.busy() {
            self.st.pending_prompt = Some((text, voice));
            self.interrupt();
            self.flash_meta("STOPPING · THEN ASKING");
        } else {
            // The reply may still be being spoken: stop it, and forget any
            // speech still owed to it.
            self.interrupt();
            self.ask(&text, voice);
        }
    }

    /// Without a conversation, the mic was only held for this reply: let it
    /// go once the turn and its speech are both finished.
    fn maybe_release_mic(&self) {
        let st = &self.st;
        if !st.conversation && !st.claude.busy() && !st.speaking && !st.speech_pending && st.listening.is_none() {
            self.audio.release_mic();
        }
    }

    fn speak(&mut self, text: String) {
        self.st.speech_pending = true;
        self.audio.speak(text);
    }

    /// In a conversation, start listening again once the reply is fully
    /// done: the agent's turn finished and nothing left to say.
    fn maybe_follow_up(&mut self) {
        let st = &self.st;
        // A permission question asked by voice: listen for the answer.
        let answering = !st.asks.is_empty() && st.voice_turn && st.answer_tries < MAX_ANSWER_TRIES;
        let next_question = st.conversation && !st.claude.busy() && st.empty_follow_ups < MAX_EMPTY_FOLLOW_UPS;
        let ready = (answering || next_question)
            && !st.speaking
            && !st.speech_pending
            && st.listening.is_none()
            && !st.transcribing;
        if ready && self.is_open() {
            self.listen_inner(true);
        }
    }

    /// User-initiated listening (hotkey, tray, `fsc listen`).
    fn listen(&mut self) {
        self.st.empty_follow_ups = 0;
        self.listen_inner(false);
    }

    fn listen_inner(&mut self, follow_up: bool) {
        let voice = self.st.voice;
        self.show(false);
        if !voice.loaded {
            self.flash_meta("VOICE MODELS STILL LOADING");
            return;
        }
        if !voice.stt {
            self.flash_meta("NO SPEECH MODEL · FSC FETCH-MODELS");
            return;
        }
        if self.st.listening.is_some() {
            return;
        }
        // Barge-in: talking over a reply stops it.
        if !follow_up {
            self.interrupt();
        }
        let start_conversation = self.cfg.voice.conversation && !self.st.conversation;
        let st = &mut self.st;
        st.listening = Some(Instant::now());
        st.transcribing = false;
        st.follow_up = follow_up;
        st.viz.set_spectrum(None);
        if start_conversation {
            st.conversation = true;
            self.audio.set_conversation(true);
        }
        if follow_up {
            self.audio.listen_follow_up();
        } else {
            self.audio.listen();
        }
        self.refresh();
    }

    /// `fsc stop` / push-to-talk release.
    fn stop(&mut self) {
        match self.st.listening {
            // A quick tap: keep listening, the VAD ends the utterance.
            Some(t) if t.elapsed() < TAP => {}
            Some(_) => self.audio.finish(),
            None => self.interrupt(),
        }
    }

    /// Push-to-talk released. Only ends a recording: if the VAD already ended
    /// it (you paused before letting go), the question is on its way and must
    /// not be interrupted.
    fn release(&mut self) {
        if let Some(t) = self.st.listening
            && t.elapsed() >= TAP
        {
            self.audio.finish();
        }
    }

    /// Stop the reply: the agent's turn and any speech.
    fn interrupt(&mut self) {
        self.audio.stop_speaking();
        let st = &mut self.st;
        st.sentences.clear();
        st.speech_pending = false;
        if st.claude.busy() {
            self.claude.interrupt();
        }
    }

    fn cancel_listen(&mut self) {
        let st = &mut self.st;
        // Also abandon a transcription in progress: its transcript is then
        // dropped when it arrives instead of being sent.
        let was = st.listening.take().is_some() | std::mem::take(&mut st.transcribing);
        if was {
            st.viz.set_spectrum(None);
            self.audio.cancel();
            self.refresh();
        }
    }

    pub fn new_conversation(&mut self) {
        self.cancel_listen();
        self.audio.stop_speaking();
        self.end_session(false);
        self.history.clear();
        self.effects.push(Effect::HistoryShown(false));
        self.flash_meta("NEW CONVERSATION");
    }

    fn show(&mut self, focus: bool) {
        self.open = true;
        if self.headless() {
            self.push_tray();
            return;
        }
        self.effects.push(Effect::Show { focus });
    }

    pub fn hide(&mut self) {
        // Closing the popup is a "no" to anything still waiting for permission.
        let open: Vec<String> = self.st.asks.iter().map(|a| a.ask.id.clone()).collect();
        for id in open {
            self.claude.answer(&id, Answer::Deny);
            self.close_ask(&id, "✕ DENIED (CLOSED)");
        }
        self.cancel_listen();
        self.end_conversation();
        self.open = false;
        if !self.headless() {
            self.effects.push(Effect::Hide);
        }
    }

    /// The agent wants to use a tool that needs your OK: show a card with
    /// buttons and, in a voice turn, ask out loud and listen for the answer.
    fn on_permission(&mut self, ask: PermissionAsk) {
        if !self.is_open() || self.headless() {
            self.show(false);
            let how = if self.headless() { " (fsc allow / fsc deny)" } else { "" };
            notify(&format!("{} wants to {}.{how}", self.st.agent.name(), ask.action));
        }
        let buttons: Vec<&str> = [("ALLOW", Answer::Once), ("ALWAYS", Answer::Always), ("DENY", Answer::Deny)]
            .into_iter()
            .filter(|(_, a)| *a != Answer::Always || ask.can_always)
            .map(|(t, _)| t)
            .collect();
        let card = Card {
            ask_id: ask.id.clone(),
            action: sentence(&ask.action),
            detail: ask.detail.clone().unwrap_or_default(),
            buttons: buttons.join(","),
            answered: false,
            note: String::new(),
        };
        // The card names the tool itself; drop the plain line just shown for it.
        if let Some(cur) = self.st.current.as_mut()
            && cur.last_tool.as_ref().is_some_and(|(t, _)| *t == ask.tool)
            && let Some((_, line)) = cur.last_tool.take()
        {
            self.history.remove(line);
        }
        // Like tool lines: above the reply text.
        let reply = self.st.current.as_ref().map(|c| c.reply);
        let id = self.history.insert_card(reply, &format!("󰒓  {} · ALLOW?", ask.tool.to_uppercase()), card);
        self.effects.push(Effect::HistoryShown(true));
        self.st.asks.push(OpenAsk { ask: ask.clone(), card: id });
        let first = self.st.asks.len() == 1;
        self.set_claude(Phase::Asking);
        self.scroll_to_end(false);
        if first {
            self.put_question(&ask);
        }
    }

    /// Read the question aloud (when replies are spoken) and listen for the answer.
    fn put_question(&mut self, ask: &PermissionAsk) {
        self.st.answer_tries = 0;
        if self.should_speak() {
            // Finish the sentence in progress first.
            if let Some(rest) = self.st.sentences.flush() {
                self.speak(rest);
            }
            let lang = self.st.spoken.language();
            let question = if lang.is_english() {
                ask.spoken.clone()
            } else {
                // "Google Calendar · create event" -> "Google Calendar".
                lang.question(ask.kind, ask.tool.split(" · ").next().unwrap_or(&ask.tool))
            };
            self.speak(format!("{question} {}", choices(lang).0));
        }
        self.maybe_follow_up();
    }

    /// An answer heard on the mic. "Always" counts as a plain yes: anything
    /// within earshot (a video, a call on speaker) can say it, and a saved
    /// rule outlives the moment. ALWAYS takes a click, or typing it.
    fn spoken_answer(&mut self, id: &str, text: &str) {
        let lang = self.st.spoken.language();
        if let Some(answer) = permission::parse_answer(text, lang) {
            if answer == Answer::Always {
                self.answer_ask_noted(id, Answer::Once, "✓ ALLOWED ONCE · ALWAYS NEEDS A CLICK");
            } else {
                self.answer_ask(id, answer);
            }
            return;
        }
        self.st.answer_tries += 1;
        if self.st.answer_tries < MAX_ANSWER_TRIES {
            let (spoken, meta) = choices(lang);
            self.flash_meta(meta);
            if !text.trim().is_empty() && self.should_speak() {
                self.speak(format!("{} {spoken}", lang.phrases.sorry));
            }
            self.maybe_follow_up();
        } else {
            self.flash_meta_for("ANSWER WITH THE BUTTONS", Duration::from_secs(8));
        }
    }

    fn answer_ask(&mut self, id: &str, answer: Answer) {
        // Nothing can be saved for this request: an "always" is a plain allow.
        let can_always = self.st.asks.iter().find(|a| a.ask.id == id).is_some_and(|a| a.ask.can_always);
        let answer = if answer == Answer::Always && !can_always { Answer::Once } else { answer };
        let note = match answer {
            Answer::Once => "✓ ALLOWED",
            Answer::Always => "✓ ALLOWED · WON'T ASK AGAIN",
            Answer::Deny => "✕ DENIED",
        };
        self.answer_ask_noted(id, answer, note);
    }

    /// Send `answer`, mark the card with `note`, and move on to the next question.
    fn answer_ask_noted(&mut self, id: &str, answer: Answer, note: &str) {
        self.claude.answer(id, answer);
        self.close_ask(id, note);
        match self.st.asks.first().map(|a| a.ask.clone()) {
            Some(ask) => self.put_question(&ask),
            None => {
                if self.st.claude == Phase::Asking {
                    self.set_claude(Phase::Working);
                }
            }
        }
    }

    /// Replace a question's buttons with what became of it.
    fn close_ask(&mut self, id: &str, note: &str) {
        let Some(i) = self.st.asks.iter().position(|a| a.ask.id == id) else { return };
        let open = self.st.asks.remove(i);
        self.st.answer_tries = 0;
        let none_left = self.st.asks.is_empty();
        self.history.update(open.card, |b| {
            b.card.buttons.clear();
            b.card.answered = true;
            b.card.note = note.to_string();
        });
        // Was listening for the answer: don't let that turn into a new prompt.
        if none_left && self.st.listening.is_some() && self.st.claude.busy() {
            self.cancel_listen();
        }
    }

    /// Nobody spoke: end the conversation and, like ending a call, put the
    /// popup away shortly after, unless you've started something meanwhile.
    fn hang_up(&mut self) {
        self.end_conversation();
        self.flash_meta_for("CONVERSATION ENDED", HANG_UP_CLOSE);
        self.timer(HANG_UP_CLOSE, Timer::HangUp);
    }

    /// "Hang up": stop speaking and release the mic so the headset can go
    /// back to its high-quality playback profile. A reply still streaming
    /// finishes as text (with a notification, since the popup is hidden).
    fn end_conversation(&mut self) {
        let st = &mut self.st;
        let was = st.conversation;
        st.conversation = false;
        st.follow_up = false;
        if was {
            st.voice_turn = false;
            st.speech_pending = false;
            st.sentences.clear();
            self.audio.stop_speaking();
            self.audio.set_conversation(false);
        }
    }

    // --- drawing --------------------------------------------------------------

    /// One animation frame: `dt` seconds since the last, the visualiser `w` wide.
    pub fn frame(&mut self, dt: f64, viz_w: f64) {
        let dt = if dt.is_finite() { dt.clamp(0.0, 0.1) } else { 0.016 };
        self.st.clock += dt;
        self.st.viz.tick(dt as f32, viz_w);
    }

    pub fn draw_viz(&self, w: f64, h: f64, scale: f64) -> Option<Frame> {
        let mut c = Canvas::new(w, h, scale)?;
        self.st.viz.draw(&mut c, w, h, &self.st.palette);
        Some(c.frame())
    }

    /// The dot-matrix status line, with a blinking record dot while listening.
    pub fn draw_status(&self, w: f64, h: f64, scale: f64) -> Option<Frame> {
        let mut c = Canvas::new(w, h, scale)?;
        let st = &self.st;
        let p = &st.palette;
        let phase = st.phase();
        let y = (h - (dotfont::ROWS - 1) as f64 * STATUS_PITCH) / 2.0;
        let mut x = STATUS_DOT;
        if phase == Phase::Listening {
            let a = 0.55 + 0.45 * (st.clock * 4.0).sin().abs();
            c.fill_circle(4.0, h / 2.0, 4.0, p.accent, a);
            x += 14.0;
        }
        let on = if matches!(phase, Phase::Error | Phase::Asking) { p.accent } else { p.fg };
        dotfont::draw(&mut c, phase.label(), x, y, STATUS_PITCH, STATUS_DOT, (on, 1.0), (p.fg, 0.10), Some((p.bg, 0.5)));
        Some(c.frame())
    }

    pub fn draw_panel(&self, w: f64, h: f64, regions: panel::Regions, scale: f64) -> Option<Frame> {
        let mut c = Canvas::new(w, h, scale)?;
        panel::draw(&mut c, w, h, &self.st.palette, regions, self.panel_look());
        Some(c.frame())
    }

    fn panel_look(&self) -> panel::Look {
        match self.cfg.ui.surface {
            Surface::Layer => panel::Look { framed: true, opacity: self.cfg.ui.opacity, gradient: self.cfg.ui.gradient },
            // Opaque: Hyprland's `opacity` window rule makes it see-through.
            Surface::Window | Surface::Headless => panel::Look { framed: false, opacity: 1.0, gradient: false },
        }
    }

    // --- state helpers --------------------------------------------------------

    fn set_claude(&mut self, p: Phase) {
        self.st.claude = p;
        self.refresh();
    }

    /// Re-derive the displayed phase and push it everywhere.
    fn refresh(&mut self) {
        self.st.viz.phase = self.st.phase();
        self.push_tray();
    }

    fn set_theme(&mut self, mode: ThemeMode) {
        self.st.theme = mode;
        self.push_tray();
        self.refresh_palette();
    }

    /// Re-read colours from the desktop palette and accent.
    fn refresh_palette(&mut self) {
        let st = &self.st;
        let accent = self.accent_pin.or(st.desktop_accent);
        let palette = Palette::from_system(st.system, accent, st.theme);
        if st.palette != palette {
            self.st.palette = palette;
            self.effects.push(Effect::Palette);
        }
    }

    fn push_tray(&mut self) {
        let st = &mut self.st;
        let state = IconState {
            phase: st.phase(),
            theme: st.theme,
            muted: st.muted,
            accent: style::NOTHING_RED,
            shape: self.cfg.ui.tray_icon,
        };
        // Each update re-renders the icon at 6 sizes and signals the tray host.
        if st.tray_sent != Some(state) {
            st.tray_sent = Some(state);
            let _ = self.tray_tx.send(TrayUpdate::Icon(state));
        }
        let menu = tray::MenuState {
            agent: st.agent,
            agents: st.agents,
            choices: st.choices.clone(),
            mic: st.mic.clone(),
            mic_missing: st.mic_missing,
            speak: st.speak,
            language: st.spoken.code,
            voice: st.wanted.clone().unwrap_or_else(|| st.voice_name.clone()),
            fetching: st.fetching.clone(),
        };
        if st.menu_sent.as_ref() != Some(&menu) {
            st.menu_sent = Some(menu.clone());
            let _ = self.tray_tx.send(TrayUpdate::Menu(menu));
        }
    }

    fn set_meta(&mut self, text: String) {
        if self.meta != text {
            self.meta = text.clone();
            self.effects.push(Effect::Meta(text));
        }
    }

    fn restore_meta(&mut self) {
        let st = &self.st;
        let mode = st.choices.mode.as_ref().map(|m| format!("{}  ·  ", m.current_name().to_uppercase())).unwrap_or_default();
        let text = format!("{}  ·  {mode}⏎ SEND  ·  ESC", st.model);
        self.set_meta(text);
    }

    /// `fsc model` / `fsc mode` and the tray's Model and Mode menus: list
    /// what the agent offers, or switch (saved in the agent's config table).
    fn set_choice(&mut self, which: Setting, value: Option<String>) -> Reply {
        let (agent, sel) = (self.st.agent, self.st.choices.get(which).cloned());
        let what = if which == Setting::Mode { "mode" } else { "model" };
        let Some(value) = value else {
            let Some(sel) = sel else {
                return Reply::err(format!("{} hasn't offered any {what}s (it does once its session is up)", agent.name()));
            };
            let lines: Vec<String> = sel
                .options
                .iter()
                .map(|o| {
                    let mark = if o.id == sel.current { "*" } else { " " };
                    if o.name == o.id { format!("{mark} {}", o.id) } else { format!("{mark} {}  ({})", o.id, o.name) }
                })
                .collect();
            return Reply::state(lines.join("\n"));
        };
        let name = match &sel {
            Some(sel) => match sel.options.iter().find(|o| o.id == value) {
                Some(o) => o.name.clone(),
                None => return Reply::err(format!("{} has no {what} {value:?} (`fsc {what}` lists them)", agent.name())),
            },
            None => value.clone(),
        };
        self.claude.set(which, &value);
        let (table, key) = Config::setting_key(agent, which == Setting::Mode);
        let saved = Config::save_value(table, key, &value);
        if let Err(e) = &saved {
            eprintln!("fsc: could not save the {what}: {e:#}");
        }
        self.flash_meta(&format!("{}: {}", what.to_uppercase(), name.to_uppercase()));
        match saved {
            Ok(()) => Reply::state(format!("{what} is now {name}")),
            Err(e) => Reply::state(format!("{what} is now {name} (not saved: {e:#})")),
        }
    }

    pub fn flash_meta(&mut self, text: &str) {
        self.flash_meta_for(text, Duration::from_millis(3000));
    }

    fn flash_meta_for(&mut self, text: &str, dur: Duration) {
        self.set_meta(text.to_string());
        self.st.meta_gen += 1;
        self.st.meta_flashing = true;
        self.timer(dur, Timer::Meta(self.st.meta_gen));
    }

    /// Show streamed text and the newest of it, once this pass of the event
    /// loop is over: repeated calls before then share that one flush.
    /// `force`: scroll even if you've scrolled up to read something.
    fn scroll_to_end(&mut self, force: bool) {
        self.scroll_force |= force;
        if !std::mem::replace(&mut self.flush_pending, true) {
            let _ = self.events.try_send(AppEvent::Flush);
        }
    }

    fn flush(&mut self) {
        self.flush_pending = false;
        self.show_current();
        let force = std::mem::take(&mut self.scroll_force);
        self.effects.push(Effect::Scroll { force });
    }
}

/// What fits the panel's status line.
fn short_meta(s: &str) -> String {
    const MAX: usize = 44;
    if s.chars().count() <= MAX {
        return s.to_string();
    }
    let mut out: String = s.chars().take(MAX - 1).collect();
    out.push('…');
    out
}

/// "search the web" -> "Search the web."
fn sentence(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => format!("{}{}.", f.to_uppercase(), c.as_str()),
        None => String::new(),
    }
}

/// How to ask for an answer: (spoken, status line). "Always" is never
/// offered by voice: see `spoken_answer`.
fn choices(lang: &Language) -> (&'static str, &'static str) {
    (lang.phrases.yes_no, "SAY YES OR NO")
}
