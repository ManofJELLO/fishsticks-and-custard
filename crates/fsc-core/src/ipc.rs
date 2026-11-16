//! Line-delimited JSON over a Unix socket in `$XDG_RUNTIME_DIR`.
//! One request line in, one reply line out.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Command {
    /// Show the popup if hidden, hide it if shown.
    Toggle,
    Show,
    Hide,
    /// Show the popup with the text field focused.
    Type,
    /// Start voice capture immediately, skipping the wake word.
    Listen,
    /// Stop listening, or interrupt the reply in progress.
    Stop,
    /// Push-to-talk key released: end the recording if one is running (a
    /// quick tap keeps listening). Never interrupts a reply.
    Release,
    /// Send a prompt without opening the text field.
    Ask { text: String },
    /// Answer the oldest open permission question. `always`: and save a
    /// rule so it isn't asked again.
    Allow { always: bool },
    /// Refuse the oldest open permission question.
    Deny,
    /// Throw away the conversation and start a fresh agent session.
    New,
    /// Mute the wake word. `None` toggles.
    Mute { on: Option<bool> },
    /// Listen for another wake phrase from now on (saved), or say which
    /// one it listens for (`None`).
    Wake { phrase: Option<String> },
    Status,
    /// Print what the assistant remembers between sessions.
    Memory,
    /// Delete long-term memory and start a fresh session.
    Forget,
    /// Switch to another agent (a `config::AgentKind` key: "claude", "codex", ...)
    /// from now on, or say which one answers (`None`).
    Agent { backend: Option<String> },
    /// Use another model (an id the agent lists), or list them (`None`).
    Model { value: Option<String> },
    /// Use another mode (an id the agent lists), or list them (`None`).
    Mode { value: Option<String> },
    /// Listen with another microphone (saved; "" or "default" is the
    /// system's), or list them (`None`).
    Mic { name: Option<String> },
    /// When to read replies aloud ("voice", "always" or "never"; saved), or
    /// say which (`None`).
    Speak { mode: Option<String> },
    /// Open the settings: HyprCustard when it's installed, otherwise the
    /// config file in the desktop's editor.
    Settings,
    Quit,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Reply {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl Reply {
    pub fn ok() -> Self {
        Reply { ok: true, ..Default::default() }
    }
    pub fn state(s: impl Into<String>) -> Self {
        Reply { ok: true, state: Some(s.into()), error: None }
    }
    pub fn err(e: impl ToString) -> Self {
        Reply { ok: false, state: None, error: Some(e.to_string()) }
    }
}

/// `$XDG_RUNTIME_DIR/fsc.sock`, or a private directory in the user's home
/// when there's no runtime dir. Never a shared path like /tmp, where another
/// user could squat the name.
pub fn socket_path() -> PathBuf {
    if let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR").filter(|d| !d.is_empty()) {
        return PathBuf::from(dir).join("fsc.sock");
    }
    directories::BaseDirs::new()
        .map(|d| d.data_local_dir().join("fsc").join("run"))
        .unwrap_or_else(|| PathBuf::from(".fsc-run"))
        .join("fsc.sock")
}

/// Blocking client used by the `fsc` CLI.
pub fn send(cmd: &Command) -> Result<Reply> {
    let path = socket_path();
    let mut stream = UnixStream::connect(&path).with_context(|| {
        format!("fscd is not running (no socket at {})", path.display())
    })?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    let mut line = serde_json::to_string(cmd)?;
    line.push('\n');
    stream.write_all(line.as_bytes())?;
    let mut reply = String::new();
    BufReader::new(stream).read_line(&mut reply)?;
    if reply.is_empty() {
        bail!("fscd closed the connection without replying");
    }
    Ok(serde_json::from_str(&reply)?)
}
