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
    /// Send a prompt without opening the text field.
    Ask { text: String },
    /// Throw away the conversation and start a fresh Claude session.
    New,
    /// Mute the wake word. `None` toggles.
    Mute { on: Option<bool> },
    Status,
    /// Print what the assistant remembers between sessions.
    Memory,
    /// Delete long-term memory and start a fresh session.
    Forget,
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

pub fn socket_path() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
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
