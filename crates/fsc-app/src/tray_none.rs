//! No tray yet outside Linux (its tray is a StatusNotifierItem over D-Bus).
//! TODO(cross-platform): Qt's QSystemTrayIcon, for the Windows notification
//! area and the macOS menu bar.

use tokio::sync::mpsc;

use fsc_core::agent::Choices;
use fsc_core::config::{AgentKind, Config, SpeakMode};

use crate::AppEvent;
use crate::icon::IconState;

#[derive(Debug, Clone, PartialEq)]
pub struct MenuState {
    pub agent: AgentKind,
    pub agents: [bool; AgentKind::ALL.len()],
    pub choices: Choices,
    pub mic: String,
    pub mic_missing: bool,
    pub speak: SpeakMode,
    pub language: &'static str,
    pub voice: String,
    pub fetching: Option<(String, u32)>,
}

impl MenuState {
    pub fn new(cfg: &Config) -> Self {
        Self {
            agent: cfg.agent.backend,
            agents: AgentKind::ALL.map(|k| cfg.available(k)),
            choices: Choices::default(),
            mic: cfg.voice.input_device.clone(),
            mic_missing: false,
            speak: cfg.voice.speak,
            language: cfg.voice.spoken().code,
            voice: cfg.voice.voice(),
            fetching: None,
        }
    }
}

pub enum TrayUpdate {
    Icon(IconState),
    Menu(MenuState),
}

/// Drain the updates, so the controller's sends never pile up.
pub async fn run(_: IconState, _: MenuState, mut rx: mpsc::UnboundedReceiver<TrayUpdate>, _: async_channel::Sender<AppEvent>) {
    while rx.recv().await.is_some() {}
}
