//! `~/.config/fsc/config.toml`. Every field is optional.

use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Config {
    pub claude: ClaudeConfig,
    pub ui: UiConfig,
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
