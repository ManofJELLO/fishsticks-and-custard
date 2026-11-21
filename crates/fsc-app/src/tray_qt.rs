//! The tray as Qt shows it (`qml/TrayIcon.qml`): the macOS menu bar, where
//! Qt's SystemTrayIcon is a native status item. On Linux the tray is a
//! StatusNotifierItem of our own (`tray.rs`); `FSC_QT_TRAY=1` uses this one
//! there instead, to try it.
//!
//! The menu is `tray_menu`'s, sent to QML as JSON. Commands stay here: an
//! item carries the index of its command, and choosing it hands that back.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use tokio::sync::mpsc;

use fsc_core::ipc::Command;

use crate::AppEvent;
use crate::icon::{self, IconState};
use crate::tray_menu::{self, Item, MenuState, Mics, TrayUpdate};

/// Hand the controller's updates to the Qt thread, which shows them.
pub async fn run(initial: IconState, menu: MenuState, mut rx: mpsc::UnboundedReceiver<TrayUpdate>, tx: async_channel::Sender<AppEvent>) {
    let _ = tx.send(AppEvent::Tray(TrayUpdate::Icon(initial))).await;
    let _ = tx.send(AppEvent::Tray(TrayUpdate::Menu(menu))).await;
    let _ = tx.send(AppEvent::TrayMics(list_mics().await)).await;
    while let Some(u) = rx.recv().await {
        if tx.send(AppEvent::Tray(u)).await.is_err() {
            break;
        }
    }
}

/// `fsc_audio::microphones`, off the async threads.
pub async fn list_mics() -> Vec<fsc_audio::Microphone> {
    tokio::task::spawn_blocking(fsc_audio::microphones).await.unwrap_or_default()
}

/// What the Qt tray shows, kept on the Qt thread.
#[derive(Default)]
pub struct QtTray {
    pub icon: Option<IconState>,
    pub menu: Option<MenuState>,
    pub mics: Mics,
    /// The commands of the menu last sent, by index.
    cmds: Vec<Command>,
}

impl QtTray {
    /// The menu as JSON for `TrayIcon.qml` (`None` until the controller has
    /// sent its state). Each choosable entry has an `act`: the index of its
    /// command, for `command`.
    pub fn menu_json(&mut self) -> Option<String> {
        let (menu, muted) = (self.menu.as_ref()?, self.icon.is_some_and(|i| i.muted));
        let mut cmds = Vec::new();
        let mut act = |c: Option<Command>| -> i64 {
            match c {
                Some(c) => {
                    cmds.push(c);
                    cmds.len() as i64 - 1
                }
                None => -1,
            }
        };
        let items: Vec<Value> = tray_menu::items(muted, menu, &self.mics)
            .into_iter()
            .map(|item| match item {
                Item::Separator => json!({ "kind": "sep" }),
                Item::Action { label, cmd, .. } => json!({ "kind": "action", "label": label, "act": act(Some(cmd)) }),
                Item::Check { label, checked, cmd } => {
                    json!({ "kind": "check", "label": label, "checked": checked, "act": act(Some(cmd)) })
                }
                Item::Choice { label, enabled, selected, options, .. } => {
                    let options: Vec<Value> = options
                        .into_iter()
                        .map(|o| json!({ "label": o.label, "enabled": o.enabled, "act": act(o.cmd) }))
                        .collect();
                    let selected = selected.map_or(-1, |i| i as i64);
                    json!({ "kind": "choice", "label": label, "enabled": enabled, "selected": selected, "options": options })
                }
            })
            .collect();
        self.cmds = cmds;
        serde_json::to_string(&items).ok()
    }

    /// The command of the entry chosen.
    pub fn command(&self, act: i32) -> Option<Command> {
        usize::try_from(act).ok().and_then(|i| self.cmds.get(i)).cloned()
    }

    pub fn tooltip(&self) -> String {
        let missing = self.menu.as_ref().is_some_and(|m| m.mic_missing);
        self.icon.map(|i| tray_menu::tooltip(&i, missing)).unwrap_or_default()
    }
}

/// The icon for `st` as a PNG file, written once per look, as a file URL
/// (Qt's tray takes an image by URL). In the state directory, or the
/// temporary one, in a folder for this version: an update that redraws an
/// icon mustn't keep showing the old drawing. Older versions' folders go.
pub fn icon_url(st: &IconState) -> Option<String> {
    let root = fsc_core::paths::state_dir().unwrap_or_else(std::env::temp_dir).join("tray");
    let dir = root.join(env!("CARGO_PKG_VERSION"));
    if !dir.is_dir() {
        clear_old_icons(&root);
    }
    // Every part of the look, so a file never shows another state's icon.
    let name = format!("{:?}-{:?}-{}-{:?}-{:02x}{:02x}{:02x}.png", st.shape, st.phase, st.muted, st.theme, to8(st.accent.0), to8(st.accent.1), to8(st.accent.2))
        .to_lowercase();
    let path: PathBuf = dir.join(name);
    if !path.exists() {
        std::fs::create_dir_all(&dir).ok()?;
        // Twice the menu bar's 22 points, for Retina screens.
        let png = icon::render(44, st).encode_png().ok()?;
        let tmp = path.with_extension("png.tmp");
        std::fs::write(&tmp, png).ok()?;
        std::fs::rename(&tmp, &path).ok()?;
    }
    Some(file_url(&path))
}

/// Everything in `root` (the tray icon folder) left by other versions.
fn clear_old_icons(root: &Path) {
    let Ok(entries) = std::fs::read_dir(root) else { return };
    for e in entries.flatten() {
        let p = e.path();
        let _ = if p.is_dir() { std::fs::remove_dir_all(&p) } else { std::fs::remove_file(&p) };
    }
}

/// `path` (absolute) as a file URL: `file:///Users/…`, or on Windows
/// `file:///C:/Users/…` (not `file://C:\Users\…`, where `C:` would be read
/// as the host).
fn file_url(path: &Path) -> String {
    let p = path.display().to_string();
    let p = if cfg!(windows) { p.replace('\\', "/") } else { p };
    if p.starts_with('/') { format!("file://{p}") } else { format!("file:///{p}") }
}

fn to8(c: f64) -> u8 {
    (c.clamp(0.0, 1.0) * 255.0).round() as u8
}

#[cfg(test)]
mod tests {
    use super::*;
    use fsc_core::config::{AgentKind, SpeakMode};

    #[test]
    fn icon_urls_are_file_urls() {
        #[cfg(unix)]
        assert_eq!(file_url(Path::new("/Users/u/Library/Application Support/fsc/x.png")), "file:///Users/u/Library/Application Support/fsc/x.png");
        #[cfg(windows)]
        assert_eq!(file_url(Path::new(r"C:\Users\u\x.png")), "file:///C:/Users/u/x.png");
    }

    #[test]
    fn menu_json_carries_commands_by_index() {
        let mut t = QtTray {
            menu: Some(MenuState {
                agent: AgentKind::Claude,
                agents: AgentKind::ALL.map(|_| true),
                choices: Default::default(),
                mic: String::new(),
                mic_missing: false,
                speak: SpeakMode::Voice,
                language: "en",
                voice: "en_GB-alan-medium".into(),
                fetching: None,
            }),
            ..Default::default()
        };
        let v: Value = serde_json::from_str(&t.menu_json().unwrap()).unwrap();
        assert_eq!(v[0]["kind"], "action");
        assert_eq!(t.command(v[0]["act"].as_i64().unwrap() as i32), Some(Command::Type));
        // "Model: not loaded" offers nothing to choose.
        let model = v.as_array().unwrap().iter().find(|i| i["label"] == "Model: not loaded").unwrap();
        assert_eq!(model["options"][0]["act"], -1);
        assert_eq!(model["enabled"], false);
        assert_eq!(t.command(-1), None);
        assert_eq!(t.command(10_000), None);
    }
}
