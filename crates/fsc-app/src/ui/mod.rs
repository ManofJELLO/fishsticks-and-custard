//! The popup: a layer-shell surface with a translucent panel, dot-matrix
//! status and visualiser, the conversation, and a text field.
//!
//! What the popup shows comes from three independent activities, combined by
//! `State::phase()`: voice capture (listening, then transcribing), the
//! Claude turn (thinking, working, responding), and speech playback
//! (speaking).

mod dotfont;
mod panel;
mod speech_text;
mod viz;

use gtk::prelude::*;
use gtk::{gdk, glib, pango};
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

use fsc_audio::{AudioEvent, AudioHandle};
use fsc_core::Phase;
use fsc_core::agent::{AgentEvent, AgentHandle, Choices, Setting, pretty_model, pretty_tool};
use fsc_core::config::{self as config, AgentKind, Config, SpeakMode, Surface};
use fsc_core::lang::{self, Language, Spoken};
use fsc_core::ipc::{Command, Reply};
use fsc_core::permission::{self, Answer, PermissionAsk};

use crate::AppEvent;
use crate::icon::IconState;
use crate::tray::{self, TrayUpdate};
use crate::style::{self, Palette, Rgb, ThemeMode};
use speech_text::SentenceBuffer;
use viz::Viz;

const MAX_EXCHANGES: usize = 20;
const PAD_X: i32 = 24;
const STATUS_PITCH: f64 = 3.0;
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

/// A permission question on screen.
struct OpenAsk {
    ask: PermissionAsk,
    card: gtk::Box,
    buttons: gtk::Box,
}

struct Exchange {
    reply: gtk::Label,
    text: String,
    /// The most recent "⚙ TOOL" line, so a permission card for the same
    /// tool can replace it instead of repeating it.
    last_tool: Option<(String, gtk::Label)>,
    /// `text` has grown since it was last put in `reply`. Streamed text is
    /// shown at most once per main-loop pass rather than on every chunk,
    /// since each `set_text` lays out the whole reply again.
    stale: bool,
}

impl Exchange {
    fn new(reply: gtk::Label) -> Self {
        Self { reply, text: String::new(), last_tool: None, stale: false }
    }

    /// Bring the label up to date with `text`.
    fn show(&mut self) {
        if std::mem::take(&mut self.stale) {
            self.reply.set_text(self.text.trim_start());
            self.reply.set_visible(true);
        }
    }
}

#[derive(Default, Clone, Copy)]
struct Voice {
    stt: bool,
    tts: bool,
    loaded: bool,
}

struct State {
    /// The Claude side: Idle / Thinking / Working / Responding / Error.
    claude: Phase,
    listening: Option<Instant>,
    transcribing: bool,
    speaking: bool,

    palette: Palette,
    /// Light/dark from the desktop (tray icon, and the palette fallback).
    theme: ThemeMode,
    /// The desktop's accent colour, when it sets one.
    desktop_accent: Option<Rgb>,
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
    last_frame: Option<i64>,
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
    /// The Claude session has exchanges since it started (worth saving).
    session_dirty: bool,
    last_activity: Option<Instant>,
    /// A session's memory update was requested and hasn't reported back.
    memory_saving: bool,
    /// Quit requested; waiting for the memory save to finish.
    quitting: bool,
    /// A prompt that arrived while Claude was still busy (e.g. you spoke over
    /// a reply that hadn't finished stopping). Sent when the turn ends.
    pending_prompt: Option<(String, bool)>,
    /// Permission questions waiting for an answer, oldest first.
    asks: Vec<OpenAsk>,
    /// Spoken replies to the current question that weren't yes/always/no.
    answer_tries: u32,
    /// Last state sent to the tray, to skip redundant re-renders.
    tray_sent: Option<IconState>,
    meta_timer: Option<glib::SourceId>,
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

pub struct Ui {
    cfg: Config,
    claude: AgentHandle,
    audio: AudioHandle,
    tray_tx: mpsc::UnboundedSender<TrayUpdate>,
    /// For work done off the main thread (voice downloads) to report back.
    events: async_channel::Sender<crate::AppEvent>,
    app: gtk::Application,
    window: gtk::ApplicationWindow,
    panel: gtk::DrawingArea,
    status: gtk::DrawingArea,
    viz_area: gtk::DrawingArea,
    meta: gtk::Label,
    scroller: gtk::ScrolledWindow,
    history: gtk::Box,
    entry: gtk::Entry,
    css: gtk::CssProvider,
    st: RefCell<State>,
    scroll_pending: Cell<bool>,
    /// The accent pinned in the config (hex), overriding the desktop's.
    accent_pin: Option<Rgb>,
    /// Headless: a voice session is "open" though nothing is shown.
    open: Cell<bool>,
}

impl Ui {
    #[allow(clippy::too_many_arguments)] // each is a separate handle it needs
    pub fn new(
        app: &gtk::Application,
        cfg: Config,
        palette: Palette,
        accent_pin: Option<Rgb>,
        claude: AgentHandle,
        audio: AudioHandle,
        tray_tx: mpsc::UnboundedSender<TrayUpdate>,
        events: async_channel::Sender<crate::AppEvent>,
    ) -> Rc<Self> {
        let window = gtk::ApplicationWindow::builder()
            .application(app)
            .title("Fishsticks & Custard")
            .decorated(false)
            .resizable(false)
            .build();
        window.add_css_class("fsc");

        if cfg.ui.surface == Surface::Layer {
            window.init_layer_shell();
            window.set_namespace(Some("fsc"));
            window.set_layer(Layer::Overlay);
            // On demand, not exclusive: other windows stay usable while it's up.
            window.set_keyboard_mode(KeyboardMode::OnDemand);
            let (top, bottom, left, right) = cfg.ui.anchor.edges();
            for (edge, on, margin) in [
                (Edge::Top, top, cfg.ui.margin),
                (Edge::Bottom, bottom, cfg.ui.margin),
                (Edge::Left, left, cfg.ui.margin_x),
                (Edge::Right, right, cfg.ui.margin_x),
            ] {
                window.set_anchor(edge, on);
                window.set_margin(edge, if on { margin } else { 0 });
            }
            if !cfg.ui.monitor.is_empty() {
                let display = gdk::Display::default().expect("a display");
                let monitors = display.monitors();
                let found = (0..monitors.n_items())
                    .filter_map(|i| monitors.item(i).and_downcast::<gdk::Monitor>())
                    .find(|m| m.connector().is_some_and(|c| c == cfg.ui.monitor));
                match found {
                    Some(m) => window.set_monitor(Some(&m)),
                    None => eprintln!("fsc: monitor {:?} not found; using the focused one", cfg.ui.monitor),
                }
            }
        } else {
            // A normal toplevel: Hyprland matches it by class
            // (dev.fsc.FishsticksAndCustard) for float, size, opacity, etc.
            window.set_default_size(cfg.ui.width, -1);
        }

        // --- widgets ------------------------------------------------------
        let panel = gtk::DrawingArea::new();

        let status = gtk::DrawingArea::new();
        status.set_content_height((dotfont::ROWS as f64 * STATUS_PITCH).ceil() as i32 + 2);
        status.set_hexpand(true);

        let meta = gtk::Label::new(None);
        meta.add_css_class("meta");
        meta.set_valign(gtk::Align::Center);

        let header = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        header.append(&status);
        header.append(&meta);

        let viz_area = gtk::DrawingArea::new();
        viz_area.set_content_height(viz::HEIGHT);
        viz_area.set_hexpand(true);

        let history = gtk::Box::new(gtk::Orientation::Vertical, 16);
        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_height(true)
            .max_content_height(cfg.ui.max_height)
            .child(&history)
            .visible(false)
            .build();

        let entry = gtk::Entry::builder().placeholder_text("Ask anything…").hexpand(true).build();

        let content = gtk::Box::new(gtk::Orientation::Vertical, 16);
        content.set_margin_top(20);
        content.set_margin_bottom(20);
        content.set_margin_start(PAD_X);
        content.set_margin_end(PAD_X);
        content.append(&header);
        content.append(&viz_area);
        content.append(&scroller);
        content.append(&entry);

        let overlay = gtk::Overlay::new();
        overlay.set_size_request(cfg.ui.width, -1);
        overlay.set_child(Some(&panel));
        overlay.add_overlay(&content);
        overlay.set_measure_overlay(&content, true);
        window.set_child(Some(&overlay));

        // --- style ------------------------------------------------------------
        let css = gtk::CssProvider::new();
        css.load_from_string(&palette.css(&cfg.ui.font));
        gtk::style_context_add_provider_for_display(
            &gdk::Display::default().expect("a display"),
            &css,
            // Above USER: ~/.config/gtk-4.0/gtk.css is often the full desktop
            // theme (focus rings etc.). Every rule is scoped to `.fsc`, so this
            // only affects the popup.
            gtk::STYLE_PROVIDER_PRIORITY_USER + 1,
        );

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
        let ui = Rc::new(Ui {
            cfg,
            claude,
            audio,
            tray_tx,
            events,
            app: app.clone(),
            window,
            panel,
            status,
            viz_area,
            meta,
            scroller,
            history,
            entry,
            css,
            st: RefCell::new(State {
                claude: Phase::Idle,
                listening: None,
                transcribing: false,
                speaking: false,
                theme: palette.mode,
                palette,
                desktop_accent: None,
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
                last_frame: None,
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
                meta_timer: None,
            }),
            scroll_pending: Cell::new(false),
            accent_pin,
            open: Cell::new(false),
        });
        ui.wire();
        ui.restore_meta();
        ui
    }

    fn wire(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        self.panel.set_draw_func(move |_, cr, w, h| {
            if let Some(ui) = weak.upgrade() {
                let regions = ui.panel_regions(h as f64);
                panel::draw(cr, w as f64, h as f64, &ui.st.borrow().palette, regions, ui.panel_look());
            }
        });

        let weak = Rc::downgrade(self);
        self.viz_area.set_draw_func(move |_, cr, w, h| {
            if let Some(ui) = weak.upgrade() {
                let st = ui.st.borrow();
                st.viz.draw(cr, w as f64, h as f64, &st.palette);
            }
        });

        let weak = Rc::downgrade(self);
        self.status.set_draw_func(move |_, cr, _w, h| {
            let Some(ui) = weak.upgrade() else { return };
            let st = ui.st.borrow();
            let p = &st.palette;
            let phase = st.phase();
            let y = (h as f64 - (dotfont::ROWS - 1) as f64 * STATUS_PITCH) / 2.0;
            let mut x = STATUS_DOT;
            if phase == Phase::Listening {
                // Blinking record dot.
                let t = st.last_frame.map(|t| t as f64 / 1e6).unwrap_or(0.0);
                let a = 0.55 + 0.45 * (t * 4.0).sin().abs();
                cr.set_source_rgba(p.accent.0, p.accent.1, p.accent.2, a);
                cr.arc(4.0, h as f64 / 2.0, 4.0, 0.0, std::f64::consts::TAU);
                cr.fill().ok();
                x += 14.0;
            }
            let on = if matches!(phase, Phase::Error | Phase::Asking) { p.accent } else { p.fg };
            dotfont::draw(cr, phase.label(), x, y, STATUS_PITCH, STATUS_DOT, (on, 1.0), (p.fg, 0.10), Some((p.bg, 0.5)));
        });

        // Animation: only runs while the popup is mapped.
        let weak = Rc::downgrade(self);
        self.viz_area.add_tick_callback(move |area, clock| {
            let Some(ui) = weak.upgrade() else { return glib::ControlFlow::Break };
            let now = clock.frame_time();
            {
                let mut st = ui.st.borrow_mut();
                let dt = st.last_frame.map(|t| ((now - t) as f32 / 1e6).clamp(0.0, 0.1)).unwrap_or(0.016);
                st.last_frame = Some(now);
                st.viz.tick(dt, area.width() as f64);
            }
            area.queue_draw();
            ui.status.queue_draw();
            glib::ControlFlow::Continue
        });

        let weak = Rc::downgrade(self);
        self.entry.connect_activate(move |e| {
            if let Some(ui) = weak.upgrade() {
                let text = e.text().to_string();
                // "y" / "yes" / "always" / "no" answers an open permission question.
                let open = ui.st.borrow().asks.first().map(|a| a.ask.id.clone());
                let lang = ui.st.borrow().spoken.language();
                if let (Some(id), Some(answer)) = (open, permission::parse_answer(&text, lang)) {
                    e.set_text("");
                    ui.answer_ask(&id, answer);
                    return;
                }
                if ui.ask(&text, false) {
                    e.set_text("");
                }
            }
        });

        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = Rc::downgrade(self);
        keys.connect_key_pressed(move |_, key, _, mods| {
            let Some(ui) = weak.upgrade() else { return glib::Propagation::Proceed };
            let ctrl = mods.contains(gdk::ModifierType::CONTROL_MASK);
            match key {
                gdk::Key::Escape => {
                    ui.hide();
                    glib::Propagation::Stop
                }
                // Ctrl+C with nothing selected stops everything, like a terminal.
                gdk::Key::c if ctrl && ui.entry.selection_bounds().is_none() => {
                    ui.cancel_listen();
                    ui.interrupt();
                    glib::Propagation::Stop
                }
                gdk::Key::n if ctrl => {
                    ui.new_conversation();
                    glib::Propagation::Stop
                }
                _ => glib::Propagation::Proceed,
            }
        });
        self.window.add_controller(keys);

        // Closing the window (e.g. Hyprland's killactive) hides it; the
        // assistant keeps running in the tray.
        let weak = Rc::downgrade(self);
        self.window.connect_close_request(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.hide();
            }
            glib::Propagation::Stop
        });

        // Follow the GTK theme: re-read its colours once styled, and again
        // whenever GTK's theme settings change.
        let weak = Rc::downgrade(self);
        self.window.connect_realize(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.refresh_palette();
            }
        });
        if let Some(settings) = gtk::Settings::default() {
            let weak = Rc::downgrade(self);
            settings.connect_notify_local(None, move |_, _| {
                if let Some(ui) = weak.upgrade() {
                    ui.refresh_palette_soon();
                }
            });
        }
    }

    // --- events -----------------------------------------------------------

    pub fn handle(self: &Rc<Self>, ev: AppEvent) {
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
                self.st.borrow_mut().desktop_accent = c;
                self.refresh_palette();
            }
        }
    }

    pub fn command(self: &Rc<Self>, cmd: Command) -> Reply {
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
                let Some(id) = self.st.borrow().asks.first().map(|a| a.ask.id.clone()) else {
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
                let muted = {
                    let mut st = self.st.borrow_mut();
                    st.muted = on.unwrap_or(!st.muted);
                    st.muted
                };
                self.audio.set_wake_enabled(!muted);
                self.flash_meta(if muted { "WAKE WORD MUTED" } else { "WAKE WORD ON" });
                self.push_tray();
            }
            Command::Wake { phrase: None } => return Reply::state(self.st.borrow().wake_phrase.clone()),
            Command::Wake { phrase: Some(phrase) } => return self.set_wake_phrase(&phrase),
            Command::Status => {
                let st = self.st.borrow();
                let mut s = st.phase().label().to_lowercase();
                if st.muted {
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
                let mut st = self.st.borrow_mut();
                st.session_dirty = false;
                st.last_activity = None;
                // Forgetting cancels any save in flight; it won't report back.
                st.memory_saving = false;
            }
            Command::Agent { backend: None } => return Reply::state(self.st.borrow().agent.key()),
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

    fn agent_event(self: &Rc<Self>, e: AgentEvent) {
        match e {
            AgentEvent::Ready { model, .. } => {
                if !model.is_empty() {
                    self.st.borrow_mut().model = pretty_model(&model);
                    self.restore_meta();
                }
            }
            AgentEvent::TextDelta(t) => {
                if self.st.borrow().claude != Phase::Responding {
                    self.set_claude(Phase::Responding);
                }
                let speak = self.should_speak();
                let mut st = self.st.borrow_mut();
                st.viz.kick(0.25 + t.len() as f32 * 0.03);
                if let Some(cur) = st.current.as_mut() {
                    cur.text.push_str(&t);
                    cur.stale = true;
                }
                let ready = if speak { st.sentences.push(&t) } else { Vec::new() };
                drop(st);
                for s in ready {
                    self.speak(s);
                }
                self.scroll_to_end();
            }
            AgentEvent::ToolUse { name } => {
                if self.st.borrow().asks.is_empty() {
                    self.set_claude(Phase::Working);
                }
                if let Some(cur) = self.st.borrow_mut().current.as_mut() {
                    let tool = pretty_tool(&name);
                    let l = label(&format!("󰒓  {}", tool.to_uppercase()), "tool");
                    if let Some(parent) = cur.reply.parent() {
                        l.insert_before(&parent, Some(&cur.reply));
                    }
                    cur.last_tool = Some((tool, l));
                }
                self.scroll_to_end();
            }
            AgentEvent::Permission(ask) => self.on_permission(ask),
            AgentEvent::PermissionClosed { id } => self.close_ask(&id, "— WITHDRAWN"),
            AgentEvent::TurnDone { ok, interrupted, error } => {
                // Questions still open when the turn ends were never answered.
                let open: Vec<String> = self.st.borrow().asks.iter().map(|a| a.ask.id.clone()).collect();
                for id in open {
                    self.close_ask(&id, "— NOT ANSWERED");
                }
                let (cur, rest) = {
                    let mut st = self.st.borrow_mut();
                    let rest = if ok && !interrupted { st.sentences.flush() } else { None };
                    st.sentences.clear();
                    let mut cur = st.current.take();
                    cur.iter_mut().for_each(Exchange::show);
                    (cur, rest)
                };
                if let Some(rest) = rest
                    && self.should_speak()
                {
                    self.speak(rest);
                }
                if let Some(cur) = &cur {
                    if cur.text.trim().is_empty() {
                        cur.reply.set_visible(false);
                    }
                    if let Some(parent) = cur.reply.parent().and_downcast::<gtk::Box>() {
                        if interrupted {
                            parent.append(&label("— STOPPED", "note"));
                        } else if ok && cur.text.trim().is_empty() {
                            // Some agents end a turn silently, e.g. after a refusal.
                            parent.append(&label("— NO REPLY", "note"));
                        }
                        if let Some(err) = &error {
                            parent.append(&label(&format!("! {err}"), "error"));
                        }
                    }
                }
                self.set_claude(if ok { Phase::Idle } else { Phase::Error });
                // Not `session_dirty`: that's set when a prompt is sent, since
                // a rotation also ends (interrupts) the turn it cuts short.
                self.st.borrow_mut().last_activity = Some(Instant::now());
                self.scroll_to_end();

                let pending = self.st.borrow_mut().pending_prompt.take();
                if let Some((text, voice)) = pending {
                    self.ask(&text, voice);
                    return;
                }
                let spoken = self.st.borrow().voice_turn && self.should_speak();
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
                self.st.borrow_mut().memory_saving = false;
                if ok {
                    self.flash_meta(&format!("MEMORY UPDATED · {facts} FACTS"));
                } else {
                    eprintln!("fsc: memory not updated: {}", error.unwrap_or_default());
                    self.flash_meta("MEMORY NOT UPDATED");
                }
                if self.st.borrow().quitting {
                    self.app.quit();
                }
            }
            AgentEvent::Forgotten => self.flash_meta("MEMORY CLEARED"),
            AgentEvent::Notice(text) => {
                eprintln!("fsc: {text}");
                self.flash_meta_for(&text.to_uppercase(), Duration::from_secs(6));
            }
            AgentEvent::Choices(c) => {
                {
                    let mut st = self.st.borrow_mut();
                    if let Some(m) = &c.model
                        && !m.current.is_empty()
                    {
                        let name = m.current_name();
                        st.model = if name == m.current { pretty_model(name) } else { name.to_uppercase() };
                    }
                    st.choices = c;
                }
                if self.st.borrow().meta_timer.is_none() {
                    self.restore_meta();
                }
                self.push_tray();
            }
        }
    }

    fn audio_event(self: &Rc<Self>, e: AudioEvent) {
        match e {
            AudioEvent::Ready { stt, tts, missing_models } => {
                self.st.borrow_mut().voice = Voice { stt, tts, loaded: true };
                let voice = self.st.borrow().voice_name.clone();
                if missing_models && !fsc_core::models::has_voice(&voice) && stt {
                    // Only the voice is missing (a new language, or the
                    // locale's after an update): fetch it now.
                    self.fetch_voice(&voice);
                } else if missing_models {
                    self.flash_meta("MODELS MISSING · FSC FETCH-MODELS");
                }
            }
            AudioEvent::Voice { voice, ok } => {
                let sample = {
                    let mut st = self.st.borrow_mut();
                    if voice != st.voice_name {
                        return; // superseded by another choice
                    }
                    st.voice.tts = ok;
                    (ok && st.sample_for.as_deref() == Some(voice.as_str())).then(|| {
                        st.sample_for = None;
                        st.spoken.language().phrases.sample
                    })
                };
                if let Some(sample) = sample {
                    // Just chosen: let it say hello, whatever `speak` is set to.
                    self.audio.speak(sample);
                } else if !ok && self.st.borrow().fetching.is_none() {
                    self.flash_meta_for(&format!("VOICE {} UNAVAILABLE", voice.to_uppercase()), Duration::from_secs(6));
                }
                self.push_tray();
            }
            AudioEvent::Spectrum(bands) => {
                self.st.borrow_mut().viz.set_spectrum(Some(bands));
            }
            AudioEvent::MicWaiting => self.flash_meta_for("WAITING FOR MIC…", Duration::from_secs(15)),
            AudioEvent::MicLive => {
                let open = self.st.borrow().asks.first().map(|a| a.ask.can_always);
                if let Some(can_always) = open {
                    self.flash_meta_for(choices(lang::english(), can_always).1, Duration::from_secs(8));
                } else if self.st.borrow().follow_up {
                    self.flash_meta_for("GO AHEAD · ESC ENDS CONVERSATION", Duration::from_secs(5));
                } else {
                    self.flash_meta("SPEAK NOW");
                }
            }
            AudioEvent::SpeechStarted => {}
            AudioEvent::Transcribing => {
                {
                    let mut st = self.st.borrow_mut();
                    st.listening = None;
                    st.transcribing = true;
                    st.viz.set_spectrum(None);
                }
                self.refresh();
            }
            AudioEvent::Transcript(text) => {
                let was_capturing = {
                    let mut st = self.st.borrow_mut();
                    let was = st.listening.is_some() || st.transcribing;
                    st.listening = None;
                    st.transcribing = false;
                    st.viz.set_spectrum(None);
                    was
                };
                self.refresh();
                if !was_capturing {
                    return; // cancelled meanwhile
                }
                let open = self.st.borrow().asks.first().map(|a| a.ask.id.clone());
                if let Some(id) = open {
                    self.spoken_answer(&id, &text);
                    return;
                }
                if text.trim().is_empty() {
                    let gave_up = {
                        let mut st = self.st.borrow_mut();
                        st.empty_follow_ups += 1;
                        st.conversation && st.empty_follow_ups >= MAX_EMPTY_FOLLOW_UPS
                    };
                    if gave_up {
                        // Nobody's talking: hang up so the wake word works again.
                        self.hang_up();
                    } else {
                        self.flash_meta("DIDN'T CATCH THAT");
                        self.maybe_follow_up();
                    }
                } else {
                    self.st.borrow_mut().empty_follow_ups = 0;
                    self.ask_or_queue(text, true);
                }
            }
            AudioEvent::NobodySpoke => {
                {
                    let mut st = self.st.borrow_mut();
                    st.listening = None;
                    st.transcribing = false;
                    st.viz.set_spectrum(None);
                }
                self.refresh();
                self.hang_up();
            }
            AudioEvent::SpeakingStarted => {
                self.st.borrow_mut().speaking = true;
                self.refresh();
            }
            AudioEvent::SpeakingDone => {
                {
                    let mut st = self.st.borrow_mut();
                    st.speaking = false;
                    st.speech_pending = false;
                    st.viz.set_spectrum(None);
                }
                self.refresh();
                self.maybe_release_mic();
                self.maybe_follow_up();
            }
            AudioEvent::Wake { command } => self.on_wake(command),
            AudioEvent::MicMissing(name) => {
                let was = std::mem::replace(&mut self.st.borrow_mut().mic_missing, name.is_some());
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

    // --- actions ----------------------------------------------------------

    /// Send a prompt. `voice`: it came from speech, so speak the reply.
    fn ask(self: &Rc<Self>, text: &str, voice: bool) -> bool {
        let text = text.trim();
        if text.is_empty() {
            return true;
        }
        if self.st.borrow().claude.busy() {
            self.flash_meta("BUSY · CTRL+C TO STOP");
            return false;
        }
        self.audio.stop_speaking();
        // Been a while? Start a fresh session (saving memory) before this
        // prompt; the session actor holds the prompt until it's ready.
        if self.session_idle() {
            self.rotate_session();
        }
        self.st.borrow_mut().session_dirty = true;

        let ex = gtk::Box::new(gtk::Orientation::Vertical, 6);
        let prefix = if voice { "󰍬" } else { "›" };
        ex.append(&label(&format!("{prefix} {text}"), "prompt"));
        let reply = label("", "reply");
        reply.set_selectable(true);
        reply.set_focusable(false);
        reply.set_visible(false);
        ex.append(&reply);
        self.history.append(&ex);
        self.trim_history();
        self.scroller.set_visible(true);

        {
            let mut st = self.st.borrow_mut();
            if let Some(old) = st.current.as_mut() {
                old.show();
            }
            st.current = Some(Exchange::new(reply));
            st.voice_turn = voice;
            st.sentences.clear();
        }
        self.set_claude(Phase::Thinking);
        match self.st.borrow_mut().language_note.take() {
            Some(note) => self.claude.ask(format!("({note})\n\n{text}")),
            None => self.claude.ask(text),
        }
        self.scroll_to_end();
        true
    }

    fn should_speak(&self) -> bool {
        let st = self.st.borrow();
        st.voice.tts
            && match st.speak {
                SpeakMode::Voice => st.voice_turn,
                SpeakMode::Always => true,
                SpeakMode::Never => false,
            }
    }

    fn session_idle(&self) -> bool {
        let mins = self.cfg.session_idle_minutes();
        let st = self.st.borrow();
        mins > 0
            && st.session_dirty
            && st.last_activity.is_some_and(|t| t.elapsed() >= Duration::from_secs(mins as u64 * 60))
    }

    /// End the Claude session (saving memory) and mark it in the history.
    fn rotate_session(&self) {
        eprintln!("fsc: session idle, rotating");
        self.end_session(false);
        self.add_divider("NEW SESSION");
    }

    /// End the Claude session, saving memory from it if it had any
    /// exchanges. A fresh session is started unless `quitting`.
    fn end_session(&self, quitting: bool) {
        let mut st = self.st.borrow_mut();
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

    /// Hand the conversation to another agent: this session ends (saving
    /// memory) and the next prompt goes to `kind`. Remembered in the config.
    /// Listen for `phrase` from now on, and save it as `voice.wake_phrase`.
    fn set_wake_phrase(self: &Rc<Self>, phrase: &str) -> Reply {
        let phrase = match fsc_audio::check_wake_phrase(phrase) {
            Ok(p) => p,
            Err(e) => return Reply::err(e),
        };
        self.audio.set_wake_phrase(phrase.clone());
        self.st.borrow_mut().wake_phrase = phrase.clone();
        self.flash_meta(&format!("WAKE PHRASE: {}", phrase.to_uppercase()));
        match Config::save_value("voice", "wake_phrase", &phrase) {
            Ok(()) => Reply::state(format!("listening for {phrase:?}")),
            Err(e) => Reply::state(format!("listening for {phrase:?} until fscd restarts (not saved: {e:#})")),
        }
    }

    /// `fsc mic` and the tray's Microphone menu: list the mics, or listen
    /// with another (saved as `voice.input_device`).
    fn set_mic(self: &Rc<Self>, name: Option<String>) -> Reply {
        let current = self.st.borrow().mic.clone();
        let is_default = |s: &str| s.trim().is_empty() || s.trim().eq_ignore_ascii_case("default");
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
                Some(m) => (m.id.clone(), m.name.clone()),
                None => (name.to_string(), name.to_string()),
            }
        };
        self.audio.set_input_device(setting.clone());
        {
            let mut st = self.st.borrow_mut();
            st.mic = setting.clone();
            // The audio thread says if the new one is missing.
            st.mic_missing = false;
        }
        self.push_tray();
        let flash = if setting.is_empty() { "SYSTEM DEFAULT".to_string() } else { shown.to_uppercase() };
        self.flash_meta(&short_meta(&format!("MIC: {flash}")));
        match Config::save_value("voice", "input_device", &setting) {
            Ok(()) => Reply::state(format!("listening with {shown}")),
            Err(e) => Reply::state(format!("listening with {shown} until fscd restarts (not saved: {e:#})")),
        }
    }

    /// `fsc speak` and the tray's Speak replies menu (saved as `voice.speak`).
    fn set_speak(self: &Rc<Self>, mode: Option<String>) -> Reply {
        let Some(mode) = mode else { return Reply::state(self.st.borrow().speak.key()) };
        let Some(mode) = SpeakMode::from_key(&mode) else {
            return Reply::err(format!("speak {mode:?}? Use voice (replies to spoken prompts), always or never"));
        };
        self.st.borrow_mut().speak = mode;
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
                Ok(path) => ("xdg-open", vec![path.display().to_string()], path.display().to_string()),
                Err(e) => return Reply::err(format!("no config file: {e:#}")),
            }
        };
        use std::os::unix::process::CommandExt;
        // Its own process group, so it outlives fscd and isn't sent fscd's
        // signals; reaped in the background.
        let child = std::process::Command::new(prog)
            .args(&args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .process_group(0)
            .spawn();
        match child {
            Ok(mut c) => {
                std::thread::spawn(move || c.wait());
                Reply::state(format!("opened {what}"))
            }
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
    fn set_language(self: &Rc<Self>, code: Option<String>) -> Reply {
        let Some(code) = code else {
            let st = self.st.borrow();
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
        let piper_voice = config::voice_for(&spoken, &self.st.borrow().piper_voice);
        let saved = Config::save_value("voice", "language", &setting)
            .and_then(|()| Config::save_value("voice", "piper_voice", &piper_voice));
        let changed = {
            let mut st = self.st.borrow_mut();
            let changed = st.spoken != spoken;
            st.language = setting;
            st.spoken = spoken.clone();
            st.piper_voice = piper_voice;
            st.sentences.set_language(lang);
            changed
        };
        if changed {
            let instruction = spoken.instruction();
            let note = instruction.clone().unwrap_or_else(|| "The user now speaks English. Reply in English.".into());
            // Sessions from now on are told in their instructions; the one
            // running started with the old language, so it's told with the
            // next prompt.
            self.claude.set_language(instruction);
            self.st.borrow_mut().language_note = Some(note);
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
    fn set_voice(self: &Rc<Self>, name: Option<String>) -> Reply {
        let (code, current) = {
            let st = self.st.borrow();
            (st.spoken.code, st.voice_name.clone())
        };
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
            let st = self.st.borrow();
            st.spoken.language().voice(&st.spoken.region).to_string()
        } else {
            name
        };
        if !setting.is_empty() {
            if setting.split('-').count() != 3 {
                return Reply::err(format!("{setting:?} isn't a Piper voice name (like de_DE-thorsten-medium; `fsc voice` lists them)"));
            }
            if config::voice_language(&setting) != code && self.st.borrow().spoken.whisper == code {
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
            let mut st = self.st.borrow_mut();
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
        let coming = self.st.borrow().wanted.is_some();
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
    fn apply_voice(self: &Rc<Self>, sample: bool) -> String {
        let here = |v: &str| fsc_core::models::has_voice(v);
        let (whisper, voice, speak_with) = {
            let mut st = self.st.borrow_mut();
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
    fn fetch_voice(self: &Rc<Self>, voice: &str) {
        {
            let mut st = self.st.borrow_mut();
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
                        let _ = tx.send_blocking(crate::AppEvent::VoiceFetch { voice: voice.clone(), progress: Ok(Some(pct)) });
                    }
                });
                if let Err(e) = r {
                    result = Err(format!("{e:#}"));
                    break;
                }
            }
            let _ = tx.send_blocking(crate::AppEvent::VoiceFetch { voice, progress: result });
        });
    }

    fn voice_fetch(self: &Rc<Self>, voice: String, progress: Result<Option<u32>, String>) {
        let current = {
            let st = self.st.borrow();
            st.wanted.as_deref() == Some(voice.as_str()) || st.voice_name == voice
        };
        match progress {
            Ok(Some(pct)) => {
                {
                    let mut st = self.st.borrow_mut();
                    if current && st.fetching.is_none() {
                        st.fetching = Some((voice.clone(), pct));
                    }
                    if let Some(f) = st.fetching.as_mut().filter(|(v, _)| *v == voice) {
                        f.1 = pct;
                    }
                }
                if current {
                    self.flash_meta_for(&format!("DOWNLOADING VOICE · {pct}%"), Duration::from_secs(60));
                }
            }
            Ok(None) => {
                eprintln!("fsc: downloaded voice {voice}");
                self.end_fetch(&voice);
                if current {
                    let unsaved = {
                        let mut st = self.st.borrow_mut();
                        st.wanted = None;
                        st.voice_name = voice.clone();
                        st.unsaved.take_if(|(v, _)| *v == voice)
                    };
                    if unsaved.is_some()
                        && let Err(e) = Config::save_value("voice", "piper_voice", &voice)
                    {
                        eprintln!("fsc: could not save the voice: {e:#}");
                    }
                    let whisper = self.st.borrow().spoken.whisper.clone();
                    self.audio.set_language(whisper, voice);
                    self.restore_meta();
                }
            }
            Err(e) => {
                eprintln!("fsc: could not download voice {voice}: {e}");
                self.end_fetch(&voice);
                {
                    // Back to what was there: the voice in use speaks on.
                    let mut st = self.st.borrow_mut();
                    if st.wanted.as_deref() == Some(voice.as_str()) {
                        st.wanted = None;
                    }
                    if let Some((_, previous)) = st.unsaved.take_if(|(v, _)| *v == voice) {
                        st.piper_voice = previous;
                    }
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
    fn end_fetch(&self, voice: &str) {
        let mut st = self.st.borrow_mut();
        st.downloads.retain(|v| v != voice);
        if st.fetching.as_ref().is_some_and(|(v, _)| v == voice) {
            st.fetching = None;
        }
    }

    fn switch_agent(self: &Rc<Self>, kind: AgentKind) -> Reply {
        // Reread the config so edits to the agent's own table count.
        let mut cfg = Config::load().unwrap_or_else(|e| {
            eprintln!("fsc: {e:#}; using the settings fscd started with");
            self.cfg.clone()
        });
        let agents = AgentKind::ALL.map(|k| cfg.available(k));
        self.st.borrow_mut().agents = agents;
        if kind == self.st.borrow().agent {
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
            let mut st = self.st.borrow_mut();
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
    pub fn tick_idle(&self) {
        let quiet = {
            let st = self.st.borrow();
            !st.claude.busy() && !st.conversation && st.listening.is_none() && !st.speaking && !st.transcribing
        };
        if quiet && self.session_idle() {
            self.rotate_session();
        }
    }

    fn add_divider(&self, text: &str) {
        if self.history.first_child().is_none() {
            return;
        }
        let time = glib::DateTime::now_local().ok().and_then(|t| t.format("%H:%M").ok()).unwrap_or_default();
        self.history.append(&label(&format!("— {text} · {time} —"), "note"));
        self.trim_history();
    }

    /// Quit, saving memory first if this session had any exchanges (or
    /// waiting for a save already under way).
    fn quit(self: &Rc<Self>) {
        let wait = {
            let mut st = self.st.borrow_mut();
            let wait = (st.memory_on && st.session_dirty || st.memory_saving) && !st.quitting;
            st.quitting = true;
            wait
        };
        if !wait {
            self.app.quit();
            return;
        }
        // Also when a save is already running (from an idle rotation), so
        // no new session is started after it.
        self.end_session(true);
        self.flash_meta_for("SAVING MEMORY…", Duration::from_secs(30));
        // Don't hang forever if the save stalls.
        let app = self.app.clone();
        glib::timeout_add_local_once(Duration::from_secs(30), move || app.quit());
    }

    /// The wake phrase was heard ("fishsticks[, <command>]").
    fn on_wake(self: &Rc<Self>, command: Option<String>) {
        self.show(false);
        let start_conversation = self.cfg.voice.conversation && !self.st.borrow().conversation;
        {
            let mut st = self.st.borrow_mut();
            st.empty_follow_ups = 0;
            if start_conversation {
                st.conversation = true;
            }
        }
        if start_conversation {
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
    fn ask_or_queue(self: &Rc<Self>, text: String, voice: bool) {
        if self.st.borrow().claude.busy() {
            self.st.borrow_mut().pending_prompt = Some((text, voice));
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
        let done = {
            let st = self.st.borrow();
            !st.conversation && !st.claude.busy() && !st.speaking && !st.speech_pending && st.listening.is_none()
        };
        if done {
            self.audio.release_mic();
        }
    }

    fn speak(&self, text: String) {
        self.st.borrow_mut().speech_pending = true;
        self.audio.speak(text);
    }

    /// In a conversation, start listening again once the reply is fully
    /// done: Claude's turn finished and nothing left to say.
    fn maybe_follow_up(self: &Rc<Self>) {
        let ready = {
            let st = self.st.borrow();
            // A permission question asked by voice: listen for the answer.
            let answering = !st.asks.is_empty() && st.voice_turn && st.answer_tries < MAX_ANSWER_TRIES;
            let next_question = st.conversation && !st.claude.busy() && st.empty_follow_ups < MAX_EMPTY_FOLLOW_UPS;
            (answering || next_question)
                && !st.speaking
                && !st.speech_pending
                && st.listening.is_none()
                && !st.transcribing
        };
        if ready && self.is_open() {
            self.listen_inner(true);
        }
    }

    /// User-initiated listening (hotkey, tray, `fsc listen`).
    fn listen(self: &Rc<Self>) {
        self.st.borrow_mut().empty_follow_ups = 0;
        self.listen_inner(false);
    }

    fn listen_inner(self: &Rc<Self>, follow_up: bool) {
        let voice = self.st.borrow().voice;
        self.show(false);
        if !voice.loaded {
            self.flash_meta("VOICE MODELS STILL LOADING");
            return;
        }
        if !voice.stt {
            self.flash_meta("NO SPEECH MODEL · FSC FETCH-MODELS");
            return;
        }
        if self.st.borrow().listening.is_some() {
            return;
        }
        // Barge-in: talking over a reply stops it.
        if !follow_up {
            self.interrupt();
        }
        let start_conversation = self.cfg.voice.conversation && !self.st.borrow().conversation;
        {
            let mut st = self.st.borrow_mut();
            st.listening = Some(Instant::now());
            st.transcribing = false;
            st.follow_up = follow_up;
            st.viz.set_spectrum(None);
            if start_conversation {
                st.conversation = true;
            }
        }
        if start_conversation {
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
    fn stop(self: &Rc<Self>) {
        let listening = self.st.borrow().listening;
        match listening {
            // A quick tap: keep listening, the VAD ends the utterance.
            Some(t) if t.elapsed() < TAP => {}
            Some(_) => self.audio.finish(),
            None => self.interrupt(),
        }
    }

    /// Push-to-talk released. Only ends a recording: if the VAD already ended
    /// it (you paused before letting go), the question is on its way and must
    /// not be interrupted.
    fn release(self: &Rc<Self>) {
        let listening = self.st.borrow().listening;
        if let Some(t) = listening
            && t.elapsed() >= TAP
        {
            self.audio.finish();
        }
    }

    /// Stop the reply: Claude's turn and any speech.
    fn interrupt(&self) {
        self.audio.stop_speaking();
        let mut st = self.st.borrow_mut();
        st.sentences.clear();
        st.speech_pending = false;
        if st.claude.busy() {
            self.claude.interrupt();
        }
    }

    fn cancel_listen(&self) {
        let was = {
            let mut st = self.st.borrow_mut();
            // Also abandon a transcription in progress: its transcript is then
            // dropped when it arrives instead of being sent.
            let was = st.listening.take().is_some() | std::mem::take(&mut st.transcribing);
            if was {
                st.viz.set_spectrum(None);
            }
            was
        };
        if was {
            self.audio.cancel();
            self.refresh();
        }
    }

    fn new_conversation(self: &Rc<Self>) {
        self.cancel_listen();
        self.audio.stop_speaking();
        self.end_session(false);
        while let Some(child) = self.history.first_child() {
            self.history.remove(&child);
        }
        self.scroller.set_visible(false);
        self.flash_meta("NEW CONVERSATION");
    }

    fn headless(&self) -> bool {
        self.cfg.ui.surface == Surface::Headless
    }

    /// The popup is up, or, headless, a voice session is running.
    fn is_open(&self) -> bool {
        if self.headless() { self.open.get() } else { self.window.is_visible() }
    }

    fn show(&self, focus: bool) {
        self.open.set(true);
        if self.headless() {
            self.push_tray();
            return;
        }
        self.refresh_palette();
        self.window.set_visible(true);
        self.window.present();
        if focus {
            self.entry.grab_focus();
        }
    }

    fn hide(&self) {
        // Closing the popup is a "no" to anything still waiting for permission.
        let open: Vec<String> = self.st.borrow().asks.iter().map(|a| a.ask.id.clone()).collect();
        for id in open {
            self.claude.answer(&id, Answer::Deny);
            self.close_ask(&id, "✕ DENIED (CLOSED)");
        }
        self.cancel_listen();
        self.end_conversation();
        self.open.set(false);
        self.window.set_visible(false);
        self.st.borrow_mut().last_frame = None;
    }

    /// Claude wants to use a tool that needs your OK: show a card with
    /// buttons and, in a voice turn, ask out loud and listen for the answer.
    fn on_permission(self: &Rc<Self>, ask: PermissionAsk) {
        if !self.is_open() || self.headless() {
            self.show(false);
            let how = if self.headless() { " (fsc allow / fsc deny)" } else { "" };
            notify(&format!("{} wants to {}.{how}", self.st.borrow().agent.name(), ask.action));
        }
        let card = gtk::Box::new(gtk::Orientation::Vertical, 6);
        card.add_css_class("ask");
        card.append(&label(&format!("󰒓  {} · ALLOW?", ask.tool.to_uppercase()), "tool"));
        card.append(&label(&sentence(&ask.action), "ask-action"));
        if let Some(d) = &ask.detail {
            let l = label(d, "ask-detail");
            l.set_selectable(true);
            l.set_focusable(false);
            card.append(&l);
        }
        let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        for (text, answer) in [("ALLOW", Answer::Once), ("ALWAYS", Answer::Always), ("DENY", Answer::Deny)] {
            if answer == Answer::Always && !ask.can_always {
                continue;
            }
            let b = gtk::Button::with_label(text);
            b.add_css_class("ask-button");
            if answer == Answer::Once {
                b.add_css_class("allow");
            }
            b.set_focus_on_click(false);
            let weak = Rc::downgrade(self);
            let id = ask.id.clone();
            b.connect_clicked(move |_| {
                if let Some(ui) = weak.upgrade() {
                    ui.answer_ask(&id, answer);
                }
            });
            buttons.append(&b);
        }
        card.append(&buttons);

        let first = {
            let mut st = self.st.borrow_mut();
            // The card names the tool itself; drop the plain line just shown for it.
            if let Some(cur) = st.current.as_mut()
                && cur.last_tool.as_ref().is_some_and(|(t, _)| *t == ask.tool)
                && let Some((_, l)) = cur.last_tool.take()
                && let Some(parent) = l.parent().and_downcast::<gtk::Box>()
            {
                parent.remove(&l);
            }
            match st.current.as_ref().and_then(|c| c.reply.parent().map(|p| (p, c.reply.clone()))) {
                // Like tool labels: above the reply text.
                Some((parent, reply)) => card.insert_before(&parent, Some(&reply)),
                None => self.history.append(&card),
            }
            st.asks.push(OpenAsk { ask: ask.clone(), card, buttons });
            st.asks.len() == 1
        };
        self.set_claude(Phase::Asking);
        self.scroll_to_end();
        if first {
            self.put_question(&ask);
        }
    }

    /// Read the question aloud (when replies are spoken) and listen for the answer.
    fn put_question(self: &Rc<Self>, ask: &PermissionAsk) {
        self.st.borrow_mut().answer_tries = 0;
        if self.should_speak() {
            // Finish the sentence in progress first.
            let rest = self.st.borrow_mut().sentences.flush();
            if let Some(rest) = rest {
                self.speak(rest);
            }
            let lang = self.st.borrow().spoken.language();
            let question = if lang.is_english() {
                ask.spoken.clone()
            } else {
                // "Google Calendar · create event" -> "Google Calendar".
                lang.question(ask.kind, ask.tool.split(" · ").next().unwrap_or(&ask.tool))
            };
            self.speak(format!("{question} {}", choices(lang, ask.can_always).0));
        }
        self.maybe_follow_up();
    }

    fn spoken_answer(self: &Rc<Self>, id: &str, text: &str) {
        let lang = self.st.borrow().spoken.language();
        if let Some(answer) = permission::parse_answer(text, lang) {
            self.answer_ask(id, answer);
            return;
        }
        let tries = {
            let mut st = self.st.borrow_mut();
            st.answer_tries += 1;
            st.answer_tries
        };
        if tries < MAX_ANSWER_TRIES {
            let can_always = self.st.borrow().asks.iter().find(|a| a.ask.id == id).is_some_and(|a| a.ask.can_always);
            let lang = self.st.borrow().spoken.language();
            let (spoken, meta) = choices(lang, can_always);
            self.flash_meta(meta);
            if !text.trim().is_empty() && self.should_speak() {
                self.speak(format!("{} {spoken}", lang.phrases.sorry));
            }
            self.maybe_follow_up();
        } else {
            self.flash_meta_for("ANSWER WITH THE BUTTONS", Duration::from_secs(8));
        }
    }

    fn answer_ask(self: &Rc<Self>, id: &str, answer: Answer) {
        // Nothing can be saved for this request: an "always" is a plain allow.
        let can_always = self.st.borrow().asks.iter().find(|a| a.ask.id == id).is_some_and(|a| a.ask.can_always);
        let answer = if answer == Answer::Always && !can_always { Answer::Once } else { answer };
        self.claude.answer(id, answer);
        let note = match answer {
            Answer::Once => "✓ ALLOWED",
            Answer::Always => "✓ ALLOWED · WON'T ASK AGAIN",
            Answer::Deny => "✕ DENIED",
        };
        self.close_ask(id, note);
        let next = self.st.borrow().asks.first().map(|a| a.ask.clone());
        match next {
            Some(ask) => self.put_question(&ask),
            None => {
                if self.st.borrow().claude == Phase::Asking {
                    self.set_claude(Phase::Working);
                }
            }
        }
    }

    /// Replace a question's buttons with what became of it.
    fn close_ask(&self, id: &str, note: &str) {
        let (open, none_left) = {
            let mut st = self.st.borrow_mut();
            let Some(i) = st.asks.iter().position(|a| a.ask.id == id) else { return };
            let open = st.asks.remove(i);
            st.answer_tries = 0;
            (open, st.asks.is_empty())
        };
        open.card.remove(&open.buttons);
        open.card.add_css_class("answered");
        open.card.append(&label(note, "note"));
        // Was listening for the answer: don't let that turn into a new prompt.
        if none_left && self.st.borrow().listening.is_some() && self.st.borrow().claude.busy() {
            self.cancel_listen();
        }
    }

    /// Nobody spoke: end the conversation and, like ending a call, put the
    /// popup away shortly after, unless you've started something meanwhile.
    fn hang_up(self: &Rc<Self>) {
        self.end_conversation();
        self.flash_meta_for("CONVERSATION ENDED", HANG_UP_CLOSE);
        let weak = Rc::downgrade(self);
        glib::timeout_add_local_once(HANG_UP_CLOSE, move || {
            let Some(ui) = weak.upgrade() else { return };
            let idle = {
                let st = ui.st.borrow();
                !st.conversation
                    && st.listening.is_none()
                    && !st.transcribing
                    && !st.claude.busy()
                    && !st.speaking
                    && !st.speech_pending
            };
            if idle && ui.entry.text().is_empty() && ui.is_open() {
                ui.hide();
            }
        });
    }

    /// "Hang up": stop speaking and release the mic so the headset can go
    /// back to its high-quality playback profile. A reply still streaming
    /// finishes as text (with a notification, since the popup is hidden).
    fn end_conversation(&self) {
        let was = {
            let mut st = self.st.borrow_mut();
            let was = st.conversation;
            st.conversation = false;
            st.follow_up = false;
            if was {
                st.voice_turn = false;
                st.speech_pending = false;
                st.sentences.clear();
            }
            was
        };
        if was {
            self.audio.stop_speaking();
            self.audio.set_conversation(false);
        }
    }

    // --- state helpers --------------------------------------------------------

    /// Where the header, visualiser and text sit, in panel coordinates.
    fn panel_regions(&self, h: f64) -> panel::Regions {
        let bounds = |w: &gtk::Widget| w.compute_bounds(&self.panel).map(|b| (b.y() as f64, (b.y() + b.height()) as f64));
        let (_, header_bottom) = bounds(self.status.upcast_ref()).unwrap_or((0.0, 40.0));
        let (graphic_top, graphic_bottom) =
            bounds(self.viz_area.upcast_ref()).unwrap_or((header_bottom + 16.0, header_bottom + 104.0));
        let text_widget: &gtk::Widget =
            if self.scroller.is_visible() { self.scroller.upcast_ref() } else { self.entry.upcast_ref() };
        let (text_top, _) = bounds(text_widget).unwrap_or((graphic_bottom + 16.0, h));
        panel::Regions { header_bottom, graphic_top, graphic_bottom, text_top }
    }

    fn set_claude(&self, p: Phase) {
        self.st.borrow_mut().claude = p;
        self.refresh();
    }

    /// Re-derive the displayed phase and push it everywhere.
    fn refresh(&self) {
        {
            let mut st = self.st.borrow_mut();
            st.viz.phase = st.phase();
        }
        self.status.queue_draw();
        self.push_tray();
    }

    fn set_theme(self: &Rc<Self>, mode: ThemeMode) {
        self.st.borrow_mut().theme = mode;
        self.push_tray();
        // GTK reloads its theme for the new scheme in its own time.
        self.refresh_palette_soon();
    }

    fn panel_look(&self) -> panel::Look {
        match self.cfg.ui.surface {
            Surface::Layer => panel::Look { framed: true, opacity: self.cfg.ui.opacity, gradient: self.cfg.ui.gradient },
            // Opaque: Hyprland's `opacity` window rule makes it see-through.
            Surface::Window | Surface::Headless => panel::Look { framed: false, opacity: 1.0, gradient: false },
        }
    }

    /// Re-read colours from the GTK theme and the desktop accent.
    fn refresh_palette(&self) {
        let palette = {
            let st = self.st.borrow();
            let accent = self.accent_pin.or(st.desktop_accent);
            Palette::from_theme(self.window.upcast_ref(), accent, st.theme)
        };
        if self.st.borrow().palette == palette {
            return;
        }
        self.st.borrow_mut().palette = palette;
        self.css.load_from_string(&palette.css(&self.cfg.ui.font));
        self.panel.queue_draw();
        self.viz_area.queue_draw();
        self.status.queue_draw();
    }

    fn refresh_palette_soon(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        glib::timeout_add_local_once(Duration::from_millis(250), move || {
            if let Some(ui) = weak.upgrade() {
                ui.refresh_palette();
            }
        });
    }

    fn push_tray(&self) {
        let mut st = self.st.borrow_mut();
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

    fn restore_meta(&self) {
        let st = self.st.borrow();
        let mode = st.choices.mode.as_ref().map(|m| format!("{}  ·  ", m.current_name().to_uppercase())).unwrap_or_default();
        self.meta.set_text(&format!("{}  ·  {mode}⏎ SEND  ·  ESC", st.model));
    }

    /// `fsc model` / `fsc mode` and the tray's Model and Mode menus: list
    /// what the agent offers, or switch (saved in the agent's config table).
    fn set_choice(self: &Rc<Self>, which: Setting, value: Option<String>) -> Reply {
        let (agent, sel) = {
            let st = self.st.borrow();
            (st.agent, st.choices.get(which).cloned())
        };
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

    fn flash_meta(self: &Rc<Self>, text: &str) {
        self.flash_meta_for(text, Duration::from_millis(3000));
    }

    fn flash_meta_for(self: &Rc<Self>, text: &str, dur: Duration) {
        self.meta.set_text(text);
        let weak = Rc::downgrade(self);
        let id = glib::timeout_add_local_once(dur, move || {
            if let Some(ui) = weak.upgrade() {
                ui.st.borrow_mut().meta_timer = None;
                ui.restore_meta();
            }
        });
        if let Some(old) = self.st.borrow_mut().meta_timer.replace(id) {
            old.remove();
        }
    }

    fn trim_history(&self) {
        let mut n = 0;
        let mut child = self.history.first_child();
        while let Some(c) = child {
            n += 1;
            child = c.next_sibling();
        }
        while n > MAX_EXCHANGES {
            if let Some(first) = self.history.first_child() {
                self.history.remove(&first);
            }
            n -= 1;
        }
    }

    /// On the next idle pass, show any streamed reply text and scroll to the
    /// bottom. Repeated calls before then share that one pass.
    fn scroll_to_end(self: &Rc<Self>) {
        if self.scroll_pending.replace(true) {
            return;
        }
        let weak = Rc::downgrade(self);
        glib::idle_add_local_once(move || {
            if let Some(ui) = weak.upgrade() {
                ui.scroll_pending.set(false);
                if let Some(cur) = ui.st.borrow_mut().current.as_mut() {
                    cur.show();
                }
                let adj = ui.scroller.vadjustment();
                adj.set_value(adj.upper() - adj.page_size());
            }
        });
    }
}

fn label(text: &str, class: &str) -> gtk::Label {
    let l = gtk::Label::new(Some(text));
    l.add_css_class(class);
    l.set_xalign(0.0);
    l.set_wrap(true);
    l.set_wrap_mode(pango::WrapMode::WordChar);
    // Keep the natural width small so long text wraps instead of widening the popup.
    l.set_max_width_chars(1);
    l.set_hexpand(true);
    l
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

fn notify(text: &str) {
    let mut body: String = text.trim().chars().take(280).collect();
    if text.trim().chars().count() > 280 {
        body.push('…');
    }
    if body.is_empty() {
        return;
    }
    tokio::spawn(async move {
        let _ = tokio::process::Command::new("notify-send")
            .args(["--app-name=Fishsticks & Custard", "Fishsticks & Custard", &body])
            .status()
            .await;
    });
}

/// How to ask for an answer: (spoken, status line). "Always" is only offered
/// when the request comes with a rule that can be saved.
fn choices(lang: &Language, can_always: bool) -> (&'static str, &'static str) {
    if can_always {
        (lang.phrases.yes_always_no, "SAY YES, ALWAYS OR NO")
    } else {
        (lang.phrases.yes_no, "SAY YES OR NO")
    }
}
