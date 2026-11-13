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
use fsc_core::claude::{ClaudeEvent, ClaudeHandle, pretty_model, pretty_tool};
use fsc_core::config::{Config, Position, SpeakMode};
use fsc_core::ipc::{Command, Reply};

use crate::AppEvent;
use crate::icon::IconState;
use crate::style::{Palette, ThemeMode};
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
/// How long "CONVERSATION ENDED" shows before the popup closes itself.
const HANG_UP_CLOSE: Duration = Duration::from_secs(3);

struct Exchange {
    reply: gtk::Label,
    text: String,
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
    muted: bool,
    model: String,
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
    claude: ClaudeHandle,
    audio: AudioHandle,
    tray_tx: mpsc::UnboundedSender<IconState>,
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
}

impl Ui {
    pub fn new(
        app: &gtk::Application,
        cfg: Config,
        palette: Palette,
        claude: ClaudeHandle,
        audio: AudioHandle,
        tray_tx: mpsc::UnboundedSender<IconState>,
    ) -> Rc<Self> {
        let window = gtk::ApplicationWindow::builder()
            .application(app)
            .title("Fishsticks & Custard")
            .decorated(false)
            .resizable(false)
            .build();
        window.add_css_class("fsc");

        window.init_layer_shell();
        window.set_namespace(Some("fsc"));
        window.set_layer(Layer::Overlay);
        window.set_keyboard_mode(KeyboardMode::Exclusive);
        if cfg.ui.position == Position::Top {
            window.set_anchor(Edge::Top, true);
            window.set_margin(Edge::Top, cfg.ui.margin);
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

        let model = pretty_model(&cfg.claude.model);
        let ui = Rc::new(Ui {
            cfg,
            claude,
            audio,
            tray_tx,
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
                palette,
                muted: false,
                model,
                voice: Voice::default(),
                viz: Viz::default(),
                last_frame: None,
                current: None,
                voice_turn: false,
                sentences: SentenceBuffer::default(),
                conversation: false,
                speech_pending: false,
                follow_up: false,
                empty_follow_ups: 0,
                session_dirty: false,
                last_activity: None,
                memory_saving: false,
                quitting: false,
                pending_prompt: None,
                tray_sent: None,
                meta_timer: None,
            }),
            scroll_pending: Cell::new(false),
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
                panel::draw(cr, w as f64, h as f64, &ui.st.borrow().palette, regions);
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
            let on = if phase == Phase::Error { p.accent } else { p.fg };
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
                let cols = Viz::columns_for(area.width() as f64);
                st.viz.tick(dt, cols);
            }
            area.queue_draw();
            ui.status.queue_draw();
            glib::ControlFlow::Continue
        });

        let weak = Rc::downgrade(self);
        self.entry.connect_activate(move |e| {
            if let Some(ui) = weak.upgrade() {
                let text = e.text().to_string();
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
            AppEvent::Claude(e) => self.claude_event(e),
            AppEvent::Audio(e) => self.audio_event(e),
            AppEvent::Theme(mode) => self.set_theme(mode),
        }
    }

    pub fn command(self: &Rc<Self>, cmd: Command) -> Reply {
        match cmd {
            Command::Toggle => {
                if self.window.is_visible() { self.hide() } else { self.show(true) }
            }
            Command::Show => self.show(false),
            Command::Hide => self.hide(),
            Command::Type => self.show(true),
            Command::Listen => self.listen(),
            Command::Stop => self.stop(),
            Command::Release => self.release(),
            Command::Ask { text } => {
                if !self.ask(&text, false) {
                    return Reply::err("busy: a reply is still in progress (fsc stop to interrupt)");
                }
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
            Command::Status => {
                let st = self.st.borrow();
                let mut s = st.phase().label().to_lowercase();
                if st.muted {
                    s.push_str(" (muted)");
                }
                return Reply::state(s);
            }
            Command::Memory => {
                let home = self.cfg.claude_home();
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
            Command::Quit => self.quit(),
        }
        Reply::ok()
    }

    fn claude_event(self: &Rc<Self>, e: ClaudeEvent) {
        match e {
            ClaudeEvent::Ready { model, .. } => {
                if !model.is_empty() {
                    self.st.borrow_mut().model = pretty_model(&model);
                    self.restore_meta();
                }
            }
            ClaudeEvent::TextDelta(t) => {
                if self.st.borrow().claude != Phase::Responding {
                    self.set_claude(Phase::Responding);
                }
                let speak = self.should_speak();
                let mut st = self.st.borrow_mut();
                st.viz.kick(0.25 + t.len() as f32 * 0.03);
                if let Some(cur) = st.current.as_mut() {
                    cur.text.push_str(&t);
                    cur.reply.set_text(cur.text.trim_start());
                    cur.reply.set_visible(true);
                }
                let ready = if speak { st.sentences.push(&t) } else { Vec::new() };
                drop(st);
                for s in ready {
                    self.speak(s);
                }
                self.scroll_to_end();
            }
            ClaudeEvent::ToolUse { name } => {
                self.set_claude(Phase::Working);
                if let Some(cur) = self.st.borrow().current.as_ref() {
                    let l = label(&format!("󰒓  {}", pretty_tool(&name).to_uppercase()), "tool");
                    if let Some(parent) = cur.reply.parent() {
                        l.insert_before(&parent, Some(&cur.reply));
                    }
                }
                self.scroll_to_end();
            }
            ClaudeEvent::TurnDone { ok, interrupted, error } => {
                let (cur, rest) = {
                    let mut st = self.st.borrow_mut();
                    let rest = if ok && !interrupted { st.sentences.flush() } else { None };
                    st.sentences.clear();
                    (st.current.take(), rest)
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
                    && !self.window.is_visible()
                    && self.cfg.ui.notify_when_hidden
                    && let Some(cur) = cur
                {
                    notify(&cur.text);
                }
                self.maybe_follow_up();
            }
            ClaudeEvent::Exited { reason } => {
                eprintln!("fsc: {reason}");
            }
            ClaudeEvent::MemorySaved { ok, facts, error } => {
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
            ClaudeEvent::Forgotten => self.flash_meta("MEMORY CLEARED"),
        }
    }

    fn audio_event(self: &Rc<Self>, e: AudioEvent) {
        match e {
            AudioEvent::Ready { stt, tts, missing_models } => {
                self.st.borrow_mut().voice = Voice { stt, tts, loaded: true };
                if missing_models {
                    self.flash_meta("MODELS MISSING · FSC FETCH-MODELS");
                }
            }
            AudioEvent::Spectrum(bands) => {
                self.st.borrow_mut().viz.set_spectrum(Some(bands));
            }
            AudioEvent::MicWaiting => self.flash_meta_for("WAITING FOR MIC…", Duration::from_secs(15)),
            AudioEvent::MicLive => {
                if self.st.borrow().follow_up {
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
            AudioEvent::Error(err) => {
                eprintln!("fsc: {err}");
                let short: String = err.to_uppercase().chars().take(44).collect();
                self.flash_meta(&short);
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
            st.current = Some(Exchange { reply, text: String::new() });
            st.voice_turn = voice;
            st.sentences.clear();
        }
        self.set_claude(Phase::Thinking);
        self.claude.ask(text);
        self.scroll_to_end();
        true
    }

    fn should_speak(&self) -> bool {
        let st = self.st.borrow();
        st.voice.tts
            && match self.cfg.voice.speak {
                SpeakMode::Voice => st.voice_turn,
                SpeakMode::Always => true,
                SpeakMode::Never => false,
            }
    }

    fn session_idle(&self) -> bool {
        let mins = self.cfg.claude.session_idle_minutes;
        let st = self.st.borrow();
        mins > 0
            && st.session_dirty
            && st.last_activity.is_some_and(|t| t.elapsed() >= Duration::from_secs(mins as u64 * 60))
    }

    /// End the Claude session (saving memory) and mark it in the history.
    fn rotate_session(&self) {
        eprintln!("fsc: session idle, rotating");
        self.end_session();
        self.add_divider("NEW SESSION");
    }

    /// Ask for a fresh Claude session, saving memory from this one if it had
    /// any exchanges.
    fn end_session(&self) {
        let mut st = self.st.borrow_mut();
        let save = st.session_dirty;
        self.claude.rotate(save);
        st.memory_saving |= save && self.cfg.claude.memory;
        st.session_dirty = false;
        st.last_activity = None;
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
        let (wait, dirty) = {
            let mut st = self.st.borrow_mut();
            let wait = self.cfg.claude.memory && (st.session_dirty || st.memory_saving) && !st.quitting;
            st.quitting = true;
            (wait, st.session_dirty)
        };
        if !wait {
            self.app.quit();
            return;
        }
        if dirty {
            self.end_session();
        }
        self.flash_meta_for("SAVING MEMORY…", Duration::from_secs(30));
        // Don't hang forever if the save stalls.
        let app = self.app.clone();
        glib::timeout_add_local_once(Duration::from_secs(30), move || app.quit());
    }

    /// "Fishsticks and custard[, <command>]" was heard.
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
            st.conversation
                && !st.claude.busy()
                && !st.speaking
                && !st.speech_pending
                && st.listening.is_none()
                && !st.transcribing
                && st.empty_follow_ups < MAX_EMPTY_FOLLOW_UPS
        };
        if ready && self.window.is_visible() {
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
        self.end_session();
        while let Some(child) = self.history.first_child() {
            self.history.remove(&child);
        }
        self.scroller.set_visible(false);
        self.flash_meta("NEW CONVERSATION");
    }

    fn show(&self, focus: bool) {
        self.window.set_visible(true);
        self.window.present();
        if focus {
            self.entry.grab_focus();
        }
    }

    fn hide(&self) {
        self.cancel_listen();
        self.end_conversation();
        self.window.set_visible(false);
        self.st.borrow_mut().last_frame = None;
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
            if idle && ui.entry.text().is_empty() && ui.window.is_visible() {
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

    fn set_theme(&self, mode: ThemeMode) {
        let palette = {
            let mut st = self.st.borrow_mut();
            st.palette = Palette::new(mode, st.palette.accent);
            st.palette
        };
        self.css.load_from_string(&palette.css(&self.cfg.ui.font));
        self.panel.queue_draw();
        self.viz_area.queue_draw();
        self.status.queue_draw();
        self.push_tray();
    }

    fn push_tray(&self) {
        let mut st = self.st.borrow_mut();
        let state = IconState {
            phase: st.phase(),
            theme: st.palette.mode,
            muted: st.muted,
            accent: st.palette.accent,
            glow: self.cfg.ui.glow,
        };
        // Each update re-renders the icon at 6 sizes and signals the tray host.
        if st.tray_sent != Some(state) {
            st.tray_sent = Some(state);
            let _ = self.tray_tx.send(state);
        }
    }

    fn restore_meta(&self) {
        let model = self.st.borrow().model.clone();
        self.meta.set_text(&format!("{model}  ·  ⏎ SEND  ·  ESC"));
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

    fn scroll_to_end(self: &Rc<Self>) {
        if self.scroll_pending.replace(true) {
            return;
        }
        let weak = Rc::downgrade(self);
        glib::idle_add_local_once(move || {
            if let Some(ui) = weak.upgrade() {
                ui.scroll_pending.set(false);
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
