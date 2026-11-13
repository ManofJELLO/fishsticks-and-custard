//! `~/.config/fsc/config.toml`. Every field is optional.

use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Config {
    pub claude: ClaudeConfig,
    pub ui: UiConfig,
    pub voice: VoiceConfig,
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

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct VoiceConfig {
    /// whisper.cpp model name: the file is `ggml-<name>.bin`.
    pub whisper_model: String,
    /// Spoken language for recognition ("en", "auto", ...).
    pub language: String,
    /// Run whisper on the GPU (Vulkan).
    pub gpu: bool,
    /// Piper voice, e.g. "en_GB-alan-medium".
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
    /// Listen for the wake phrase whenever the mic isn't otherwise in use.
    pub wake_word: bool,
    pub wake_phrase: String,
}

impl Default for VoiceConfig {
    fn default() -> Self {
        Self {
            whisper_model: "large-v3-turbo-q5_0".into(),
            language: "en".into(),
            gpu: true,
            piper_voice: "en_GB-alan-medium".into(),
            speech_rate: 1.0,
            speak: SpeakMode::Voice,
            input_device: String::new(),
            end_silence_ms: 800,
            no_speech_timeout_ms: 6000,
            max_utterance_secs: 30,
            hold_mic_while_speaking: true,
            conversation: true,
            wake_word: true,
            wake_phrase: "fishsticks and custard".into(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ClaudeConfig {
    /// Path or name of the Claude Code CLI.
    pub bin: String,
    /// Model alias or full id passed to `--model`.
    pub model: String,
    /// Working directory for the assistant's Claude session. Holds
    /// `persona.md` and `.claude/settings.json` (the tool allow-list).
    /// Defaults to `~/.local/share/fsc/home`.
    pub home: Option<PathBuf>,
    /// Passed to `--setting-sources`. "project" keeps your personal
    /// Claude Code settings/hooks out of the voice assistant.
    pub setting_sources: String,
    /// Extra CLI arguments appended verbatim.
    pub extra_args: Vec<String>,
}

impl Default for ClaudeConfig {
    fn default() -> Self {
        Self {
            bin: "claude".into(),
            model: "sonnet".into(),
            home: None,
            setting_sources: "project".into(),
            extra_args: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Position {
    #[default]
    Top,
    Center,
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
#[serde(rename_all = "lowercase")]
pub enum GlowStyle {
    /// Glow in the opposite of the theme colour (white on dark, black on light).
    #[default]
    Contrast,
    /// Glow in the accent colour.
    Accent,
    None,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct UiConfig {
    pub position: Position,
    /// Distance from the top edge when `position = "top"`.
    pub margin: i32,
    pub width: i32,
    /// Max height of the conversation area before it scrolls.
    pub max_height: i32,
    pub font: String,
    /// The one colour allowed to shout. Hex, e.g. "#D71921".
    pub accent: String,
    pub theme: ThemePref,
    /// Tray icon glow while recording.
    pub glow: GlowStyle,
    /// Send a desktop notification when a reply finishes while the popup is hidden.
    pub notify_when_hidden: bool,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            position: Position::Top,
            margin: 80,
            width: 680,
            max_height: 360,
            font: "0xProto Nerd Font".into(),
            accent: "#D71921".into(),
            theme: ThemePref::Auto,
            glow: GlowStyle::Contrast,
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

    pub fn claude_home(&self) -> PathBuf {
        self.claude.home.clone().unwrap_or_else(|| {
            directories::BaseDirs::new()
                .map(|d| d.data_dir().join("fsc").join("home"))
                .unwrap_or_else(|| PathBuf::from(".fsc-home"))
        })
    }
}
