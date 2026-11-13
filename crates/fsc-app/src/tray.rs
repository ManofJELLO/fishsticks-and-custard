//! StatusNotifierItem tray icon (works with Waybar's `tray` module).

use ksni::menu::{CheckmarkItem, StandardItem};
use ksni::{MenuItem, ToolTip, TrayMethods};
use tokio::sync::mpsc;

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
            StandardItem {
                label: "New conversation".into(),
                activate: Box::new(|t: &mut Self| t.send(Command::New)),
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
    let handle = match (FscTray { st: initial, tx }).spawn().await {
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
