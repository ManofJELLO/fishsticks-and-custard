//! StatusNotifierItem tray icon (works with Waybar's `tray` module).

use ksni::menu::{CheckmarkItem, RadioGroup, RadioItem, StandardItem, SubMenu};
use ksni::{MenuItem, ToolTip, TrayMethods};
use tokio::sync::mpsc;

use fsc_core::agent::{Choices, Select};
use fsc_core::config::{AgentKind, Config, SpeakMode};
use fsc_core::ipc::Command;

use crate::AppEvent;
use crate::icon::{self, IconState};

/// What the menu shows beyond the basics: the agents, and the models and
/// modes the current one offers.
#[derive(Debug, Clone, PartialEq)]
pub struct MenuState {
    pub agent: AgentKind,
    /// Which agents are installed, in `AgentKind::ALL` order.
    pub agents: [bool; AgentKind::ALL.len()],
    pub choices: Choices,
    /// `voice.input_device`: "" is the system's default mic.
    pub mic: String,
    pub speak: SpeakMode,
}

impl MenuState {
    pub fn new(cfg: &Config) -> Self {
        Self {
            agent: cfg.agent.backend,
            agents: AgentKind::ALL.map(|k| cfg.available(k)),
            choices: Choices::default(),
            mic: cfg.voice.input_device.clone(),
            speak: cfg.voice.speak,
        }
    }
}

pub enum TrayUpdate {
    Icon(IconState),
    Menu(MenuState),
}

struct FscTray {
    st: IconState,
    menu: MenuState,
    /// The microphones PipeWire has, looked up again whenever the menu opens.
    mics: Vec<String>,
    refresh: mpsc::UnboundedSender<()>,
    tx: async_channel::Sender<AppEvent>,
}

impl FscTray {
    fn send(&self, cmd: Command) {
        let _ = self.tx.try_send(AppEvent::Command(cmd, None));
    }
}

impl FscTray {
    /// Agent ▸ Claude / Codex / Antigravity (/ Custom): the ones not
    /// installed are greyed out, a custom one only shows when it's set up.
    fn agent_menu(&self) -> MenuItem<Self> {
        let m = &self.menu;
        let shown: Vec<AgentKind> = AgentKind::ALL
            .into_iter()
            .zip(m.agents)
            .filter(|&(k, ok)| k != AgentKind::Acp || ok || k == m.agent)
            .map(|(k, _)| k)
            .collect();
        let selected = shown.iter().position(|&k| k == m.agent).unwrap_or(0);
        let options = shown
            .iter()
            .map(|&k| {
                let ok = m.agents[AgentKind::ALL.iter().position(|&a| a == k).unwrap_or(0)];
                RadioItem {
                    label: if ok { k.name().into() } else { format!("{} (not installed)", k.name()) },
                    enabled: ok || k == m.agent,
                    ..Default::default()
                }
            })
            .collect();
        SubMenu {
            label: format!("Agent: {}", m.agent.name()),
            icon_name: "system-users-symbolic".into(),
            submenu: vec![
                RadioGroup {
                    selected,
                    select: Box::new(move |t: &mut Self, i: usize| {
                        if let Some(&k) = shown.get(i) {
                            t.send(Command::Agent { backend: Some(k.key().into()) });
                        }
                    }),
                    options,
                }
                .into(),
            ],
            ..Default::default()
        }
        .into()
    }

    /// Model ▸ or Mode ▸: the current agent's options. Always there, so the
    /// menu keeps its length and order; greyed out until the agent has said
    /// what it offers (while it starts, or if it offers none).
    fn choice_menu(&self, title: &str, sel: Option<&Select>, cmd: fn(String) -> Command) -> MenuItem<Self> {
        let Some(sel) = sel.filter(|s| !s.options.is_empty()) else {
            return SubMenu {
                label: format!("{title}: not loaded"),
                enabled: false,
                submenu: vec![StandardItem { label: "Not loaded yet".into(), enabled: false, ..Default::default() }.into()],
                ..Default::default()
            }
            .into();
        };
        let ids: Vec<String> = sel.options.iter().map(|o| o.id.clone()).collect();
        let selected = ids.iter().position(|id| *id == sel.current).unwrap_or(usize::MAX);
        let options = sel.options.iter().map(|o| RadioItem { label: menu_label(&o.name), ..Default::default() }).collect();
        SubMenu {
            label: format!("{title}: {}", menu_label(sel.current_name())),
            submenu: vec![
                RadioGroup {
                    selected,
                    select: Box::new(move |t: &mut Self, i: usize| {
                        if let Some(id) = ids.get(i) {
                            t.send(cmd(id.clone()));
                        }
                    }),
                    options,
                }
                .into(),
            ],
            ..Default::default()
        }
        .into()
    }
}

impl FscTray {
    /// Microphone ▸ System default / each mic PipeWire has. A saved mic
    /// that isn't plugged in stays listed (and chosen) so you can see why.
    fn mic_menu(&self) -> MenuItem<Self> {
        let current = self.menu.mic.trim();
        let default = current.is_empty() || current.eq_ignore_ascii_case("default");
        // What to save for each entry; "" is the default.
        let mut values: Vec<String> = vec![String::new()];
        values.extend(self.mics.iter().cloned());
        let lower = current.to_lowercase();
        let selected = if default {
            Some(0)
        } else {
            // Saved by the menu: the exact name. Typed in the file: part of it.
            values.iter().skip(1).position(|m| m.to_lowercase() == lower)
                .or_else(|| values.iter().skip(1).position(|m| m.to_lowercase().contains(&lower)))
                .map(|i| i + 1)
        };
        let mut labels: Vec<String> = values.iter().map(|v| if v.is_empty() { "System default".into() } else { menu_label(v) }).collect();
        let selected = selected.unwrap_or_else(|| {
            values.push(current.to_string());
            labels.push(format!("{} (not connected)", menu_label(current)));
            values.len() - 1
        });
        let title = if default { "System default".to_string() } else { labels[selected].clone() };
        let options = labels.into_iter().map(|label| RadioItem { label, ..Default::default() }).collect();
        SubMenu {
            label: format!("Microphone: {title}"),
            icon_name: "audio-input-microphone-symbolic".into(),
            submenu: vec![
                RadioGroup {
                    selected,
                    select: Box::new(move |t: &mut Self, i: usize| {
                        if let Some(v) = values.get(i) {
                            t.send(Command::Mic { name: Some(v.clone()) });
                        }
                    }),
                    options,
                }
                .into(),
            ],
            ..Default::default()
        }
        .into()
    }

    /// Speak replies ▸ When I talk / Always / Never.
    fn speak_menu(&self) -> MenuItem<Self> {
        let selected = SpeakMode::ALL.iter().position(|&m| m == self.menu.speak).unwrap_or(0);
        SubMenu {
            label: format!("Speak replies: {}", self.menu.speak.name()),
            icon_name: "audio-speakers-symbolic".into(),
            submenu: vec![
                RadioGroup {
                    selected,
                    select: Box::new(|t: &mut Self, i: usize| {
                        if let Some(m) = SpeakMode::ALL.get(i) {
                            t.send(Command::Speak { mode: Some(m.key().into()) });
                        }
                    }),
                    options: SpeakMode::ALL.iter().map(|m| RadioItem { label: m.name().into(), ..Default::default() }).collect(),
                }
                .into(),
            ],
            ..Default::default()
        }
        .into()
    }
}

/// Menu labels treat "_" as an access-key marker; double it to show one.
fn menu_label(s: &str) -> String {
    s.replace('_', "__")
}

impl ksni::Tray for FscTray {
    fn id(&self) -> String {
        "fishsticks-and-custard".into()
    }

    fn title(&self) -> String {
        "Fishsticks & Custard".into()
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        icon::sni_icons(&self.st)
    }

    fn tool_tip(&self) -> ToolTip {
        let mut desc = self.st.phase.label().to_lowercase();
        if self.st.muted {
            desc.push_str(" · wake word muted");
        }
        ToolTip { title: "Fishsticks & Custard".into(), description: desc, ..Default::default() }
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        self.send(Command::Toggle);
    }

    fn menu_about_to_show(&mut self) {
        let _ = self.refresh.send(());
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        vec![
            StandardItem {
                label: "Type…".into(),
                icon_name: "input-keyboard-symbolic".into(),
                activate: Box::new(|t: &mut Self| t.send(Command::Type)),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: "Talk".into(),
                icon_name: "audio-input-microphone-symbolic".into(),
                activate: Box::new(|t: &mut Self| t.send(Command::Listen)),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            CheckmarkItem {
                label: "Mute wake word".into(),
                checked: self.st.muted,
                activate: Box::new(|t: &mut Self| t.send(Command::Mute { on: None })),
                ..Default::default()
            }
            .into(),
            self.mic_menu(),
            self.speak_menu(),
            MenuItem::Separator,
            self.agent_menu(),
            self.choice_menu("Model", self.menu.choices.model.as_ref(), |v| Command::Model { value: Some(v) }),
            self.choice_menu("Mode", self.menu.choices.mode.as_ref(), |v| Command::Mode { value: Some(v) }),
            StandardItem {
                label: "New conversation".into(),
                activate: Box::new(|t: &mut Self| t.send(Command::New)),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: "Forget memories".into(),
                icon_name: "edit-clear-history-symbolic".into(),
                activate: Box::new(|t: &mut Self| t.send(Command::Forget)),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: "Settings…".into(),
                icon_name: "preferences-system-symbolic".into(),
                activate: Box::new(|t: &mut Self| t.send(Command::Settings)),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: "Quit".into(),
                activate: Box::new(|t: &mut Self| t.send(Command::Quit)),
                ..Default::default()
            }
            .into(),
        ]
    }
}

/// Owns the tray and applies state updates sent from the UI thread.
pub async fn run(
    initial: IconState,
    menu: MenuState,
    mut updates: mpsc::UnboundedReceiver<TrayUpdate>,
    tx: async_channel::Sender<AppEvent>,
) {
    // At login fscd can start before the bar's StatusNotifierWatcher is on
    // the bus. Without this, ksni treats that as fatal and the icon never
    // appears; with it, ksni waits and registers once the watcher shows up.
    let (refresh, mut refreshes) = mpsc::unbounded_channel();
    let mut mics = list_mics().await;
    let handle = match (FscTray { st: initial, menu, mics: mics.clone(), refresh, tx }).assume_sni_available(true).spawn().await {
        Ok(h) => h,
        Err(e) => {
            eprintln!("fsc: tray icon unavailable: {e}");
            return;
        }
    };
    loop {
        tokio::select! {
            u = updates.recv() => match u {
                Some(TrayUpdate::Icon(st)) => { handle.update(|t| t.st = st).await; }
                Some(TrayUpdate::Menu(m)) => { handle.update(|t| t.menu = m).await; }
                None => break,
            },
            Some(()) = refreshes.recv() => {
                while refreshes.try_recv().is_ok() {}
                let now = list_mics().await;
                // Only when something changed: an update redraws the open menu.
                if now != mics {
                    mics = now;
                    let m = mics.clone();
                    handle.update(|t| t.mics = m).await;
                }
            }
        }
    }
}

/// `fsc_audio::microphones`, off the async threads (it runs `pw-dump`).
async fn list_mics() -> Vec<String> {
    tokio::task::spawn_blocking(fsc_audio::microphones).await.unwrap_or_default()
}
