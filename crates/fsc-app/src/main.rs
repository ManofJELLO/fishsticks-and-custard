//! fscd: the Fishsticks & Custard assistant. Tray icon, popup UI, control
//! socket and the Claude session, all in one process.
//!
//! Threads: GTK runs on the main thread; tokio (socket, portal, tray, Claude)
//! runs on a small worker pool. Everything flows to the UI as `AppEvent`s
//! over one async channel.

mod icon;
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

use fsc_core::claude::{ClaudeEvent, ClaudeHandle};
use fsc_core::config::{Config, ThemePref};
use fsc_core::ipc::{Command, Reply};

use crate::style::{Palette, Rgb, ThemeMode};

pub enum AppEvent {
    Command(Command, Option<oneshot::Sender<Reply>>),
    Claude(ClaudeEvent),
    Audio(fsc_audio::AudioEvent),
    Theme(ThemeMode),
}

const APP_ID: &str = "dev.fsc.FishsticksAndCustard";

fn main() -> Result<()> {
    // Before any threads exist (these set environment variables).
    fsc_audio::prepare_env();
    // Wayland only: the popup is a layer-shell surface, which has no X11
    // equivalent, and the session's GDK_BACKEND may list x11 as a fallback.
    // SAFETY: no other threads exist yet.
    unsafe { std::env::set_var("GDK_BACKEND", "wayland") };
    gtk::gdk::set_allowed_backends("wayland");
    if std::env::var_os("WAYLAND_DISPLAY").is_none() {
        anyhow::bail!("fscd needs a Wayland session (WAYLAND_DISPLAY is not set)");
    }

    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--self-test") {
        return fsc_audio::self_test(&Config::load()?.voice);
    }
    if let Some(i) = args.iter().position(|a| a == "--dump-icons") {
        let dir = args.get(i + 1).map(String::as_str).unwrap_or(".");
        return dump_icons(dir);
    }

    let cfg = Config::load()?;
    let accent = Rgb::from_hex(&cfg.ui.accent).unwrap_or(style::NOTHING_RED);

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

    let (tray_tx, tray_rx) = mpsc::unbounded_channel();
    rt.spawn(tray::run(
        icon::IconState {
            phase: fsc_core::Phase::Idle,
            theme: initial_theme,
            muted: false,
            accent,
            glow: cfg.ui.glow,
        },
        tray_rx,
        ev_tx.clone(),
    ));

    let claude = {
        let tx = ev_tx.clone();
        ClaudeHandle::spawn(cfg.claude.clone(), cfg.claude_home(), move |e| {
            let _ = tx.try_send(AppEvent::Claude(e));
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
        app.connect_activate(move |app| {
            if let Some(ui) = ui_cell.borrow().as_ref() {
                // A second launch activates us: treat it as "toggle".
                ui.command(Command::Toggle);
                return;
            }
            // Keep running with no visible window.
            std::mem::forget(app.hold());

            let ui = ui::Ui::new(
                app,
                cfg.clone(),
                Palette::new(initial_theme, accent),
                claude.clone(),
                audio.clone(),
                tray_tx.clone(),
            );
            *ui_cell.borrow_mut() = Some(ui.clone());

            {
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

    // SIGINT/SIGTERM -> clean quit (removes the socket).
    {
        let tx = ev_tx.clone();
        rt.spawn(async move {
            use tokio::signal::unix::{SignalKind, signal};
            let (Ok(mut int), Ok(mut term)) = (signal(SignalKind::interrupt()), signal(SignalKind::terminate())) else {
                return;
            };
            tokio::select! { _ = int.recv() => {}, _ = term.recv() => {} }
            let _ = tx.send(AppEvent::Command(Command::Quit, None)).await;
        });
    }

    app.run_with_args::<&str>(&[]);
    server::cleanup();
    Ok(())
}

/// `fscd --dump-icons DIR`: write the tray icon variants as PNGs for a look.
fn dump_icons(dir: &str) -> Result<()> {
    use fsc_core::Phase;
    use fsc_core::config::GlowStyle;
    std::fs::create_dir_all(dir)?;
    for theme in [ThemeMode::Dark, ThemeMode::Light] {
        for (name, phase, muted) in [("idle", Phase::Idle, false), ("rec", Phase::Listening, false), ("muted", Phase::Idle, true)] {
            let st = icon::IconState { phase, theme, muted, accent: style::NOTHING_RED, glow: GlowStyle::Contrast };
            for size in [22u32, 64] {
                let path = format!("{dir}/{theme:?}-{name}-{size}.png").to_lowercase();
                icon::render(size, &st).save_png(&path)?;
            }
        }
    }
    println!("wrote icons to {dir}");
    Ok(())
}
