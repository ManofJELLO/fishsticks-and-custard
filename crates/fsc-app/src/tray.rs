//! StatusNotifierItem tray icon (works with Waybar's `tray` module).

use ksni::menu::{CheckmarkItem, RadioGroup, RadioItem, StandardItem, SubMenu};
use ksni::{MenuItem, ToolTip, TrayMethods};
use tokio::sync::mpsc;

use fsc_core::ipc::Command;

use crate::AppEvent;
use crate::icon::{self, IconState};
pub use crate::tray_menu::{MenuState, TrayUpdate};
use crate::tray_menu::{self, Item, Mics};

struct FscTray {
    st: IconState,
    menu: MenuState,
    /// The microphones, looked up again whenever the menu opens.
    mics: Mics,
    refresh: mpsc::UnboundedSender<()>,
    tx: async_channel::Sender<AppEvent>,
}

impl FscTray {
    fn send(&self, cmd: Command) {
        let _ = self.tx.try_send(AppEvent::Command(cmd, None));
    }
}

/// Menu labels treat "_" as an access-key marker; double it to show one.
fn menu_label(s: &str) -> String {
    s.replace('_', "__")
}

/// One item of the shared menu, as ksni shows it.
fn to_ksni(item: Item) -> MenuItem<FscTray> {
    match item {
        Item::Separator => MenuItem::Separator,
        Item::Action { label, icon, cmd } => StandardItem {
            label: menu_label(&label),
            icon_name: icon.into(),
            activate: Box::new(move |t: &mut FscTray| t.send(cmd.clone())),
            ..Default::default()
        }
        .into(),
        Item::Check { label, checked, cmd } => CheckmarkItem {
            label: menu_label(&label),
            checked,
            activate: Box::new(move |t: &mut FscTray| t.send(cmd.clone())),
            ..Default::default()
        }
        .into(),
        // Only notes ("Not loaded yet"): plain greyed lines, no radio marks.
        Item::Choice { label, icon, enabled, options, .. } if options.iter().all(|o| o.cmd.is_none()) => SubMenu {
            label: menu_label(&label),
            icon_name: icon.into(),
            enabled,
            submenu: options
                .into_iter()
                .map(|o| StandardItem { label: menu_label(&o.label), enabled: false, ..Default::default() }.into())
                .collect(),
            ..Default::default()
        }
        .into(),
        Item::Choice { label, icon, enabled, selected, options } => {
            let radios = options
                .iter()
                .map(|o| RadioItem { label: menu_label(&o.label), enabled: o.enabled, ..Default::default() })
                .collect();
            let cmds: Vec<Option<Command>> = options.into_iter().map(|o| o.cmd).collect();
            SubMenu {
                label: menu_label(&label),
                icon_name: icon.into(),
                enabled,
                submenu: vec![
                    RadioGroup {
                        selected: selected.unwrap_or(usize::MAX),
                        select: Box::new(move |t: &mut FscTray, i: usize| {
                            if let Some(Some(cmd)) = cmds.get(i) {
                                t.send(cmd.clone());
                            }
                        }),
                        options: radios,
                    }
                    .into(),
                ],
                ..Default::default()
            }
            .into()
        }
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
        if self.menu.mic_missing {
            desc.push_str(" · microphone not connected");
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
        tray_menu::items(self.st.muted, &self.menu, &self.mics).into_iter().map(to_ksni).collect()
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
    let mut mics = Mics::default();
    mics.update(list_mics().await);
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
                // Only when something changed: an update redraws the open menu.
                if mics.update(list_mics().await) {
                    let m = mics.clone();
                    handle.update(|t| t.mics = m).await;
                }
            }
        }
    }
}

/// `fsc_audio::microphones`, off the async threads (it runs `pw-dump`).
async fn list_mics() -> Vec<fsc_audio::Microphone> {
    tokio::task::spawn_blocking(fsc_audio::microphones).await.unwrap_or_default()
}
