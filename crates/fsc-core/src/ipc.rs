//! Line-delimited JSON between `fsc` (and the tray, scripts, binds) and
//! `fscd`: one request line in, one reply line out.
//!
//! The transport is the platform's local IPC: a Unix socket in
//! `$XDG_RUNTIME_DIR` on Linux and macOS, a named pipe on Windows. Both are
//! private to the user. `send` is the blocking client; `Listener` is the
//! daemon's end.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Write};
#[cfg(unix)]
use std::path::PathBuf;

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
    /// Speak and listen in another language (a `lang` code; "" or "default"
    /// follows the desktop's), or list them (`None`). Saved.
    Language { code: Option<String> },
    /// Speak with another Piper voice ("" or "default": the language's
    /// usual one), or list the language's voices (`None`). Saved.
    Voice { name: Option<String> },
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

/// Blocking client used by the `fsc` CLI.
pub fn send(cmd: &Command) -> Result<Reply> {
    let stream = transport::connect()?;
    let mut line = serde_json::to_string(cmd)?;
    line.push('\n');
    (&stream).write_all(line.as_bytes())?;
    let mut reply = String::new();
    BufReader::new(&stream).read_line(&mut reply)?;
    if reply.is_empty() {
        bail!("fscd closed the connection without replying");
    }
    Ok(serde_json::from_str(&reply)?)
}

/// Where `fscd` listens, for messages: a socket path or a pipe name.
pub fn address() -> String {
    transport::address()
}

pub use transport::{Conn, Listener};

#[cfg(unix)]
mod transport {
    use super::*;
    use std::os::unix::net::UnixStream;
    use std::time::Duration;

    pub type Conn = tokio::net::UnixStream;

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

    pub fn address() -> String {
        socket_path().display().to_string()
    }

    pub fn connect() -> Result<UnixStream> {
        let path = socket_path();
        let stream = UnixStream::connect(&path)
            .with_context(|| format!("fscd is not running (no socket at {})", path.display()))?;
        stream.set_read_timeout(Some(Duration::from_secs(5)))?;
        Ok(stream)
    }

    /// The daemon's end of the socket.
    pub struct Listener(tokio::net::UnixListener);

    impl Listener {
        /// Bind the socket, refusing if another fscd is already listening.
        pub async fn bind() -> Result<Self> {
            let path = socket_path();
            if let Some(dir) = path.parent()
                && !dir.exists()
            {
                use std::os::unix::fs::DirBuilderExt;
                std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
            }
            if path.exists() {
                if tokio::net::UnixStream::connect(&path).await.is_ok() {
                    bail!("fscd is already running ({})", path.display());
                }
                let _ = std::fs::remove_file(&path);
            }
            Ok(Self(tokio::net::UnixListener::bind(&path)?))
        }

        pub async fn accept(&mut self) -> std::io::Result<Conn> {
            self.0.accept().await.map(|(s, _)| s)
        }
    }

    /// Remove the socket (on the way out).
    pub fn cleanup() {
        let _ = std::fs::remove_file(socket_path());
    }
}

#[cfg(windows)]
mod transport {
    use super::*;
    use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};

    pub type Conn = NamedPipeServer;

    /// `\\.\pipe\fsc-<user>`: named pipes are machine-wide, so the user's
    /// name keeps two people's daemons apart.
    pub fn address() -> String {
        let user = std::env::var("USERNAME").unwrap_or_default();
        let user: String = user.chars().filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_').collect();
        format!(r"\\.\pipe\fsc-{user}")
    }

    pub fn connect() -> Result<std::fs::File> {
        let name = address();
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&name)
            .with_context(|| format!("fscd is not running (no pipe {name})"))
    }

    /// The daemon's end: one pipe instance waiting for the next client.
    pub struct Listener {
        next: NamedPipeServer,
    }

    impl Listener {
        /// Create the pipe, refusing if another fscd already has it
        /// (`first_pipe_instance` fails when the name is taken).
        pub async fn bind() -> Result<Self> {
            let name = address();
            let next = ServerOptions::new()
                .first_pipe_instance(true)
                .reject_remote_clients(true)
                .create(&name)
                .with_context(|| format!("fscd is already running ({name})"))?;
            Ok(Self { next })
        }

        pub async fn accept(&mut self) -> std::io::Result<Conn> {
            self.next.connect().await?;
            let fresh = ServerOptions::new().reject_remote_clients(true).create(address())?;
            Ok(std::mem::replace(&mut self.next, fresh))
        }
    }

    /// A pipe goes when its last handle closes.
    pub fn cleanup() {}
}

/// Remove what `Listener::bind` made (the socket file), on the way out.
pub fn cleanup() {
    transport::cleanup();
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    /// A command goes through the listener and its reply comes back; a
    /// second daemon is refused while the first listens.
    #[test]
    fn round_trip() {
        let dir = std::env::temp_dir().join(format!("fsc-ipc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // SAFETY: the only test reading XDG_RUNTIME_DIR.
        unsafe { std::env::set_var("XDG_RUNTIME_DIR", &dir) };
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let mut listener = rt.block_on(Listener::bind()).unwrap();
        assert!(rt.block_on(Listener::bind()).is_err(), "a second daemon must be refused");
        let server = rt.spawn(async move {
            // The refused bind's "anyone there?" probe connects first.
            loop {
                let conn = listener.accept().await.unwrap();
                let (r, mut w) = tokio::io::split(conn);
                let Some(line) = BufReader::new(r).lines().next_line().await.unwrap() else { continue };
                assert_eq!(serde_json::from_str::<Command>(&line).unwrap(), Command::Status);
                w.write_all(b"{\"ok\":true,\"state\":\"idle\"}\n").await.unwrap();
                break;
            }
        });
        let client = std::thread::spawn(|| send(&Command::Status));
        rt.block_on(server).unwrap();
        let reply = client.join().unwrap().unwrap();
        assert_eq!(reply.state.as_deref(), Some("idle"));
        cleanup();
        assert!(send(&Command::Status).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
