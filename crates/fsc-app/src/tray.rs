//! StatusNotifierItem tray icon (works with Waybar's `tray` module).

use ksni::menu::{CheckmarkItem, RadioGroup, RadioItem, StandardItem, SubMenu};
use ksni::{MenuItem, ToolTip, TrayMethods};
use tokio::sync::mpsc;

use fsc_core::config::AgentKind;
use fsc_core::ipc::Command;

use crate::AppEvent;
use crate::icon::{self, IconState};

struct FscTray {
    st: IconState,
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
        let shown: Vec<AgentKind> = AgentKind::ALL
            .into_iter()
            .zip(self.st.agents)
            .filter(|&(k, ok)| k != AgentKind::Acp || ok || k == self.st.agent)
            .map(|(k, _)| k)
            .collect();
        let selected = shown.iter().position(|&k| k == self.st.agent).unwrap_or(0);
        let options = shown
            .iter()
            .map(|&k| {
                let ok = self.st.agents[AgentKind::ALL.iter().position(|&a| a == k).unwrap_or(0)];
                RadioItem {
                    label: if ok { k.name().into() } else { format!("{} (not installed)", k.name()) },
                    enabled: ok || k == self.st.agent,
                    ..Default::default()
                }
            })
            .collect();
        SubMenu {
            label: format!("Agent: {}", self.st.agent.name()),
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
            self.agent_menu(),
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
                label: "Quit".into(),
                activate: Box::new(|t: &mut Self| t.send(Command::Quit)),
                ..Default::default()
            }
            .into(),
        ]
    }
}

/// Owns the tray and applies state updates sent from the UI thread.
pub async fn run(initial: IconState, mut updates: mpsc::UnboundedReceiver<IconState>, tx: async_channel::Sender<AppEvent>) {
    // At login fscd can start before the bar's StatusNotifierWatcher is on
    // the bus. Without this, ksni treats that as fatal and the icon never
    // appears; with it, ksni waits and registers once the watcher shows up.
    let handle = match (FscTray { st: initial, tx }).assume_sni_available(true).spawn().await {
        Ok(h) => h,
        Err(e) => {
            eprintln!("fsc: tray icon unavailable: {e}");
            return;
        }
    };
    while let Some(st) = updates.recv().await {
        handle.update(|t| t.st = st).await;
    }
}
