//! The tray menu as plain data, for every platform: what each item says,
//! which is chosen, what's greyed out, and the command each one sends. The
//! Linux tray (StatusNotifierItem) and the Qt one (the macOS menu bar) both
//! show this, so they can't drift apart.
//!
//! The menu keeps its length and order whatever happens: items are greyed
//! out, never added or removed (a menu that shifts under the pointer sets up
//! a misclick).

use std::collections::HashMap;

use fsc_audio::Microphone;
use fsc_core::agent::{Choices, Select};
use fsc_core::config::{AgentKind, Config, SpeakMode};
use fsc_core::ipc::Command;

use crate::icon::IconState;

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

/// The microphones to list: those connected now, and every one seen while
/// running (id -> name), to name a chosen one that's gone.
#[derive(Default, Clone)]
pub struct Mics {
    pub now: Vec<Microphone>,
    pub seen: HashMap<String, String>,
}

impl Mics {
    /// Take a new list. False if it's the same as before.
    pub fn update(&mut self, now: Vec<Microphone>) -> bool {
        self.seen.extend(now.iter().map(|m| (m.id.clone(), m.name.clone())));
        if now == self.now {
            return false;
        }
        self.now = now;
        true
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Item {
    Action { label: String, icon: &'static str, cmd: Command },
    Check { label: String, checked: bool, cmd: Command },
    /// A submenu of options, one of them chosen (if any). Options without
    /// a command are notes ("Not loaded yet"), shown greyed out.
    Choice { label: String, icon: &'static str, enabled: bool, selected: Option<usize>, options: Vec<Opt> },
    Separator,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Opt {
    pub label: String,
    pub enabled: bool,
    pub cmd: Option<Command>,
}

impl Opt {
    fn new(label: impl Into<String>, cmd: Command) -> Self {
        Self { label: label.into(), enabled: true, cmd: Some(cmd) }
    }
}

/// The tray icon's tooltip: what it's doing, and what's wrong.
pub fn tooltip(st: &IconState, mic_missing: bool) -> String {
    let mut desc = st.phase.label().to_lowercase();
    if st.muted {
        desc.push_str(" · wake word muted");
    }
    if mic_missing {
        desc.push_str(" · microphone not connected");
    }
    desc
}

/// The whole menu, top to bottom.
pub fn items(muted: bool, m: &MenuState, mics: &Mics) -> Vec<Item> {
    let action = |label: &str, icon, cmd| Item::Action { label: label.into(), icon, cmd };
    vec![
        action("Type…", "input-keyboard-symbolic", Command::Type),
        action("Talk", "audio-input-microphone-symbolic", Command::Listen),
        Item::Separator,
        Item::Check { label: "Mute wake word".into(), checked: muted, cmd: Command::Mute { on: None } },
        mic_menu(m, mics),
        speak_menu(m),
        language_menu(m),
        voice_menu(m),
        Item::Separator,
        agent_menu(m),
        choice_menu("Model", m.choices.model.as_ref(), |v| Command::Model { value: Some(v) }),
        choice_menu("Mode", m.choices.mode.as_ref(), |v| Command::Mode { value: Some(v) }),
        action("New conversation", "", Command::New),
        action("Forget memories", "edit-clear-history-symbolic", Command::Forget),
        Item::Separator,
        action("Settings…", "preferences-system-symbolic", Command::Settings),
        action("Quit", "", Command::Quit),
    ]
}

fn choice(label: String, icon: &'static str, selected: Option<usize>, options: Vec<Opt>) -> Item {
    Item::Choice { label, icon, enabled: true, selected, options }
}

/// Agent ▸ Claude / Codex / Antigravity / OpenCode (/ Custom): the ones not
/// installed are greyed out, a custom one only shows when it's set up.
fn agent_menu(m: &MenuState) -> Item {
    let shown: Vec<(AgentKind, bool)> = AgentKind::ALL
        .into_iter()
        .zip(m.agents)
        .filter(|&(k, ok)| k != AgentKind::Acp || ok || k == m.agent)
        .collect();
    let selected = shown.iter().position(|&(k, _)| k == m.agent).unwrap_or(0);
    let options = shown
        .iter()
        .map(|&(k, ok)| Opt {
            label: if ok { k.name().into() } else { format!("{} (not installed)", k.name()) },
            enabled: ok || k == m.agent,
            cmd: Some(Command::Agent { backend: Some(k.key().into()) }),
        })
        .collect();
    choice(format!("Agent: {}", m.agent.name()), "system-users-symbolic", Some(selected), options)
}

/// Model ▸ or Mode ▸: the current agent's options. Always there, so the
/// menu keeps its length and order; greyed out until the agent has said
/// what it offers (while it starts, or if it offers none).
fn choice_menu(title: &str, sel: Option<&Select>, cmd: fn(String) -> Command) -> Item {
    let Some(sel) = sel.filter(|s| !s.options.is_empty()) else {
        return Item::Choice {
            label: format!("{title}: not loaded"),
            icon: "",
            enabled: false,
            selected: None,
            options: vec![Opt { label: "Not loaded yet".into(), enabled: false, cmd: None }],
        };
    };
    let selected = sel.options.iter().position(|o| o.id == sel.current);
    let options = sel.options.iter().map(|o| Opt::new(o.name.clone(), cmd(o.id.clone()))).collect();
    choice(format!("{title}: {}", sel.current_name()), "", selected, options)
}

/// Microphone ▸ System default / each mic there is. A saved mic that isn't
/// plugged in stays listed (and chosen) so you can see why.
fn mic_menu(m: &MenuState, mics: &Mics) -> Item {
    let current = m.mic.trim();
    let set = |v: String| Command::Mic { name: Some(v) };
    let mut options = vec![Opt::new("System default", set(String::new()))];
    options.extend(mics.now.iter().map(|x| Opt::new(x.name.clone(), set(fsc_audio::device_setting(&x.id)))));
    let selected = if current.is_empty() || current.eq_ignore_ascii_case("default") {
        0
    } else {
        match fsc_audio::which_mic(current, &mics.now) {
            Some(x) => mics.now.iter().position(|y| y.id == x.id).map_or(0, |i| i + 1),
            None => {
                let id = current.strip_prefix("pipewire:").unwrap_or(current);
                let name = mics.seen.get(id).map(String::as_str).unwrap_or(id);
                options.push(Opt::new(format!("{name} (not connected)"), set(current.to_string())));
                options.len() - 1
            }
        }
    };
    let title = format!("Microphone: {}", options[selected].label);
    choice(title, "audio-input-microphone-symbolic", Some(selected), options)
}

/// Language ▸ each language, in its own name.
fn language_menu(m: &MenuState) -> Item {
    let langs = &fsc_core::lang::LANGUAGES;
    let selected = langs.iter().position(|l| l.code == m.language).unwrap_or(0);
    let options = langs
        .iter()
        .map(|l| {
            let label = if l.native == l.name { l.native.into() } else { format!("{} · {}", l.native, l.name) };
            Opt::new(label, Command::Language { code: Some(l.code.into()) })
        })
        .collect();
    choice(format!("Language: {}", langs[selected].native), "preferences-desktop-locale-symbolic", Some(selected), options)
}

/// Voice ▸ the language's voices; ones still to download say how big.
fn voice_menu(m: &MenuState) -> Item {
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
    let options = keys.iter().map(|k| Opt::new(label(k), Command::Voice { name: Some(k.clone()) })).collect();
    choice(format!("Voice: {}", label(&m.voice)), "audio-speakers-symbolic", Some(selected), options)
}

/// Speak replies ▸ When I talk / Always / Never.
fn speak_menu(m: &MenuState) -> Item {
    let selected = SpeakMode::ALL.iter().position(|&s| s == m.speak).unwrap_or(0);
    let options = SpeakMode::ALL.iter().map(|s| Opt::new(s.name(), Command::Speak { mode: Some(s.key().into()) })).collect();
    choice(format!("Speak replies: {}", m.speak.name()), "audio-volume-high-symbolic", Some(selected), options)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> MenuState {
        MenuState {
            agent: AgentKind::Claude,
            agents: AgentKind::ALL.map(|k| k == AgentKind::Claude),
            choices: Choices::default(),
            mic: String::new(),
            mic_missing: false,
            speak: SpeakMode::Voice,
            language: "en",
            voice: "en_GB-alan-medium".into(),
            fetching: None,
        }
    }

    #[test]
    fn the_menu_keeps_its_length_whatever_the_state() {
        let mut m = state();
        let before = items(false, &m, &Mics::default()).len();
        m.mic = "pipewire:42".into();
        m.agents = [true; AgentKind::ALL.len()];
        let mics = Mics { now: vec![Microphone { id: "7".into(), name: "USB".into() }], ..Default::default() };
        assert_eq!(items(true, &m, &mics).len(), before);
    }

    #[test]
    fn a_missing_mic_stays_listed_and_chosen() {
        let mut m = state();
        m.mic = "pipewire:42".into();
        let mut mics = Mics::default();
        mics.seen.insert("42".into(), "Headset".into());
        let Item::Choice { label, selected, options, .. } = mic_menu(&m, &mics) else { panic!() };
        assert_eq!(label, "Microphone: Headset (not connected)");
        assert_eq!(selected, Some(1));
        assert_eq!(options[1].cmd, Some(Command::Mic { name: Some("pipewire:42".into()) }));
    }

    #[test]
    fn models_are_greyed_until_loaded() {
        let Item::Choice { enabled, options, .. } = choice_menu("Model", None, |v| Command::Model { value: Some(v) }) else {
            panic!()
        };
        assert!(!enabled);
        assert_eq!(options[0].cmd, None);
    }
}
