//! The only bridge between QML and the controller.
//!
//! QML sees one singleton, `Backend`: properties for what the popup shows,
//! signals for changes to the conversation list and for things to do
//! (show, hide, scroll), and invokables for what you do in it. All the
//! logic is in `controller`; this file only carries things across.
//!
//! `Raster` is an item that shows one of the parts drawn in Rust (panel,
//! status line, visualiser): QML says which, and calls `update()` when
//! `Backend` says it has been drawn again.

#[cxx_qt::bridge]
pub mod qobject {
    unsafe extern "C++" {
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
        include!("cxx-qt-lib/qcolor.h");
        type QColor = cxx_qt_lib::QColor;
        include!("cxx-qt-lib/qpainter.h");
        type QPainter = cxx_qt_lib::QPainter;
        include!(<QtQuick/QQuickPaintedItem>);
        type QQuickPaintedItem;
    }

    unsafe extern "C++" {
        include!("fsc-app/cpp/fsc_qt.h");

        #[namespace = "fsc"]
        fn set_clipboard(text: &QString);

        /// Set a LayerShellQt window's margins.
        #[namespace = "fsc"]
        unsafe fn set_margins(layer_window: *mut QObject, left: i32, top: i32, right: i32, bottom: i32);

        #[namespace = "fsc"]
        fn quit_app();

        /// macOS: the popup floats on every Space and over full-screen apps;
        /// with `activate`, fscd becomes the active app. Nothing elsewhere.
        #[namespace = "fsc"]
        unsafe fn float_window(window: *mut QObject, activate: bool);

        #[namespace = "fsc"]
        fn set_icon_name(name: &QString);

        /// macOS 27: keep Qt's tray from crashing on a click (see the header).
        #[namespace = "fsc"]
        fn patch_tray_clicks() -> bool;
    }

    extern "RustQt" {
        #[qobject]
        #[qml_element]
        #[qml_singleton]
        #[qproperty(QString, meta)]
        #[qproperty(bool, shown)]
        #[qproperty(bool, history_shown, cxx_name = "historyShown")]
        /// A reply is on its way (the popup keeps room for it).
        #[qproperty(bool, busy)]
        /// A voice is loaded: replies' speakers can read them aloud.
        #[qproperty(bool, can_speak, cxx_name = "canSpeak")]
        #[qproperty(QColor, fg)]
        #[qproperty(QColor, bg)]
        #[qproperty(QColor, accent)]
        #[qproperty(QColor, accent_text, cxx_name = "accentText")]
        #[qproperty(QColor, error)]
        #[qproperty(QString, font_family, cxx_name = "fontFamily")]
        #[qproperty(QString, surface)]
        #[qproperty(i32, popup_width, cxx_name = "popupWidth")]
        #[qproperty(i32, max_height, cxx_name = "maxHeight")]
        #[qproperty(i32, anchors)]
        #[qproperty(QString, monitor)]
        #[qproperty(i32, viz_height, cxx_name = "vizHeight")]
        #[qproperty(i32, status_height, cxx_name = "statusHeight")]
        #[qproperty(QString, shot)]
        /// The screen's device pixel ratio, for drawing sharp on HiDPI.
        #[qproperty(f64, scale)]
        /// The Qt tray (`TrayIcon.qml`): its icon (a file URL), whether the
        /// system tints it (a macOS template image), tooltip, and menu (JSON
        /// from `tray_qt`).
        #[qproperty(QString, tray_icon, cxx_name = "trayIcon")]
        #[qproperty(bool, tray_mask, cxx_name = "trayMask")]
        #[qproperty(QString, tray_tip, cxx_name = "trayTip")]
        #[qproperty(QString, tray_menu, cxx_name = "trayMenu")]
        type Backend = super::BackendRust;
    }

    extern "RustQt" {
        /// A block was added to the conversation at `index` (JSON).
        #[qsignal]
        #[cxx_name = "historyInsert"]
        fn history_insert(self: Pin<&mut Backend>, index: i32, block: &QString);

        /// The block at `index` changed (JSON).
        #[qsignal]
        #[cxx_name = "historyUpdate"]
        fn history_update(self: Pin<&mut Backend>, index: i32, block: &QString);

        #[qsignal]
        #[cxx_name = "historyRemove"]
        fn history_remove(self: Pin<&mut Backend>, index: i32, count: i32);

        #[qsignal]
        #[cxx_name = "historyClear"]
        fn history_clear(self: Pin<&mut Backend>);

        /// Show the newest text; `force`: even if scrolled up to read.
        #[qsignal]
        #[cxx_name = "scrollToEnd"]
        fn scroll_to_end(self: Pin<&mut Backend>, force: bool);

        /// Drop the highlight in the line last clicked.
        #[qsignal]
        #[cxx_name = "clearPick"]
        fn clear_pick(self: Pin<&mut Backend>);

        /// Put the popup up; `focus`: with the text field focused.
        #[qsignal]
        #[cxx_name = "showPopup"]
        fn show_popup(self: Pin<&mut Backend>, focus: bool);

        #[qsignal]
        #[cxx_name = "hidePopup"]
        fn hide_popup(self: Pin<&mut Backend>);

        /// A drawn part ("panel", "status", "viz") has a new frame.
        #[qsignal]
        fn repaint(self: Pin<&mut Backend>, part: &QString);
    }

    extern "RustQt" {
        /// Enter in the text field. True: clear it.
        #[qinvokable]
        fn submit(self: Pin<&mut Backend>, text: &QString) -> bool;

        /// A permission card's button.
        #[qinvokable]
        fn answer(self: Pin<&mut Backend>, ask_id: &QString, label: &QString);

        /// Escape, or the window closed by the compositor.
        #[qinvokable]
        fn close(self: Pin<&mut Backend>);

        /// Ctrl+C with nothing highlighted.
        #[qinvokable]
        #[cxx_name = "stopAll"]
        fn stop_all(self: Pin<&mut Backend>);

        #[qinvokable]
        #[cxx_name = "newConversation"]
        fn new_conversation(self: Pin<&mut Backend>);

        #[qinvokable]
        #[cxx_name = "copyHistory"]
        fn copy_history(self: Pin<&mut Backend>);

        /// Copy `text` (a highlight) and say so.
        #[qinvokable]
        #[cxx_name = "copyText"]
        fn copy_text(self: Pin<&mut Backend>, text: &QString);

        /// A reply's speaker: read it aloud, or stop reading it.
        #[qinvokable]
        #[cxx_name = "readAloud"]
        fn read_aloud(self: Pin<&mut Backend>, key: f64);

        /// The Qt tray's menu is opening: look for microphones again.
        #[qinvokable]
        #[cxx_name = "trayOpening"]
        fn tray_opening(self: Pin<&mut Backend>);

        /// A Qt tray menu entry was chosen (its `act`).
        #[qinvokable]
        #[cxx_name = "trayActivate"]
        fn tray_activate(self: Pin<&mut Backend>, act: i32);

        #[qinvokable]
        #[cxx_name = "setEntryEmpty"]
        fn set_entry_empty(self: Pin<&mut Backend>, empty: bool);

        /// One animation frame: `dt` seconds since the last. Draws the
        /// visualiser and the status line at their sizes.
        #[qinvokable]
        fn frame(self: Pin<&mut Backend>, dt: f64, viz_w: f64, viz_h: f64, status_w: f64, status_h: f64);

        /// Draw the panel `w` x `h`, its regions in panel coordinates.
        #[qinvokable]
        #[cxx_name = "paintPanel"]
        fn paint_panel(
            self: Pin<&mut Backend>,
            w: f64,
            h: f64,
            header_bottom: f64,
            viz_top: f64,
            viz_bottom: f64,
            text_top: f64,
        );

        /// The desktop palette's window, text and accent colours.
        #[qinvokable]
        #[cxx_name = "setSystemColors"]
        fn set_system_colors(self: Pin<&mut Backend>, bg: &QColor, fg: &QColor, accent: &QColor);

        /// macOS: float the popup's window (see `fsc::float_window`).
        #[qinvokable]
        #[cxx_name = "floatWindow"]
        unsafe fn float_window_from_qml(self: Pin<&mut Backend>, window: *mut QObject, activate: bool);

        /// Give the popup's LayerShellQt window (`LayerShell.Window`) its margins.
        #[qinvokable]
        #[cxx_name = "layerMargins"]
        unsafe fn layer_margins(self: Pin<&mut Backend>, layer_window: *mut QObject);
    }

    impl cxx_qt::Initialize for Backend {}
    impl cxx_qt::Threading for Backend {}

    extern "RustQt" {
        #[qobject]
        #[base = QQuickPaintedItem]
        #[qml_element]
        #[qproperty(QString, part)]
        type Raster = super::RasterRust;

        #[cxx_override]
        #[cxx_name = "paint"]
        unsafe fn paint(self: Pin<&mut Raster>, painter: *mut QPainter);
    }

    impl cxx_qt::Initialize for Raster {}
}

use core::pin::Pin;
use std::cell::RefCell;

use cxx_qt::{CxxQtType, Threading};
use cxx_qt_lib::{QColor, QImage, QImageFormat, QRect, QString};

use crate::AppEvent;
use crate::controller::{self, Controller, Effect};
use crate::draw::{self, Part, panel};
use crate::history::Op;
use crate::style::{Rgb, SystemColors};

/// What `main` hands the singleton when QML first asks for it.
pub struct Startup {
    pub ctl: Controller,
    pub events: async_channel::Receiver<AppEvent>,
}

thread_local! {
    /// Set by `main` before the interface loads; taken by `Backend`.
    pub static STARTUP: RefCell<Option<Startup>> = const { RefCell::new(None) };
}

#[derive(Default)]
pub struct BackendRust {
    meta: QString,
    shown: bool,
    history_shown: bool,
    busy: bool,
    can_speak: bool,
    fg: QColor,
    bg: QColor,
    accent: QColor,
    accent_text: QColor,
    error: QColor,
    font_family: QString,
    surface: QString,
    popup_width: i32,
    max_height: i32,
    anchors: i32,
    monitor: QString,
    viz_height: i32,
    status_height: i32,
    /// `FSC_SHOT`: render the popup to this PNG and quit (a development aid
    /// for checking the look without a compositor or taking focus).
    shot: QString,
    scale: f64,
    tray_icon: QString,
    tray_mask: bool,
    tray_tip: QString,
    tray_menu: QString,
    tray: crate::tray_qt::QtTray,
    ctl: Option<Controller>,
}

fn qcolor(c: Rgb) -> QColor {
    QColor::from_rgb_f(c.0 as f32, c.1 as f32, c.2 as f32)
}

fn rgb(c: &QColor) -> Option<Rgb> {
    c.is_valid().then(|| Rgb(c.red_f() as f64, c.green_f() as f64, c.blue_f() as f64))
}

impl cxx_qt::Initialize for qobject::Backend {
    fn initialize(mut self: Pin<&mut Self>) {
        let Some(start) = STARTUP.with(|s| s.borrow_mut().take()) else {
            eprintln!("fsc: the interface started without the assistant behind it");
            return;
        };
        let ui = start.ctl.cfg.ui.clone();
        use fsc_core::config::Surface;
        self.as_mut().set_surface(QString::from(match ui.surface {
            Surface::Layer => "layer",
            Surface::Window => "window",
            Surface::Headless => "headless",
        }));
        self.as_mut().set_font_family(QString::from(&ui.font));
        self.as_mut().set_popup_width(ui.width);
        self.as_mut().set_max_height(ui.max_height);
        let (top, bottom, left, right) = ui.anchor.edges();
        // LayerShellQt's anchor bits.
        let anchors = top as i32 | (bottom as i32) << 1 | (left as i32) << 2 | (right as i32) << 3;
        self.as_mut().set_anchors(anchors);
        self.as_mut().set_monitor(QString::from(&ui.monitor));
        self.as_mut().set_viz_height(draw::viz::HEIGHT);
        self.as_mut().set_status_height(controller::status_height() as i32);
        self.as_mut().set_shot(QString::from(&std::env::var("FSC_SHOT").unwrap_or_default()));
        self.as_mut().set_scale(1.0);
        if cfg!(target_os = "macos") && !qobject::patch_tray_clicks() {
            eprintln!("fsc: couldn't guard Qt's menu-bar icon against macOS 27's clicks; clicking it may crash");
        }
        qobject::set_icon_name(&QString::from(crate::APP_ID));

        let meta = QString::from(start.ctl.meta());
        self.as_mut().set_meta(meta);
        self.as_mut().rust_mut().ctl = Some(start.ctl);
        self.as_mut().push_palette();

        // Everything else arrives as events, handled here on the Qt thread.
        let thread = self.qt_thread();
        let rx = start.events;
        tokio::spawn(async move {
            while let Ok(ev) = rx.recv().await {
                if thread.queue(move |b| b.handle_event(ev)).is_err() {
                    break;
                }
            }
        });
    }
}

impl qobject::Backend {
    fn handle_event(mut self: Pin<&mut Self>, ev: AppEvent) {
        match ev {
            AppEvent::Tray(u) => self.tray_update(u),
            AppEvent::TrayMics(mics) => {
                if self.as_mut().rust_mut().tray.mics.update(mics) {
                    self.tray_refresh(false);
                }
            }
            ev => self.as_mut().with_ctl(|c| c.handle(ev)),
        }
    }

    fn tray_update(mut self: Pin<&mut Self>, u: crate::tray_menu::TrayUpdate) {
        use crate::tray_menu::TrayUpdate;
        let icon = matches!(u, TrayUpdate::Icon(_));
        match u {
            TrayUpdate::Icon(st) => self.as_mut().rust_mut().tray.icon = Some(st),
            TrayUpdate::Menu(m) => self.as_mut().rust_mut().tray.menu = Some(m),
        }
        self.tray_refresh(icon);
    }

    /// Show the Qt tray's state: the menu (only when it changed, so an
    /// open menu isn't redone for nothing), the tooltip, and the icon too
    /// when `icon`.
    fn tray_refresh(mut self: Pin<&mut Self>, icon: bool) {
        if icon && let Some(st) = self.as_ref().rust().tray.icon {
            // The macOS menu bar tints a one-colour (template) image to suit
            // itself; the police box keeps its own colours.
            let mask = cfg!(target_os = "macos") && st.shape.one_colour();
            if *self.tray_mask() != mask {
                self.as_mut().set_tray_mask(mask);
            }
            if let Some(url) = crate::tray_qt::icon_url(&st) {
                self.as_mut().set_tray_icon(QString::from(&url));
            }
        }
        let tip = QString::from(&self.as_ref().rust().tray.tooltip());
        if *self.tray_tip() != tip {
            self.as_mut().set_tray_tip(tip);
        }
        if let Some(json) = self.as_mut().rust_mut().tray.menu_json() {
            let json = QString::from(&json);
            if *self.tray_menu() != json {
                self.as_mut().set_tray_menu(json);
            }
        }
    }

    pub fn tray_opening(self: Pin<&mut Self>) {
        let thread = self.qt_thread();
        tokio::spawn(async move {
            let mics = crate::tray_qt::list_mics().await;
            let _ = thread.queue(move |b| b.handle_event(AppEvent::TrayMics(mics)));
        });
    }

    pub fn tray_activate(self: Pin<&mut Self>, act: i32) {
        if let Some(cmd) = self.as_ref().rust().tray.command(act) {
            self.with_ctl(|c| {
                c.command(cmd);
            });
        }
    }

    /// Run `f` on the controller, then show what it changed. The controller
    /// is back in place before any signal reaches QML, which may call in again.
    fn with_ctl<R: Default>(mut self: Pin<&mut Self>, f: impl FnOnce(&mut Controller) -> R) -> R {
        let Some(mut ctl) = self.as_mut().rust_mut().ctl.take() else { return R::default() };
        let r = f(&mut ctl);
        let ops = ctl.history.take_ops();
        let effects = ctl.take_effects();
        let busy = ctl.in_turn();
        let can_speak = ctl.can_speak();
        self.as_mut().rust_mut().ctl = Some(ctl);
        if *self.busy() != busy {
            self.as_mut().set_busy(busy);
        }
        if *self.can_speak() != can_speak {
            self.as_mut().set_can_speak(can_speak);
        }
        self.apply(ops, effects);
        r
    }

    fn apply(mut self: Pin<&mut Self>, ops: Vec<Op>, effects: Vec<Effect>) {
        for op in ops {
            match op {
                Op::Insert(i, b) => {
                    let json = QString::from(&serde_json::to_string(&b).unwrap_or_default());
                    self.as_mut().history_insert(i as i32, &json);
                }
                Op::Update(i, b) => {
                    let json = QString::from(&serde_json::to_string(&b).unwrap_or_default());
                    self.as_mut().history_update(i as i32, &json);
                }
                Op::Remove(i, n) => self.as_mut().history_remove(i as i32, n as i32),
                Op::Clear => self.as_mut().history_clear(),
            }
        }
        for e in effects {
            match e {
                Effect::Show { focus } => {
                    self.as_mut().set_shown(true);
                    self.as_mut().show_popup(focus);
                }
                Effect::Hide => {
                    self.as_mut().set_shown(false);
                    self.as_mut().hide_popup();
                }
                Effect::Quit => qobject::quit_app(),
                Effect::Meta(text) => self.as_mut().set_meta(QString::from(&text)),
                Effect::Palette => self.as_mut().push_palette(),
                Effect::HistoryShown(on) => self.as_mut().set_history_shown(on),
                Effect::Scroll { force } => self.as_mut().scroll_to_end(force),
                Effect::ClearPick => self.as_mut().clear_pick(),
                Effect::Copy(text) => qobject::set_clipboard(&QString::from(&text)),
            }
        }
    }

    fn push_palette(mut self: Pin<&mut Self>) {
        let Some(p) = self.as_ref().rust().ctl.as_ref().map(Controller::palette) else { return };
        self.as_mut().set_fg(qcolor(p.fg));
        self.as_mut().set_bg(qcolor(p.bg));
        self.as_mut().set_accent(qcolor(p.accent));
        self.as_mut().set_accent_text(qcolor(p.on_accent()));
        self.as_mut().set_error(qcolor(p.error));
        // The panel is drawn when QML lays it out (`paintPanel`), which a
        // colour change doesn't cause.
        self.as_mut().repaint(&QString::from("panel"));
    }

    pub fn submit(self: Pin<&mut Self>, text: &QString) -> bool {
        let text = text.to_string();
        self.with_ctl(|c| c.submit(&text))
    }

    pub fn answer(self: Pin<&mut Self>, ask_id: &QString, label: &QString) {
        let (id, label) = (ask_id.to_string(), label.to_string());
        self.with_ctl(|c| c.answer_button(&id, &label));
    }

    pub fn close(self: Pin<&mut Self>) {
        self.with_ctl(|c| {
            if c.is_open() {
                c.hide()
            }
        });
    }

    pub fn stop_all(self: Pin<&mut Self>) {
        self.with_ctl(Controller::stop_all);
    }

    pub fn new_conversation(self: Pin<&mut Self>) {
        self.with_ctl(Controller::new_conversation);
    }

    pub fn copy_history(self: Pin<&mut Self>) {
        self.with_ctl(Controller::copy_history);
    }

    pub fn copy_text(self: Pin<&mut Self>, text: &QString) {
        qobject::set_clipboard(text);
        self.with_ctl(|c| c.flash_meta("COPIED"));
    }

    pub fn read_aloud(self: Pin<&mut Self>, key: f64) {
        if key.is_finite() && key >= 1.0 {
            self.with_ctl(|c| c.read_aloud(key as u64));
        }
    }

    pub fn set_entry_empty(self: Pin<&mut Self>, empty: bool) {
        self.with_ctl(|c| c.set_entry_empty(empty));
    }

    pub fn frame(mut self: Pin<&mut Self>, dt: f64, viz_w: f64, viz_h: f64, status_w: f64, status_h: f64) {
        {
            let mut rust = self.as_mut().rust_mut();
            let scale = rust.scale;
            let Some(ctl) = rust.ctl.as_mut() else { return };
            ctl.frame(dt, viz_w);
            if let Some(f) = ctl.draw_viz(viz_w, viz_h, scale) {
                draw::put(Part::Viz, f);
            }
            if let Some(f) = ctl.draw_status(status_w, status_h, scale) {
                draw::put(Part::Status, f);
            }
        }
        self.as_mut().repaint(&QString::from("viz"));
        self.as_mut().repaint(&QString::from("status"));
    }

    pub fn paint_panel(mut self: Pin<&mut Self>, w: f64, h: f64, header_bottom: f64, viz_top: f64, viz_bottom: f64, text_top: f64) {
        let regions = panel::Regions { header_bottom, graphic_top: viz_top, graphic_bottom: viz_bottom, text_top };
        let frame = {
            let this = self.as_ref();
            let Some(ctl) = this.rust().ctl.as_ref() else { return };
            ctl.draw_panel(w, h, regions, this.rust().scale)
        };
        if let Some(f) = frame {
            draw::put(Part::Panel, f);
            self.as_mut().repaint(&QString::from("panel"));
        }
    }

    pub fn set_system_colors(self: Pin<&mut Self>, bg: &QColor, fg: &QColor, accent: &QColor) {
        let sys = SystemColors { bg: rgb(bg), fg: rgb(fg), accent: rgb(accent) };
        self.with_ctl(|c| c.set_system_colors(sys));
    }

    /// # Safety
    /// `window` is the popup's live Window, from QML (or null).
    pub unsafe fn float_window_from_qml(self: Pin<&mut Self>, window: *mut qobject::QObject, activate: bool) {
        // SAFETY: QML passes its own window object, alive for this call.
        unsafe { qobject::float_window(window, activate) };
    }

    /// # Safety
    /// `layer_window` is the attached `LayerShell.Window` object, from QML.
    pub unsafe fn layer_margins(self: Pin<&mut Self>, layer_window: *mut qobject::QObject) {
        let this = self.as_ref();
        let Some(ctl) = this.rust().ctl.as_ref() else { return };
        let ui = &ctl.cfg.ui;
        let (top, bottom, left, right) = ui.anchor.edges();
        let m = |on: bool, v: i32| if on { v } else { 0 };
        // SAFETY: QML passes the live attached object (or null, which is checked).
        unsafe {
            qobject::set_margins(layer_window, m(left, ui.margin_x), m(top, ui.margin), m(right, ui.margin_x), m(bottom, ui.margin));
        }
    }
}

#[derive(Default)]
pub struct RasterRust {
    part: QString,
}

impl cxx_qt::Initialize for qobject::Raster {
    fn initialize(self: Pin<&mut Self>) {}
}

impl qobject::Raster {
    /// # Safety
    /// Qt passes a painter that's active for this call.
    unsafe fn paint(self: Pin<&mut Self>, painter: *mut qobject::QPainter) {
        let Some(part) = Part::from_name(&self.as_ref().rust().part.to_string()) else { return };
        // SAFETY: Qt's painter is valid for the duration of paint().
        let Some(painter) = (unsafe { painter.as_mut() }) else { return };
        // SAFETY: the painter isn't moved while pinned here.
        let mut painter = unsafe { Pin::new_unchecked(painter) };
        draw::with(part, |f| {
            // SAFETY: the bytes are premultiplied RGBA, `w` x `h`, as stated.
            let img = unsafe { QImage::from_raw_bytes(f.data.clone(), f.w as i32, f.h as i32, QImageFormat::Format_RGBA8888_Premultiplied) };
            let (lw, lh) = f.logical;
            painter.as_mut().draw_image(&QRect::new(0, 0, lw.round() as i32, lh.round() as i32), &img);
        });
    }
}
