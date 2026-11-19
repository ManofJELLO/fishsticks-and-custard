//! fscd: the Fishsticks & Custard assistant. Tray icon, popup UI, control
//! socket and the Claude session, all in one process.
//!
//! Threads: GTK runs on the main thread; tokio (socket, portal, tray, Claude)
//! runs on a small worker pool. Everything flows to the UI as `AppEvent`s
//! over one async channel.

mod icon;
mod log;
mod platform;
mod server;
mod style;
mod theme;
mod tray;
mod ui;

use anyhow::Result;
use gtk::prelude::*;
use gtk::glib;
use std::cell::RefCell;
use std::rc::Rc;
use tokio::sync::{mpsc, oneshot};

use fsc_core::agent::{AgentEvent, AgentHandle};
use fsc_core::config::{Config, ThemePref};
use fsc_core::ipc::{Command, Reply};

use crate::style::{Palette, Rgb, ThemeMode};

pub enum AppEvent {
    Command(Command, Option<oneshot::Sender<Reply>>),
    Agent(AgentEvent),
    Audio(fsc_audio::AudioEvent),
    Theme(ThemeMode),
    /// The desktop's accent colour changed (`None`: it has none).
    Accent(Option<Rgb>),
    /// A voice download moved on: `Ok(Some(percent))` while it runs,
    /// `Ok(None)` when it's done, `Err` if it failed.
    VoiceFetch { voice: String, progress: Result<Option<u32>, String> },
}

const APP_ID: &str = "dev.fsc.FishsticksAndCustard";

fn main() -> std::process::ExitCode {
    let result = run();
    // Said here, not by the runtime after main, so it reaches the log.
    if let Err(e) = &result {
        eprintln!("fscd: {e:#}");
    }
    log::drain();
    if result.is_ok() { std::process::ExitCode::SUCCESS } else { std::process::ExitCode::FAILURE }
}

fn run() -> Result<()> {
    // Before any threads exist (these set environment variables).
    fsc_audio::prepare_env();
    // Wayland only: the popup is a layer-shell surface, which has no X11
    // equivalent, and the session's GDK_BACKEND may list x11 as a fallback.
    // SAFETY: no other threads exist yet.
    unsafe { std::env::set_var("GDK_BACKEND", "wayland") };
    gtk::gdk::set_allowed_backends("wayland");

    // Headless diagnostics: no display needed.
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--self-test") {
        return fsc_audio::self_test(&Config::load()?.voice);
    }
    if let Some(i) = args.iter().position(|a| a == "--dump-icons") {
        let dir = args.get(i + 1).map(String::as_str).unwrap_or(".");
        return dump_icons(dir);
    }

    // Already running? Launching again (e.g. from a launcher) toggles the popup.
    if fsc_core::ipc::send(&Command::Status).is_ok() {
        fsc_core::ipc::send(&Command::Toggle)?;
        println!("fscd is already running; toggled its popup");
        return Ok(());
    }

    if std::env::var_os("WAYLAND_DISPLAY").is_none() {
        anyhow::bail!("fscd needs a Wayland session (WAYLAND_DISPLAY is not set)");
    }
    // Only the instance that stays: a second launch mustn't rotate its log.
    log::start();

    let cfg = Config::load()?;
    // A hex colour in the config pins the accent; otherwise follow the desktop.
    let accent = Rgb::from_hex(&cfg.ui.accent);

    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build()?;
    // Keep the runtime entered on the GTK thread so UI code can tokio::spawn.
    let _guard = rt.enter();

    let (ev_tx, ev_rx) = async_channel::unbounded::<AppEvent>();

    let listener = rt.block_on(server::bind())?;
    rt.spawn(server::serve(listener, ev_tx.clone()));

    let initial_theme = match cfg.ui.theme {
        ThemePref::Light => ThemeMode::Light,
        ThemePref::Dark | ThemePref::Auto => ThemeMode::Dark,
    };
    if cfg.ui.theme == ThemePref::Auto {
        rt.spawn(theme::watch(ev_tx.clone()));
    }
    if accent.is_none() {
        rt.spawn(theme::watch_accent(ev_tx.clone()));
    }

    let (tray_tx, tray_rx) = mpsc::unbounded_channel();
    rt.spawn(tray::run(
        icon::IconState {
            phase: fsc_core::Phase::Idle,
            theme: initial_theme,
            muted: false,
            // The recording dot is always red, whatever the theme.
            accent: style::NOTHING_RED,
            shape: cfg.ui.tray_icon,
        },
        tray::MenuState::new(&cfg),
        tray_rx,
        ev_tx.clone(),
    ));

    let claude = {
        let tx = ev_tx.clone();
        AgentHandle::spawn(&cfg, cfg.home(), move |e| {
            let _ = tx.try_send(AppEvent::Agent(e));
        })
    };

    let audio = {
        let tx = ev_tx.clone();
        fsc_audio::spawn(cfg.voice.clone(), move |e| {
            let _ = tx.try_send(AppEvent::Audio(e));
        })
    };

    let app = gtk::Application::builder().application_id(APP_ID).build();
    let ui_cell: Rc<RefCell<Option<Rc<ui::Ui>>>> = Rc::default();
    let ev_rx = RefCell::new(Some(ev_rx));

    {
        let ui_cell = ui_cell.clone();
        let ev_tx = ev_tx.clone();
        app.connect_activate(move |app| {
            // Activated once at startup; a second fscd toggles us over the
            // control socket instead (see above).
            if ui_cell.borrow().is_some() {
                return;
            }
            // Keep running with no visible window.
            std::mem::forget(app.hold());
            // The window's icon: the police box from the installed icon theme.
            gtk::Window::set_default_icon_name(APP_ID);

            let ui = ui::Ui::new(
                app,
                cfg.clone(),
                Palette::new(initial_theme, accent.unwrap_or(style::NOTHING_RED)),
                accent,
                claude.clone(),
                audio.clone(),
                tray_tx.clone(),
                ev_tx.clone(),
            );
            *ui_cell.borrow_mut() = Some(ui.clone());

            if cfg.session_idle_minutes() > 0 {
                let ui = ui.clone();
                glib::timeout_add_seconds_local(60, move || {
                    ui.tick_idle();
                    glib::ControlFlow::Continue
                });
            }

            let rx = ev_rx.borrow_mut().take().expect("activate runs once");
            glib::spawn_future_local(async move {
                while let Ok(ev) = rx.recv().await {
                    ui.handle(ev);
                }
            });
        });
    }

    // SIGINT/SIGTERM: clean quit (saves memory, removes the socket). A second
    // signal while that's under way exits immediately.
    {
        let tx = ev_tx.clone();
        rt.spawn(async move {
            if !platform::quit_signal().await {
                return;
            }
            let _ = tx.send(AppEvent::Command(Command::Quit, None)).await;
            platform::quit_signal().await;
            eprintln!("fsc: second signal, exiting now");
            server::cleanup();
            log::drain();
            std::process::exit(130);
        });
    }

    app.run_with_args::<&str>(&[]);
    server::cleanup();
    Ok(())
}

/// `fscd --dump-icons DIR`: write the tray icon variants as PNGs for a look.
fn dump_icons(dir: &str) -> Result<()> {
    use fsc_core::Phase;
    use fsc_core::config::TrayIcon;
    std::fs::create_dir_all(dir)?;
    for shape in [TrayIcon::PoliceBox, TrayIcon::Fishbone] {
        for theme in [ThemeMode::Dark, ThemeMode::Light] {
            for (name, phase, muted) in [("idle", Phase::Idle, false), ("rec", Phase::Listening, false), ("muted", Phase::Idle, true)] {
                let st = icon::IconState {
                    phase,
                    theme,
                    muted,
                    accent: style::NOTHING_RED,
                    shape,
                };
                for size in [22u32, 64] {
                    let path = format!("{dir}/{shape:?}-{theme:?}-{name}-{size}.png").to_lowercase();
                    icon::render(size, &st).save_png(&path)?;
                }
            }
        }
    }
    println!("wrote icons to {dir}");
    Ok(())
}
