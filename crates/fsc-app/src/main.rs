//! fscd: the Fishsticks & Custard assistant. Tray icon, popup UI, control
//! socket and the agent session, all in one process.
//!
//! Threads: Qt runs on the main thread; tokio (socket, portal, tray, agent)
//! runs on a small worker pool. Everything flows to the UI as `AppEvent`s
//! over one async channel, which `qt::bridge` hands to the `controller` on
//! the Qt thread.

mod controller;
mod draw;
mod history;
#[cfg(not(target_os = "linux"))]
mod hotkeys;
mod icon;
mod log;
mod platform;
mod qt;
mod server;
mod speech_text;
mod style;
#[cfg(target_os = "linux")]
mod theme;
#[cfg(target_os = "linux")]
mod tray;
mod tray_menu;
mod tray_qt;

use anyhow::Result;
use cxx_qt_lib::{QGuiApplication, QQmlApplicationEngine, QString, QUrl};
use tokio::sync::{mpsc, oneshot};

use fsc_core::agent::{AgentEvent, AgentHandle};
use fsc_core::config::{Config, Surface, ThemePref};
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
    /// A timer the controller set is due.
    Timer(controller::Timer),
    /// A minute has passed (idle sessions are rotated).
    IdleTick,
    /// A mic just chosen by id has this name.
    MicName(String),
    /// Show streamed text now: the events before this one are handled.
    Flush,
    /// For the Qt tray (`tray_qt`), which lives on the Qt thread.
    Tray(tray_menu::TrayUpdate),
    TrayMics(Vec<fsc_audio::Microphone>),
}

pub const APP_ID: &str = "dev.fsc.FishsticksAndCustard";

fn main() -> std::process::ExitCode {
    let result = run();
    // Said here, not by the runtime after main, so it reaches the log.
    if let Err(e) = &result {
        eprintln!("fscd: {e:#}");
    }
    log::drain();
    if result.is_ok() { std::process::ExitCode::SUCCESS } else { std::process::ExitCode::FAILURE }
}

/// The tray is Qt's (`tray_qt`): everywhere but Linux, and there when
/// `FSC_QT_TRAY` is set, to try it.
pub fn qt_tray() -> bool {
    !cfg!(target_os = "linux") || std::env::var_os("FSC_QT_TRAY").is_some()
}

/// The platform Qt runs on. On Linux the popup is a Wayland surface (a
/// layer-shell one has no X11 equivalent), though a test may ask for
/// `offscreen`.
fn offscreen() -> bool {
    // Options may follow, as in "offscreen:configfile=screens.json".
    std::env::var("QT_QPA_PLATFORM").is_ok_and(|p| p.split(':').next() == Some("offscreen"))
}

fn run() -> Result<()> {
    // Before any threads exist (these set environment variables).
    fsc_audio::prepare_env();
    // For fscd's own Qt only: the programs it starts get these as they were.
    // (The QML imports QtQuick.Controls.Basic by name, so the user's
    // QT_QUICK_CONTROLS_STYLE can't restyle the popup and needn't be set.)
    // SAFETY (both below): no other threads exist yet.
    if cfg!(target_os = "linux") && !offscreen() {
        unsafe { fsc_core::own_env::set("QT_QPA_PLATFORM", "wayland") };
    }
    // Qt's own messages (QML errors among them) go to the systemd journal
    // when stderr isn't a terminal; fscd's stderr is the log, so keep them there.
    unsafe { fsc_core::own_env::set("QT_FORCE_STDERR_LOGGING", "1") };
    // Qt's Wayland text input warns each time focus leaves a text field.
    if std::env::var_os("QT_LOGGING_RULES").is_none() {
        unsafe { fsc_core::own_env::set("QT_LOGGING_RULES", "qt.qpa.wayland.textinput=false") };
    }

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

    if cfg!(target_os = "linux") && !offscreen() && std::env::var_os("WAYLAND_DISPLAY").is_none() {
        anyhow::bail!("fscd needs a Wayland session (WAYLAND_DISPLAY is not set)");
    }
    // Still before any threads: this sets PATH, for fscd and all it starts.
    #[cfg(target_os = "macos")]
    let path_note = platform::adopt_login_path();
    // Only the instance that stays: a second launch mustn't rotate its log.
    log::start();
    #[cfg(target_os = "macos")]
    eprintln!("fsc: {path_note}");

    let cfg = Config::load()?;
    #[cfg(not(target_os = "linux"))]
    let keys = cfg.keys.clone();
    // A hex colour in the config pins the accent; otherwise follow the desktop.
    let accent = Rgb::from_hex(&cfg.ui.accent);

    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build()?;
    // Keep the runtime entered on the Qt thread so UI code can tokio::spawn.
    let _guard = rt.enter();

    let (ev_tx, ev_rx) = async_channel::unbounded::<AppEvent>();

    let listener = rt.block_on(server::bind())?;
    rt.spawn(server::serve(listener, ev_tx.clone()));

    let initial_theme = match cfg.ui.theme {
        ThemePref::Light => ThemeMode::Light,
        ThemePref::Dark | ThemePref::Auto => ThemeMode::Dark,
    };
    #[cfg(target_os = "linux")]
    {
        if cfg.ui.theme == ThemePref::Auto {
            rt.spawn(theme::watch(ev_tx.clone()));
        }
        if accent.is_none() {
            rt.spawn(theme::watch_accent(ev_tx.clone()));
        }
    }

    let (tray_tx, tray_rx) = mpsc::unbounded_channel();
    let first_icon = icon::IconState {
        phase: fsc_core::Phase::Idle,
        theme: initial_theme,
        muted: false,
        // The recording dot is always red, whatever the theme.
        accent: style::NOTHING_RED,
        shape: cfg.ui.tray_icon,
    };
    let first_menu = tray_menu::MenuState::new(&cfg);
    if qt_tray() {
        rt.spawn(tray_qt::run(first_icon, first_menu, tray_rx, ev_tx.clone()));
    } else {
        #[cfg(target_os = "linux")]
        rt.spawn(tray::run(first_icon, first_menu, tray_rx, ev_tx.clone()));
    }

    let agent = {
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

    if cfg.session_idle_minutes() > 0 {
        let tx = ev_tx.clone();
        rt.spawn(async move {
            let mut every = tokio::time::interval(std::time::Duration::from_secs(60));
            every.tick().await;
            loop {
                every.tick().await;
                if tx.send(AppEvent::IdleTick).await.is_err() {
                    break;
                }
            }
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

    let surface = cfg.ui.surface;
    let ctl = controller::Controller::new(
        cfg,
        Palette::new(initial_theme, accent.unwrap_or(style::NOTHING_RED)),
        accent,
        agent,
        audio,
        tray_tx,
        ev_tx.clone(),
    );
    qt::bridge::STARTUP.with(|s| *s.borrow_mut() = Some(qt::bridge::Startup { ctl, events: ev_rx }));

    let mut app = QGuiApplication::new();
    if let Some(mut app) = app.as_mut() {
        app.as_mut().set_application_name(&QString::from("fscd"));
        app.as_mut().set_application_display_name(&QString::from("Fishsticks & Custard"));
        // The Wayland app id, which Hyprland's window rules match as the class.
        QGuiApplication::set_desktop_file_name(&QString::from(APP_ID));
    }
    // After the app exists, before its event loop runs (macOS sends the
    // keys through it). Kept until fscd exits.
    #[cfg(not(target_os = "linux"))]
    let _hotkeys = hotkeys::start(&keys, ev_tx.clone());
    let mut engine = QQmlApplicationEngine::new();
    if let Some(mut engine) = engine.as_mut() {
        engine
            .as_mut()
            .on_object_creation_failed(|_, url| {
                // Without its tray icon fscd still works (hotkeys, the wake
                // phrase, `fsc`): say so and carry on.
                if format!("{url}").ends_with("TrayIcon.qml") {
                    eprintln!("fscd: the tray icon failed to load ({url}); carrying on without it");
                    return;
                }
                eprintln!(
                    "fscd: the popup failed to load ({url}). On Arch, check that qt6-declarative \
                     (and, for surface = \"layer\", layer-shell-qt) are installed."
                );
                // Straight out, but not before the hint reaches the log.
                server::cleanup();
                log::drain();
                std::process::exit(1);
            })
            .release();
        // Layer shell is Wayland's; elsewhere the popup is a window.
        let root = match surface {
            Surface::Layer if cfg!(target_os = "linux") && !offscreen() => "LayerPopup",
            _ => "WindowPopup",
        };
        engine.as_mut().load(&QUrl::from(&format!("qrc:/qt/qml/dev/fsc/app/qml/{root}.qml")));
        if qt_tray() {
            engine.load(&QUrl::from("qrc:/qt/qml/dev/fsc/app/qml/TrayIcon.qml"));
        }
    }
    if let Some(app) = app.as_mut() {
        app.exec();
    }
    server::cleanup();
    Ok(())
}

/// `fscd --dump-icons DIR`: write the tray icon variants as PNGs for a look.
fn dump_icons(dir: &str) -> Result<()> {
    use fsc_core::Phase;
    use fsc_core::config::TrayIcon;
    std::fs::create_dir_all(dir)?;
    for shape in [TrayIcon::PoliceBox, TrayIcon::Fishbone, TrayIcon::Fez] {
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
