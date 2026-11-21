//! `~/.config/fsc/config.toml`. Every field is optional.

use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Config {
    pub agent: AgentConfig,
    pub claude: ClaudeConfig,
    pub codex: AcpAgentConfig,
    pub antigravity: AcpAgentConfig,
    pub opencode: AcpAgentConfig,
    /// Any other ACP agent.
    pub acp: AcpAgentConfig,
    pub ui: UiConfig,
    pub voice: VoiceConfig,
    pub keys: KeysConfig,
}

/// Global keys fscd binds itself: on macOS and Windows. (On Linux the
/// compositor binds them, e.g. Hyprland's `bind`s running `fsc type`.)
/// Written like "alt+a", "option+shift+a", "ctrl+alt+space"; empty for none.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct KeysConfig {
    /// Open the popup to type.
    #[serde(rename = "type")]
    pub type_key: String,
    /// Hold to talk; a tap starts listening until you stop talking.
    pub talk: String,
}

impl Default for KeysConfig {
    fn default() -> Self {
        Self { type_key: "alt+a".into(), talk: "alt+shift+a".into() }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum SpeakMode {
    /// Speak replies to spoken prompts; typed prompts stay silent.
    #[default]
    Voice,
    Always,
    Never,
}

impl SpeakMode {
    pub const ALL: [SpeakMode; 3] = [SpeakMode::Voice, SpeakMode::Always, SpeakMode::Never];

    /// As written in config.toml.
    pub fn key(self) -> &'static str {
        match self {
            SpeakMode::Voice => "voice",
            SpeakMode::Always => "always",
            SpeakMode::Never => "never",
        }
    }

    pub fn from_key(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|m| m.key().eq_ignore_ascii_case(s.trim()))
    }

    /// For the tray menu.
    pub fn name(self) -> &'static str {
        match self {
            SpeakMode::Voice => "When I talk",
            SpeakMode::Always => "Always",
            SpeakMode::Never => "Never",
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct VoiceConfig {
    /// whisper.cpp model name: the file is `ggml-<name>.bin`.
    pub whisper_model: String,
    /// The language spoken and heard ("de"): see `lang`. Empty follows the
    /// desktop's locale.
    pub language: String,
    /// Run whisper on the GPU (Vulkan).
    pub gpu: bool,
    /// Piper voice, e.g. "en_GB-alan-medium". Empty: the language's usual one.
    pub piper_voice: String,
    /// >1 speaks faster, <1 slower.
    pub speech_rate: f32,
    pub speak: SpeakMode,
    /// Substring of the input device name. Empty = system default.
    pub input_device: String,
    /// Silence after speech that ends an utterance.
    pub end_silence_ms: u32,
    /// Give up if no speech starts within this long.
    pub no_speech_timeout_ms: u32,
    /// Hard cap on one utterance.
    pub max_utterance_secs: u32,
    /// Keep the mic open until the spoken reply finishes. Needed for
    /// Bluetooth headsets, whose output device changes when the mic closes.
    pub hold_mic_while_speaking: bool,
    /// After a spoken reply, listen again automatically and keep the mic
    /// open (headset in two-way mode) until the popup is closed.
    pub conversation: bool,
    /// In a conversation, hang up after this long with nobody speaking
    /// (0 = wait until the popup is closed).
    pub hang_up_after_secs: u32,
    /// Listen for the wake phrase whenever the mic isn't otherwise in use.
    /// Off is "muted": the tray's Mute wake word and `fsc mute` save it.
    pub wake_word: bool,
    pub wake_phrase: String,
    /// The whisper model that listens for the wake phrase: small, so it's
    /// heard fast; what's asked is heard with `whisper_model`. Empty: use
    /// `whisper_model` for both.
    pub wake_model: String,
}

impl Default for VoiceConfig {
    fn default() -> Self {
        Self {
            whisper_model: "large-v3-turbo-q5_0".into(),
            language: String::new(),
            gpu: true,
            piper_voice: String::new(),
            speech_rate: 1.0,
            speak: SpeakMode::Voice,
            input_device: String::new(),
            end_silence_ms: 1500,
            no_speech_timeout_ms: 6000,
            max_utterance_secs: 30,
            hold_mic_while_speaking: true,
            conversation: true,
            hang_up_after_secs: 60,
            wake_word: true,
            wake_phrase: "fishsticks and custard".into(),
            wake_model: "small-q8_0".into(),
        }
    }
}

impl VoiceConfig {
    /// What `language` comes to (the locale's when it's empty).
    pub fn spoken(&self) -> crate::lang::Spoken {
        crate::lang::resolve(&self.language)
    }

    /// The Piper voice to speak with: `piper_voice`, unless that's for
    /// another of the listed languages (left over from before a language
    /// change), else the language's usual one.
    pub fn voice(&self) -> String {
        voice_for(&self.spoken(), &self.piper_voice)
    }
}

/// The voice `spoken` uses with `piper_voice` set as given.
pub fn voice_for(spoken: &crate::lang::Spoken, piper_voice: &str) -> String {
    let chosen = piper_voice.trim();
    let listed = spoken.whisper == spoken.code;
    if !chosen.is_empty() && (!listed || voice_language(chosen) == spoken.code) {
        return chosen.to_string();
    }
    spoken.language().voice(&spoken.region).to_string()
}

/// "pt_BR-faber-medium" -> "pt".
pub fn voice_language(voice: &str) -> &str {
    voice.split(['_', '-']).next().unwrap_or("")
}

/// An agent's usual program and set-up.
#[derive(Debug, Clone, Copy)]
pub struct Preset {
    pub program: &'static str,
    pub args: &'static [&'static str],
    /// Sign-in command to suggest when the agent says it needs one.
    pub login: Option<&'static str>,
    /// What to install when the program can't be found.
    pub install: Option<&'static str>,
    /// The session mode to ask for unless the config names one.
    pub mode: &'static str,
}

/// Which agent answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum AgentKind {
    /// Claude Code, settings in `[claude]`.
    #[default]
    Claude,
    /// OpenAI Codex through `codex-acp` (sign in with `codex login`).
    Codex,
    /// Google Antigravity CLI through `agy acp` (sign in with `agy acp --login`).
    Antigravity,
    /// OpenCode through `opencode acp`: free models, or your own providers
    /// (`opencode auth login`).
    #[serde(rename = "opencode")]
    OpenCode,
    /// Any other Agent Client Protocol agent: set `[acp] command`.
    Acp,
}

impl AgentKind {
    pub const ALL: [AgentKind; 5] =
        [AgentKind::Claude, AgentKind::Codex, AgentKind::Antigravity, AgentKind::OpenCode, AgentKind::Acp];

    /// As written in the config and on the command line.
    pub fn key(self) -> &'static str {
        match self {
            AgentKind::Claude => "claude",
            AgentKind::Codex => "codex",
            AgentKind::Antigravity => "antigravity",
            AgentKind::OpenCode => "opencode",
            AgentKind::Acp => "acp",
        }
    }

    pub fn from_key(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.key().eq_ignore_ascii_case(s.trim()))
    }

    /// How this agent is usually run. (Claude's program is `[claude] bin`.)
    pub fn preset(self) -> Preset {
        match self {
            AgentKind::Codex => Preset {
                program: "codex-acp",
                args: &[],
                login: Some("codex login"),
                install: Some("codex-acp (npm i -g @agentclientprotocol/codex-acp)"),
                // Ask before edits, commands and network use.
                mode: "read-only",
            },
            AgentKind::Antigravity => Preset {
                program: "agy",
                args: &["acp"],
                login: Some("agy acp --login"),
                install: Some("antigravity-cli"),
                mode: "",
            },
            AgentKind::OpenCode => Preset {
                program: "opencode",
                args: &["acp"],
                login: Some("opencode auth login"),
                install: Some("opencode"),
                mode: "",
            },
            AgentKind::Claude => Preset { program: "claude", args: &[], login: None, install: None, mode: "" },
            AgentKind::Acp => Preset { program: "", args: &[], login: None, install: None, mode: "" },
        }
    }

    /// "claude, codex, antigravity, opencode or acp".
    pub fn all_keys() -> String {
        let keys: Vec<&str> = Self::ALL.iter().map(|k| k.key()).collect();
        let (last, rest) = keys.split_last().expect("there are agents");
        format!("{} or {last}", rest.join(", "))
    }

    /// For people: "Codex wants to ...".
    pub fn name(self) -> &'static str {
        match self {
            AgentKind::Claude => "Claude",
            AgentKind::Codex => "Codex",
            AgentKind::Antigravity => "Antigravity",
            AgentKind::OpenCode => "OpenCode",
            AgentKind::Acp => "Custom agent",
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct AgentConfig {
    pub backend: AgentKind,
    /// ACP agents only: tool kinds that run without asking. The rest (edit, delete, move,
    /// execute, fetch, other) are asked about, when the agent asks at all. Opening a web
    /// page (fetch) isn't here by default: a page can carry instructions for the agent.
    /// Anything touching `permission::SECRET_PATHS` is always asked about.
    pub auto_allow: Vec<String>,
    /// The assistant's home: persona, memory, and the agent's own settings
    /// (ACP agents work in its `workspace` folder). Defaults to
    /// `~/.local/share/fsc/home`. See `Config::home`.
    pub home: Option<PathBuf>,
    /// Start a fresh session when this many minutes have passed since the
    /// last exchange, so context doesn't grow forever. 0 = never. Default 20.
    pub session_idle_minutes: Option<u32>,
    /// Before a session ends, save durable facts about the user to
    /// `<home>/memory.md` and load them into new sessions. Default on.
    pub memory: Option<bool>,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            backend: AgentKind::Claude,
            auto_allow: ["read", "search", "think"].map(String::from).to_vec(),
            home: None,
            session_idle_minutes: None,
            memory: None,
        }
    }
}

/// `[codex]`, `[antigravity]` and `[acp]`: one ACP agent's settings.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct AcpAgentConfig {
    /// The agent's program. Empty: the usual one (`codex-acp`, `agy`).
    pub command: String,
    /// Arguments. With `command` set, the whole list (agy needs "acp");
    /// otherwise added after the usual ones.
    pub args: Vec<String>,
    /// Extra environment variables for the agent.
    pub env: std::collections::BTreeMap<String, String>,
    /// Model id as the agent lists it. Empty: the agent's default.
    pub model: String,
    /// The agent's session mode (its approval preset), e.g. Codex's
    /// "read-only" or Antigravity's "default". Empty: the usual one.
    pub mode: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ClaudeConfig {
    /// Path or name of the Claude Code CLI.
    pub bin: String,
    /// Model alias or full id passed to `--model`.
    pub model: String,
    /// Claude Code's permission mode: "default" (ask), "acceptEdits",
    /// "auto" or "plan". Empty: default.
    pub permission_mode: String,
    /// Old place for `[agent] home`, still read when that isn't set.
    pub home: Option<PathBuf>,
    /// Passed to `--setting-sources`. "project" keeps your personal
    /// Claude Code settings/hooks out of the voice assistant.
    pub setting_sources: String,
    /// Extra CLI arguments appended verbatim.
    pub extra_args: Vec<String>,
    /// Old place for `[agent] session_idle_minutes`.
    pub session_idle_minutes: Option<u32>,
    /// Old place for `[agent] memory`.
    pub memory: Option<bool>,
}

impl Default for ClaudeConfig {
    fn default() -> Self {
        Self {
            bin: "claude".into(),
            model: "sonnet".into(),
            permission_mode: String::new(),
            home: None,
            setting_sources: "project,local".into(),
            extra_args: Vec::new(),
            session_idle_minutes: None,
            memory: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Surface {
    /// A normal window: Hyprland's window rules place, decorate and animate it.
    #[default]
    Window,
    /// A layer-shell popup pinned to a screen edge or corner (`anchor`).
    Layer,
    /// No popup at all: voice only, the tray icon shows the state.
    Headless,
}

/// Where a layer popup sits on the screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum Anchor {
    #[default]
    Top,
    TopLeft,
    TopRight,
    Bottom,
    BottomLeft,
    BottomRight,
    Left,
    Right,
    Center,
}

impl Anchor {
    /// (top, bottom, left, right) edges to pin to.
    pub fn edges(self) -> (bool, bool, bool, bool) {
        match self {
            Anchor::Top => (true, false, false, false),
            Anchor::TopLeft => (true, false, true, false),
            Anchor::TopRight => (true, false, false, true),
            Anchor::Bottom => (false, true, false, false),
            Anchor::BottomLeft => (false, true, true, false),
            Anchor::BottomRight => (false, true, false, true),
            Anchor::Left => (false, false, true, false),
            Anchor::Right => (false, false, false, true),
            Anchor::Center => (false, false, false, false),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ThemePref {
    /// Follow the desktop portal's colour-scheme.
    #[default]
    Auto,
    Dark,
    Light,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum TrayIcon {
    #[cfg_attr(not(target_os = "macos"), default)]
    PoliceBox,
    Fishbone,
    /// One colour, so it suits the macOS menu bar (where it's the default):
    /// the police box's colours are lost there, leaving a blob.
    #[cfg_attr(target_os = "macos", default)]
    Fez,
}

impl TrayIcon {
    /// Drawn in one colour (tinted by the macOS menu bar to suit itself).
    pub fn one_colour(self) -> bool {
        self != TrayIcon::PoliceBox
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Visualiser {
    /// Frequency bands as LED-segment bars.
    #[default]
    Spectrum,
    /// Loudness over time, scrolling right to left.
    Loudness,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct UiConfig {
    pub surface: Surface,
    /// Layer only: which edge or corner to pin to. (`position` is the old name.)
    #[serde(alias = "position")]
    pub anchor: Anchor,
    /// Layer only: distance from the top or bottom edge it's pinned to.
    pub margin: i32,
    /// Layer only: distance from the left or right edge it's pinned to.
    pub margin_x: i32,
    /// Layer only: output name ("DP-1"); empty = the focused one.
    pub monitor: String,
    /// Layer and macOS: panel opacity, 0..1. (Windows on Linux take
    /// Hyprland's `opacity` rule.) On macOS it's the tint over the glass.
    pub opacity: f64,
    /// Layer and macOS: fade the panel, most see-through behind the
    /// visualiser and most solid behind text, scaled by `opacity`.
    pub gradient: bool,
    /// macOS only: the system's glass behind the panel (Liquid Glass on
    /// macOS 26 and later, else the frosted blur).
    pub glass: bool,
    pub width: i32,
    /// Max height of the conversation area before it scrolls.
    pub max_height: i32,
    pub font: String,
    /// The one colour allowed to shout: "system" (the desktop's accent, else
    /// the Qt palette's) or hex, e.g. "#D71921".
    pub accent: String,
    pub theme: ThemePref,
    pub tray_icon: TrayIcon,
    pub visualiser: Visualiser,
    /// Frequency bands shown by the spectrum visualiser.
    pub bands: u32,
    /// Each visualiser bar is split into blocks this many percent tall.
    pub block_percent: u32,
    /// Send a desktop notification when a reply finishes while the popup is hidden.
    pub notify_when_hidden: bool,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            surface: Surface::Window,
            anchor: Anchor::Top,
            margin: 80,
            margin_x: 24,
            monitor: String::new(),
            // Over the glass on macOS, a tint you can see through.
            opacity: if cfg!(target_os = "macos") { 0.5 } else { 0.92 },
            gradient: false,
            glass: true,
            width: 680,
            max_height: 360,
            font: "0xProto Nerd Font".into(),
            accent: "system".into(),
            theme: ThemePref::Auto,
            tray_icon: TrayIcon::default(),
            visualiser: Visualiser::Spectrum,
            bands: 16,
            block_percent: 10,
            notify_when_hidden: true,
        }
    }
}

impl Config {
    pub fn path() -> Option<PathBuf> {
        directories::BaseDirs::new().map(|d| d.config_dir().join("fsc").join("config.toml"))
    }

    pub fn load() -> Result<Self> {
        let Some(path) = Self::path() else { return Ok(Self::default()) };
        match std::fs::read_to_string(&path) {
            Ok(s) => toml::from_str(&s).with_context(|| format!("parsing {}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
        }
    }

    /// Whether long-term memory is kept (`[agent] memory`).
    pub fn memory(&self) -> bool {
        self.agent.memory.or(self.claude.memory).unwrap_or(true)
    }

    /// `[agent] session_idle_minutes`.
    pub fn session_idle_minutes(&self) -> u32 {
        self.agent.session_idle_minutes.or(self.claude.session_idle_minutes).unwrap_or(20)
    }

    /// The agent's name, for messages: "Claude wants to ...".
    pub fn agent_name(&self) -> &'static str {
        self.agent.backend.name()
    }

    /// The settings of an ACP agent (`Claude` has none: `[acp]`'s are given).
    pub fn acp_agent(&self, kind: AgentKind) -> &AcpAgentConfig {
        match kind {
            AgentKind::Codex => &self.codex,
            AgentKind::Antigravity => &self.antigravity,
            AgentKind::OpenCode => &self.opencode,
            AgentKind::Acp | AgentKind::Claude => &self.acp,
        }
    }

    /// The program that runs `kind`. Empty: none configured.
    pub fn program(&self, kind: AgentKind) -> String {
        if kind == AgentKind::Claude {
            return self.claude.bin.clone();
        }
        let own = &self.acp_agent(kind).command;
        if own.is_empty() { kind.preset().program.to_string() } else { own.clone() }
    }

    /// Use `value` as `kind`'s model (or mode) from now on, in this config.
    pub fn set_choice(&mut self, kind: AgentKind, mode: bool, value: &str) {
        let slot = match (kind, mode) {
            (AgentKind::Claude, false) => &mut self.claude.model,
            (AgentKind::Claude, true) => &mut self.claude.permission_mode,
            (k, false) => &mut self.acp_agent_mut(k).model,
            (k, true) => &mut self.acp_agent_mut(k).mode,
        };
        *slot = value.to_string();
    }

    fn acp_agent_mut(&mut self, kind: AgentKind) -> &mut AcpAgentConfig {
        match kind {
            AgentKind::Codex => &mut self.codex,
            AgentKind::Antigravity => &mut self.antigravity,
            AgentKind::OpenCode => &mut self.opencode,
            AgentKind::Acp | AgentKind::Claude => &mut self.acp,
        }
    }

    /// Whether `kind` can run here: its program is configured and found.
    pub fn available(&self, kind: AgentKind) -> bool {
        let prog = self.program(kind);
        !prog.is_empty() && find_program(&prog).is_some()
    }

    /// The model to show before the agent says which it's using.
    pub fn model_hint(&self) -> String {
        match self.agent.backend {
            AgentKind::Claude => self.claude.model.clone(),
            k if !self.acp_agent(k).model.is_empty() => self.acp_agent(k).model.clone(),
            k => k.name().to_string(),
        }
    }

    /// Switch to `kind` from now on: set `[agent] backend` in the config
    /// file, keeping the rest of it (comments included) as it is.
    pub fn save_backend(kind: AgentKind) -> Result<()> {
        Self::save_value("agent", "backend", kind.key())
    }

    /// Where `kind`'s model or mode is kept: (table, key).
    pub fn setting_key(kind: AgentKind, mode: bool) -> (&'static str, &'static str) {
        match (kind, mode) {
            (AgentKind::Claude, false) => ("claude", "model"),
            (AgentKind::Claude, true) => ("claude", "permission_mode"),
            (k, false) => (k.key(), "model"),
            (k, true) => (k.key(), "mode"),
        }
    }

    /// Set `[table] key = value` in the config file, keeping the rest of it
    /// (comments included) as it is.
    pub fn save_value(table: &str, key: &str, value: impl Into<toml_edit::Value>) -> Result<()> {
        let path = Self::path().context("no config directory")?;
        let text = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        let new = set_value(&text, table, key, value).with_context(|| format!("parsing {}", path.display()))?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        // Write a copy and rename it over, so a crash can't leave half a file.
        let tmp = path.with_extension("toml.new");
        std::fs::write(&tmp, new).with_context(|| format!("writing {}", tmp.display()))?;
        std::fs::rename(&tmp, &path).with_context(|| format!("replacing {}", path.display()))?;
        Ok(())
    }

    /// The config file, created first if there isn't one: the packaged
    /// example (every setting, with its default and a note) with its values
    /// commented out, so later versions' defaults still apply.
    pub fn ensure_file() -> Result<PathBuf> {
        let path = Self::path().context("no config directory")?;
        if path.exists() {
            return Ok(path);
        }
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        const EXAMPLE: &str = "/usr/share/fishsticks-and-custard/config.example.toml";
        let text = match std::fs::read_to_string(EXAMPLE) {
            Ok(example) => commented_out(&example),
            Err(_) => format!("# Fishsticks & Custard settings. Every key is optional; see {EXAMPLE}.\n"),
        };
        std::fs::write(&path, text).with_context(|| format!("writing {}", path.display()))?;
        Ok(path)
    }

    /// The assistant's home (`[agent] home`).
    pub fn home(&self) -> PathBuf {
        let base = directories::BaseDirs::new();
        match self.agent.home.as_ref().or(self.claude.home.as_ref()) {
            Some(p) => expand_tilde(p, base.as_ref().map(|d| d.home_dir())),
            None => base
                .map(|d| d.data_dir().join("fsc").join("home"))
                .unwrap_or_else(|| PathBuf::from(".fsc-home")),
        }
    }
}

/// `text` (a config file) with `[table] key` set to `value`.
fn set_value(text: &str, table: &str, key: &str, value: impl Into<toml_edit::Value>) -> Result<String> {
    let value = value.into();
    let mut doc: toml_edit::DocumentMut = text.parse()?;
    let t = doc.entry(table).or_insert_with(toml_edit::table);
    let t = t.as_table_like_mut().with_context(|| format!("`{table}` isn't a table"))?;
    match t.get_mut(key) {
        // Keep the comment that follows the value.
        Some(item) if item.is_value() => {
            let decor = item.as_value().map(|v| v.decor().clone());
            *item = toml_edit::value(value);
            if let (Some(d), Some(v)) = (decor, item.as_value_mut()) {
                *v.decor_mut() = d;
            }
        }
        _ => {
            t.insert(key, toml_edit::value(value));
        }
    }
    Ok(doc.to_string())
}

#[cfg(test)]
fn set_backend(text: &str, kind: AgentKind) -> Result<String> {
    set_value(text, "agent", "backend", kind.key())
}

/// A config file's settings turned into comments; table headers and
/// comments stay as they are.
fn commented_out(text: &str) -> String {
    text.lines()
        .map(|l| {
            let t = l.trim_start();
            if t.is_empty() || t.starts_with('#') || t.starts_with('[') { format!("{l}\n") } else { format!("# {l}\n") }
        })
        .collect()
}

/// Look `prog` up like a shell would: a path as given, a name on `$PATH`.
/// On Windows a bare name also matches with each of `%PATHEXT%`'s endings
/// (`codex` finds `codex.cmd`).
pub fn find_program(prog: &str) -> Option<PathBuf> {
    if prog.contains('/') || (cfg!(windows) && prog.contains('\\')) {
        let p = PathBuf::from(prog);
        return runnable(&p).then_some(p);
    }
    let dirs: Vec<PathBuf> = std::env::split_paths(&std::env::var_os("PATH")?).collect();
    dirs.iter().find_map(|d| with_exe_suffixes(&d.join(prog)).into_iter().find(|p| runnable(p)))
}

#[cfg(unix)]
fn runnable(p: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    p.metadata().is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn runnable(p: &std::path::Path) -> bool {
    p.is_file()
}

/// `p`, and on Windows `p` with each executable ending.
fn with_exe_suffixes(p: &std::path::Path) -> Vec<PathBuf> {
    let mut out = vec![p.to_path_buf()];
    if cfg!(windows) && p.extension().is_none() {
        let exts = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into());
        out.extend(exts.split(';').filter(|e| !e.is_empty()).map(|e| p.with_extension(e.trim_start_matches('.'))));
    }
    out
}

/// "~" or "~/x" -> under the home directory, as a shell would. TOML has no
/// such expansion, but config paths are naturally written that way.
fn expand_tilde(p: &std::path::Path, home: Option<&std::path::Path>) -> PathBuf {
    match (p.strip_prefix("~"), home) {
        (Ok(rest), Some(home)) => home.join(rest),
        _ => p.to_path_buf(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_ui_keys_still_load() {
        // 0.5.x configs: `position` was renamed `anchor`, `glow` was removed.
        let c: Config = toml::from_str("[ui]\nposition = \"center\"\nglow = \"accent\"\n").unwrap();
        assert_eq!(c.ui.anchor, Anchor::Center);
        assert_eq!(c.ui.surface, Surface::Window);
        let c: Config = toml::from_str("[ui]\nsurface = \"layer\"\nanchor = \"bottom-right\"\n").unwrap();
        assert_eq!((c.ui.surface, c.ui.anchor), (Surface::Layer, Anchor::BottomRight));
        assert_eq!(c.ui.anchor.edges(), (false, true, false, true));
    }

    #[test]
    fn agent_section() {
        let c: Config = toml::from_str("").unwrap();
        assert_eq!(c.agent.backend, AgentKind::Claude);
        let c: Config = toml::from_str("[agent]\nbackend = \"codex\"\n[codex]\nmodel = \"gpt-6-luna[low]\"\n").unwrap();
        assert_eq!(c.agent.backend, AgentKind::Codex);
        assert_eq!(c.codex.model, "gpt-6-luna[low]");
        assert_eq!(c.program(AgentKind::Antigravity), "agy");
        assert_eq!(c.program(AgentKind::Acp), "");
        assert!(!c.available(AgentKind::Acp));
        assert!(c.agent.auto_allow.iter().any(|k| k == "read"));
        assert_eq!(c.model_hint(), "gpt-6-luna[low]");
        assert!(c.memory());
        assert_eq!(c.session_idle_minutes(), 20);
    }

    #[test]
    fn saving_the_backend_keeps_the_file() {
        let text = "# my settings\n[agent]\nbackend = \"claude\"   # which agent\nmemory = true\n\n[ui]\nwidth = 700\n";
        let new = set_backend(text, AgentKind::Codex).unwrap();
        assert_eq!(new, text.replace("\"claude\"", "\"codex\""));
        // No [agent] table yet: one is added.
        let new = set_backend("[ui]\nwidth = 700\n", AgentKind::Antigravity).unwrap();
        let c: Config = toml::from_str(&new).unwrap();
        assert_eq!((c.agent.backend, c.ui.width), (AgentKind::Antigravity, 700));
        assert_eq!(set_backend("", AgentKind::Acp).unwrap().trim(), "[agent]\nbackend = \"acp\"");
        assert!(set_backend("not = [toml", AgentKind::Acp).is_err());
        assert_eq!(AgentKind::from_key(" Codex "), Some(AgentKind::Codex));
        assert_eq!(AgentKind::all_keys(), "claude, codex, antigravity, opencode or acp");
        assert_eq!(AgentKind::from_key("gemini"), None);
    }

    #[test]
    fn programs_are_found() {
        #[cfg(unix)]
        {
            assert!(find_program("sh").is_some());
            assert!(find_program("/bin/sh").is_some());
        }
        // A bare name finds the .exe through PATHEXT.
        #[cfg(windows)]
        {
            assert!(find_program("cmd").is_some());
            assert!(find_program(r"C:\Windows\System32\cmd.exe").is_some());
        }
        assert!(find_program("surely-not-a-program-here").is_none());
    }

    #[test]
    fn old_claude_keys_still_count() {
        let c: Config = toml::from_str("[claude]\nmemory = false\nsession_idle_minutes = 5\nhome = \"/srv/a\"\n").unwrap();
        assert!(!c.memory());
        assert_eq!(c.session_idle_minutes(), 5);
        assert_eq!(c.home(), PathBuf::from("/srv/a"));
        // [agent] wins when both are set.
        let c: Config = toml::from_str("[claude]\nmemory = false\n[agent]\nmemory = true\nhome = \"/srv/b\"\n").unwrap();
        assert!(c.memory());
        assert_eq!(c.home(), PathBuf::from("/srv/b"));
    }

    #[test]
    fn tilde_paths() {
        let home = std::path::Path::new("/home/u");
        let t = |p: &str| expand_tilde(std::path::Path::new(p), Some(home));
        assert_eq!(t("~/.local/share/fsc/home"), PathBuf::from("/home/u/.local/share/fsc/home"));
        assert_eq!(t("~"), PathBuf::from("/home/u"));
        assert_eq!(t("/srv/fsc"), PathBuf::from("/srv/fsc"));
        assert_eq!(t("~other/x"), PathBuf::from("~other/x"));
    }

    #[test]
    fn starter_config_keeps_defaults_open() {
        let example = include_str!("../../../packaging/config.example.toml");
        let text = commented_out(example);
        let cfg: Config = toml::from_str(&text).unwrap();
        assert_eq!(cfg.voice.end_silence_ms, Config::default().voice.end_silence_ms);
        // Every setting is a comment; the tables are there to add to.
        assert!(text.lines().all(|l| l.trim().is_empty() || l.trim_start().starts_with(['#', '['])));
        assert!(text.contains("[voice]") && text.contains("# speak = \"voice\""));
        // Saving into it keeps working.
        let saved = set_value(&text, "voice", "speak", "always").unwrap();
        let cfg: Config = toml::from_str(&saved).unwrap();
        assert_eq!(cfg.voice.speak, SpeakMode::Always);
    }

    #[test]
    fn example_keys_are_the_defaults() {
        let example = include_str!("../../../packaging/config.example.toml");
        let cfg: Config = toml::from_str(example).unwrap();
        assert_eq!(cfg.keys, KeysConfig::default());
        let none: Config = toml::from_str("[keys]\ntype = \"\"").unwrap();
        assert_eq!((none.keys.type_key.as_str(), none.keys.talk.as_str()), ("", "alt+shift+a"));
    }

    #[test]
    fn mute_is_saved_as_a_bool() {
        let text = "[voice]\nwake_word = true # comment\n";
        let saved = set_value(text, "voice", "wake_word", false).unwrap();
        assert_eq!(saved, "[voice]\nwake_word = false # comment\n");
        let cfg: Config = toml::from_str(&saved).unwrap();
        assert!(!cfg.voice.wake_word);
    }

    #[test]
    fn speak_mode_keys() {
        for m in SpeakMode::ALL {
            assert_eq!(SpeakMode::from_key(m.key()), Some(m));
            let cfg: Config = toml::from_str(&format!("[voice]\nspeak = \"{}\"", m.key())).unwrap();
            assert_eq!(cfg.voice.speak, m);
        }
        assert_eq!(SpeakMode::from_key(" Always "), Some(SpeakMode::Always));
        assert_eq!(SpeakMode::from_key("loud"), None);
    }
}
