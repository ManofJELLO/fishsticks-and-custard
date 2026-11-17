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
    /// That mic is named but not connected (said in the tooltip).
    pub mic_missing: bool,
    pub speak: SpeakMode,
    /// The language's code (`fsc_core::lang`).
    pub language: &'static str,
    /// The Piper voice in use, and one being downloaded (percent).
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

struct FscTray {
    st: IconState,
    menu: MenuState,
    /// The microphones PipeWire has, looked up again whenever the menu opens.
    mics: Vec<fsc_audio::Microphone>,
    /// Every mic seen while running, id -> name: to name one that's gone.
    seen: std::collections::HashMap<String, String>,
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
        // What to save for each entry (a mic's id; "" is the default), and
        // what to show.
        let mut values: Vec<String> = vec![String::new()];
        let mut labels: Vec<String> = vec!["System default".into()];
        for m in &self.mics {
            values.push(fsc_audio::device_setting(&m.id));
            labels.push(menu_label(&m.name));
        }
        let selected = if default {
            0
        } else {
            match fsc_audio::which_mic(current, &self.mics) {
                Some(m) => self.mics.iter().position(|x| x.id == m.id).map_or(0, |i| i + 1),
                None => {
                    let id = current.strip_prefix("pipewire:").unwrap_or(current);
                    let name = self.seen.get(id).map(String::as_str).unwrap_or(id);
                    values.push(current.to_string());
                    labels.push(format!("{} (not connected)", menu_label(name)));
                    values.len() - 1
                }
            }
        };
        let title = labels[selected].clone();
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

    /// Language ▸ each language, in its own name.
    fn language_menu(&self) -> MenuItem<Self> {
        let langs = &fsc_core::lang::LANGUAGES;
        let selected = langs.iter().position(|l| l.code == self.menu.language).unwrap_or(0);
        SubMenu {
            label: format!("Language: {}", langs[selected].native),
            icon_name: "preferences-desktop-locale-symbolic".into(),
            submenu: vec![
                RadioGroup {
                    selected,
                    select: Box::new(|t: &mut Self, i: usize| {
                        if let Some(l) = fsc_core::lang::LANGUAGES.get(i) {
                            t.send(Command::Language { code: Some(l.code.into()) });
                        }
                    }),
                    options: langs
                        .iter()
                        .map(|l| RadioItem {
                            label: if l.native == l.name { l.native.into() } else { format!("{} · {}", l.native, l.name) },
                            ..Default::default()
                        })
                        .collect(),
                }
                .into(),
            ],
            ..Default::default()
        }
        .into()
    }

    /// Voice ▸ the language's voices; ones still to download say how big.
    fn voice_menu(&self) -> MenuItem<Self> {
        let m = &self.menu;
        let mut keys: Vec<String> = fsc_core::voices::for_language(m.language).map(|v| v.key.to_string()).collect();
        if !keys.contains(&m.voice) {
            keys.push(m.voice.clone()); // set by hand in config.toml
        }
        let fetching = |key: &str| m.fetching.as_ref().filter(|(v, _)| v == key).map(|(_, pct)| *pct);
        let label = |key: &str| -> String {
            let info = fsc_core::voices::find(key);
            let name = info.map(|v| v.label.to_string()).unwrap_or_else(|| key.to_string());
            match (fetching(key), info) {
                (Some(pct), _) => format!("{name} · downloading {pct}%"),
                (None, Some(v)) if !fsc_core::models::has_voice(key) => format!("{name} · {} MB", v.mb),
                _ => name,
            }
        };
        let selected = keys.iter().position(|k| *k == m.voice).unwrap_or(0);
        let title = label(&m.voice);
        let options = keys.iter().map(|k| RadioItem { label: menu_label(&label(k)), ..Default::default() }).collect();
        SubMenu {
            label: format!("Voice: {}", menu_label(&title)),
            icon_name: "audio-speakers-symbolic".into(),
            submenu: vec![
                RadioGroup {
                    selected,
                    select: Box::new(move |t: &mut Self, i: usize| {
                        if let Some(k) = keys.get(i) {
                            t.send(Command::Voice { name: Some(k.clone()) });
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
            icon_name: "audio-volume-high-symbolic".into(),
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
            self.language_menu(),
            self.voice_menu(),
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
    let seen = mics.iter().map(|m| (m.id.clone(), m.name.clone())).collect();
    let handle = match (FscTray { st: initial, menu, mics: mics.clone(), seen, refresh, tx }).assume_sni_available(true).spawn().await {
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
                    handle
                        .update(|t| {
                            t.seen.extend(m.iter().map(|x| (x.id.clone(), x.name.clone())));
                            t.mics = m;
                        })
                        .await;
                }
            }
        }
    }
}

/// `fsc_audio::microphones`, off the async threads (it runs `pw-dump`).
async fn list_mics() -> Vec<fsc_audio::Microphone> {
    tokio::task::spawn_blocking(fsc_audio::microphones).await.unwrap_or_default()
}
