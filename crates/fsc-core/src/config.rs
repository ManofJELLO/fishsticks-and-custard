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
    /// In a conversation, hang up after this long with nobody speaking
    /// (0 = wait until the popup is closed).
    pub hang_up_after_secs: u32,
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
            end_silence_ms: 1500,
            no_speech_timeout_ms: 6000,
            max_utterance_secs: 30,
            hold_mic_while_speaking: true,
            conversation: true,
            hang_up_after_secs: 60,
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
    /// `persona.md` and `.claude/settings.local.json` (the tool allow-list).
    /// Defaults to `~/.local/share/fsc/home`.
    pub home: Option<PathBuf>,
    /// Passed to `--setting-sources`. "project" keeps your personal
    /// Claude Code settings/hooks out of the voice assistant.
    pub setting_sources: String,
    /// Extra CLI arguments appended verbatim.
    pub extra_args: Vec<String>,
    /// Start a fresh Claude session when this many minutes have passed since
    /// the last exchange, so context doesn't grow forever. 0 = never.
    pub session_idle_minutes: u32,
    /// Before a session ends, save durable facts about the user to
    /// `<home>/memory.md` and load them into new sessions.
    pub memory: bool,
}

impl Default for ClaudeConfig {
    fn default() -> Self {
        Self {
            bin: "claude".into(),
            model: "sonnet".into(),
            home: None,
            setting_sources: "project,local".into(),
            extra_args: Vec::new(),
            session_idle_minutes: 20,
            memory: true,
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
    #[default]
    PoliceBox,
    Fishbone,
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
    /// Layer only: panel opacity, 0..1. (Windows take Hyprland's `opacity` rule.)
    pub opacity: f64,
    /// Layer only: fade the panel, most see-through behind the visualiser
    /// and most solid behind text, scaled by `opacity`.
    pub gradient: bool,
    pub width: i32,
    /// Max height of the conversation area before it scrolls.
    pub max_height: i32,
    pub font: String,
    /// The one colour allowed to shout: "system" (the desktop's accent, else
    /// the GTK theme's selection colour) or hex, e.g. "#D71921".
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
            opacity: 0.92,
            gradient: false,
            width: 680,
            max_height: 360,
            font: "0xProto Nerd Font".into(),
            accent: "system".into(),
            theme: ThemePref::Auto,
            tray_icon: TrayIcon::PoliceBox,
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

    pub fn claude_home(&self) -> PathBuf {
        self.claude.home.clone().unwrap_or_else(|| {
            directories::BaseDirs::new()
                .map(|d| d.data_dir().join("fsc").join("home"))
                .unwrap_or_else(|| PathBuf::from(".fsc-home"))
        })
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
}
