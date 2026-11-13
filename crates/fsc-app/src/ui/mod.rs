//! The popup: a layer-shell surface with a fluted-glass panel, dot-matrix
//! status and visualiser, the conversation, and a text field.

mod dotfont;
mod glass;
mod viz;

use gtk::prelude::*;
use gtk::{gdk, glib, pango};
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;
use tokio::sync::mpsc;

use fsc_core::Phase;
use fsc_core::claude::{ClaudeEvent, ClaudeHandle, pretty_model, pretty_tool};
use fsc_core::config::{Config, Position};
use fsc_core::ipc::{Command, Reply};

use crate::AppEvent;
use crate::icon::IconState;
use crate::style::{Palette, ThemeMode};
use viz::Viz;

const MAX_EXCHANGES: usize = 20;
const PAD_X: i32 = 24;
const STATUS_PITCH: f64 = 3.0;
const STATUS_DOT: f64 = 1.15;

struct Exchange {
    reply: gtk::Label,
    text: String,
}

struct State {
    phase: Phase,
    palette: Palette,
    muted: bool,
    model: String,
    viz: Viz,
    last_frame: Option<i64>,
    current: Option<Exchange>,
    listen_timer: Option<glib::SourceId>,
    meta_timer: Option<glib::SourceId>,
}

pub struct Ui {
    cfg: Config,
    claude: ClaudeHandle,
    tray_tx: mpsc::UnboundedSender<IconState>,
    app: gtk::Application,
    window: gtk::ApplicationWindow,
    glass: gtk::DrawingArea,
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
        let glass = gtk::DrawingArea::new();

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
        overlay.set_child(Some(&glass));
        overlay.add_overlay(&content);
        overlay.set_measure_overlay(&content, true);
        window.set_child(Some(&overlay));

        // --- style ------------------------------------------------------------
        let css = gtk::CssProvider::new();
        css.load_from_string(&palette.css(&cfg.ui.font));
        gtk::style_context_add_provider_for_display(
            &gdk::Display::default().expect("a display"),
            &css,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );

        let model = pretty_model(&cfg.claude.model);
        let ui = Rc::new(Ui {
            cfg,
            claude,
            tray_tx,
            app: app.clone(),
            window,
            glass,
            status,
            viz_area,
            meta,
            scroller,
            history,
            entry,
            css,
            st: RefCell::new(State {
                phase: Phase::Idle,
                palette,
                muted: false,
                model,
                viz: Viz::default(),
                last_frame: None,
                current: None,
                listen_timer: None,
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
        self.glass.set_draw_func(move |_, cr, w, h| {
            if let Some(ui) = weak.upgrade() {
                glass::draw(cr, w as f64, h as f64, &ui.st.borrow().palette);
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
            let y = (h as f64 - (dotfont::ROWS - 1) as f64 * STATUS_PITCH) / 2.0;
            let mut x = STATUS_DOT;
            if st.phase == Phase::Listening {
                // Blinking record dot.
                let a = 0.55 + 0.45 * (st.viz_time() * 4.0).sin().abs();
                cr.set_source_rgba(p.accent.0, p.accent.1, p.accent.2, a);
                cr.arc(4.0, h as f64 / 2.0, 4.0, 0.0, std::f64::consts::TAU);
                cr.fill().ok();
                x += 14.0;
            }
            let on = if st.phase == Phase::Error { p.accent } else { p.fg };
            dotfont::draw(cr, st.phase.label(), x, y, STATUS_PITCH, STATUS_DOT, (on, 1.0), (p.fg, 0.07));
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
                if ui.ask(&text) {
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
                // Ctrl+C with nothing selected stops, like a terminal.
                gdk::Key::c if ctrl && ui.entry.selection_bounds().is_none() => {
                    ui.stop();
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
            Command::Ask { text } => {
                if !self.ask(&text) {
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
                self.flash_meta(if muted { "WAKE WORD MUTED" } else { "WAKE WORD ON" });
                self.push_tray();
            }
            Command::Status => {
                let st = self.st.borrow();
                let mut s = st.phase.label().to_lowercase();
                if st.muted {
                    s.push_str(" (muted)");
                }
                return Reply::state(s);
            }
            Command::Quit => self.app.quit(),
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
                if self.phase() != Phase::Responding {
                    self.set_phase(Phase::Responding);
                }
                let mut st = self.st.borrow_mut();
                st.viz.kick(0.25 + t.len() as f32 * 0.03);
                if let Some(cur) = st.current.as_mut() {
                    cur.text.push_str(&t);
                    cur.reply.set_text(cur.text.trim_start());
                    cur.reply.set_visible(true);
                }
                drop(st);
                self.scroll_to_end();
            }
            ClaudeEvent::ToolUse { name } => {
                self.set_phase(Phase::Working);
                if let Some(cur) = self.st.borrow().current.as_ref() {
                    let l = label(&format!("󰒓  {}", pretty_tool(&name).to_uppercase()), "tool");
                    if let Some(parent) = cur.reply.parent() {
                        l.insert_before(&parent, Some(&cur.reply));
                    }
                }
                self.scroll_to_end();
            }
            ClaudeEvent::TurnDone { ok, interrupted, error } => {
                let cur = self.st.borrow_mut().current.take();
                if let Some(cur) = &cur {
                    let parent = cur.reply.parent().and_downcast::<gtk::Box>();
                    if cur.text.trim().is_empty() {
                        cur.reply.set_visible(false);
                    }
                    if let Some(parent) = parent {
                        if interrupted {
                            parent.append(&label("— STOPPED", "note"));
                        }
                        if let Some(err) = &error {
                            parent.append(&label(&format!("! {err}"), "error"));
                        }
                    }
                }
                self.set_phase(if ok { Phase::Idle } else { Phase::Error });
                self.scroll_to_end();

                if ok
                    && !interrupted
                    && !self.window.is_visible()
                    && self.cfg.ui.notify_when_hidden
                    && let Some(cur) = cur
                {
                    notify(&cur.text);
                }
            }
            ClaudeEvent::Exited { reason } => {
                eprintln!("fsc: {reason}");
            }
        }
    }

    // --- actions ----------------------------------------------------------

    fn ask(self: &Rc<Self>, text: &str) -> bool {
        let text = text.trim();
        if text.is_empty() {
            return true;
        }
        if self.phase().busy() {
            self.flash_meta("BUSY · CTRL+C TO STOP");
            return false;
        }
        self.cancel_listen();

        let ex = gtk::Box::new(gtk::Orientation::Vertical, 6);
        ex.append(&label(&format!("› {text}"), "prompt"));
        let reply = label("", "reply");
        reply.set_selectable(true);
        reply.set_focusable(false);
        reply.set_visible(false);
        ex.append(&reply);
        self.history.append(&ex);
        self.trim_history();
        self.scroller.set_visible(true);

        self.st.borrow_mut().current = Some(Exchange { reply, text: String::new() });
        self.set_phase(Phase::Thinking);
        self.claude.ask(text);
        self.scroll_to_end();
        true
    }

    fn listen(self: &Rc<Self>) {
        if self.phase().busy() {
            self.claude.interrupt();
        }
        self.show(false);
        self.set_phase(Phase::Listening);
        self.flash_meta("VOICE INPUT ARRIVES IN STEP 2");
        // Demo only: until audio exists, stop "listening" after a few seconds.
        let weak = Rc::downgrade(self);
        let id = glib::timeout_add_local_once(Duration::from_secs(6), move || {
            if let Some(ui) = weak.upgrade() {
                ui.st.borrow_mut().listen_timer = None;
                if ui.phase() == Phase::Listening {
                    ui.set_phase(Phase::Idle);
                }
            }
        });
        if let Some(old) = self.st.borrow_mut().listen_timer.replace(id) {
            old.remove();
        }
    }

    fn cancel_listen(&self) {
        if let Some(id) = self.st.borrow_mut().listen_timer.take() {
            id.remove();
        }
        if self.phase() == Phase::Listening {
            self.set_phase(Phase::Idle);
        }
    }

    fn stop(self: &Rc<Self>) {
        let phase = self.phase();
        if phase == Phase::Listening {
            self.cancel_listen();
        } else if phase.busy() {
            self.claude.interrupt();
        }
    }

    fn new_conversation(self: &Rc<Self>) {
        self.cancel_listen();
        self.claude.restart();
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
        self.window.set_visible(false);
        self.st.borrow_mut().last_frame = None;
    }

    // --- state helpers --------------------------------------------------------

    fn phase(&self) -> Phase {
        self.st.borrow().phase
    }

    fn set_phase(&self, p: Phase) {
        {
            let mut st = self.st.borrow_mut();
            st.phase = p;
            st.viz.phase = p;
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
        self.glass.queue_draw();
        self.viz_area.queue_draw();
        self.status.queue_draw();
        self.push_tray();
    }

    fn push_tray(&self) {
        let st = self.st.borrow();
        let _ = self.tray_tx.send(IconState {
            phase: st.phase,
            theme: st.palette.mode,
            muted: st.muted,
            accent: st.palette.accent,
            glow: self.cfg.ui.glow,
        });
    }

    fn restore_meta(&self) {
        let model = self.st.borrow().model.clone();
        self.meta.set_text(&format!("{model}  ·  ⏎ SEND  ·  ESC"));
    }

    fn flash_meta(self: &Rc<Self>, text: &str) {
        self.meta.set_text(text);
        let weak = Rc::downgrade(self);
        let id = glib::timeout_add_local_once(Duration::from_millis(2500), move || {
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

impl State {
    fn viz_time(&self) -> f64 {
        self.last_frame.map(|t| t as f64 / 1e6).unwrap_or(0.0)
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
