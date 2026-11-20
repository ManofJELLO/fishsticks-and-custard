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

/// How long the client waits for fscd's reply.
const REPLY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Blocking client used by the `fsc` CLI.
pub fn send(cmd: &Command) -> Result<Reply> {
    let mut line = serde_json::to_string(cmd)?;
    line.push('\n');
    let reply = transport::exchange(line)?;
    if reply.is_empty() {
        bail!("fscd closed the connection without replying");
    }
    Ok(serde_json::from_str(&reply)?)
}

/// Send one request line and read one reply line.
fn talk<S>(stream: &S, line: &str) -> std::io::Result<String>
where
    for<'a> &'a S: std::io::Read + Write,
{
    let mut w = stream;
    w.write_all(line.as_bytes())?;
    let mut reply = String::new();
    BufReader::new(stream).read_line(&mut reply)?;
    Ok(reply)
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

    pub type Conn = tokio::net::UnixStream;

    /// `$XDG_RUNTIME_DIR/fsc.sock`, or a private directory in the user's home
    /// when there's no runtime dir. Never a shared path like /tmp, where another
    /// user could squat the name.
    pub fn socket_path() -> PathBuf {
        #[cfg(test)]
        if let Some(path) = tests::SOCKET.get() {
            return path.clone();
        }
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

    pub fn exchange(line: String) -> Result<String> {
        let path = socket_path();
        let stream = UnixStream::connect(&path)
            .with_context(|| format!("fscd is not running (no socket at {})", path.display()))?;
        stream.set_read_timeout(Some(REPLY_TIMEOUT))?;
        Ok(talk(&stream, &line)?)
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
    use std::time::Instant;
    use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};

    pub type Conn = NamedPipeServer;

    /// `\\.\pipe\fsc-<SID>`. Pipe names are machine-wide, so the user's SID
    /// keeps two people's daemons apart (user names, once filtered, might not).
    pub fn address() -> String {
        match sec::own_sid() {
            Some(sid) => format!(r"\\.\pipe\fsc-{sid}"),
            None => {
                let user = std::env::var("USERNAME").unwrap_or_default();
                let user: String = user.chars().filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_').collect();
                format!(r"\\.\pipe\fsc-{user}")
            }
        }
    }

    pub fn exchange(line: String) -> Result<String> {
        let name = address();
        let deadline = Instant::now() + REPLY_TIMEOUT;
        let pipe = loop {
            match std::fs::OpenOptions::new().read(true).write(true).open(&name) {
                Ok(pipe) => break pipe,
                // Every instance is taken (another client is mid-call):
                // wait for one to come free rather than say fscd isn't there.
                Err(e) if e.raw_os_error() == Some(sec::ERROR_PIPE_BUSY) && Instant::now() < deadline => {
                    sec::wait_pipe(&name, deadline.saturating_duration_since(Instant::now()));
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    return Err(e).with_context(|| format!("fscd is not running (no pipe {name})"));
                }
                Err(e) => return Err(e).with_context(|| format!("could not reach fscd at {name}")),
            }
        };
        // Anyone can create a pipe of any name: only talk to our own fscd.
        if !sec::server_is_ours(&pipe) {
            bail!("{name} belongs to a program run by another user; not sending to it");
        }
        // A pipe read can't time out, so read on a thread and stop waiting
        // for it instead; the CLI exits soon after, taking the thread along.
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(talk(&pipe, &line));
        });
        match rx.recv_timeout(REPLY_TIMEOUT) {
            Ok(reply) => Ok(reply?),
            Err(_) => bail!("fscd did not reply within {} s", REPLY_TIMEOUT.as_secs()),
        }
    }

    /// A pipe instance only this user can open, local clients only.
    fn create(first: bool) -> std::io::Result<NamedPipeServer> {
        let sd = sec::OwnerOnly::new()?;
        let mut attrs = sd.attributes();
        let mut opts = ServerOptions::new();
        opts.first_pipe_instance(first).reject_remote_clients(true);
        // SAFETY: `attrs`, and the descriptor in `sd` it points to, outlive
        // the call (the pipe keeps its own copy).
        unsafe { opts.create_with_security_attributes_raw(address(), (&raw mut attrs).cast()) }
    }

    /// The daemon's end: one pipe instance waiting for the next client.
    pub struct Listener {
        next: NamedPipeServer,
    }

    impl Listener {
        /// Create the pipe, refusing if another fscd already has it
        /// (`first_pipe_instance` fails when the name is taken).
        pub async fn bind() -> Result<Self> {
            let next = create(true)
                .with_context(|| format!("fscd is already running, or another program holds {}", address()))?;
            Ok(Self { next })
        }

        pub async fn accept(&mut self) -> std::io::Result<Conn> {
            let connected = self.next.connect().await;
            // A fresh instance waits for the next client either way: one
            // whose connect failed (say its client gave up first) can't be
            // used again, and keeping it would fail every accept after.
            let fresh = create(false)?;
            let conn = std::mem::replace(&mut self.next, fresh);
            connected.map(|()| conn)
        }
    }

    /// A pipe goes when its last handle closes.
    pub fn cleanup() {}

    /// The Win32 calls behind a private pipe.
    mod sec {
        use std::os::windows::io::AsRawHandle;
        use std::ptr::null_mut;
        use std::sync::OnceLock;
        use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, LocalFree};
        use windows_sys::Win32::Security::Authorization::{
            ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
        };
        use windows_sys::Win32::Security::{
            GetTokenInformation, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER, TokenUser,
        };
        use windows_sys::Win32::System::Pipes::{GetNamedPipeServerProcessId, WaitNamedPipeW};
        use windows_sys::Win32::System::Threading::{
            GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
        };

        pub const ERROR_PIPE_BUSY: i32 = windows_sys::Win32::Foundation::ERROR_PIPE_BUSY as i32;

        fn wide(s: &str) -> Vec<u16> {
            s.encode_utf16().chain(std::iter::once(0)).collect()
        }

        /// The user a process runs as, as a SID string ("S-1-5-21-…").
        fn process_sid(process: HANDLE) -> Option<String> {
            // SAFETY: plain Win32 calls; every buffer is sized from the
            // call before it, and every handle and allocation is released.
            unsafe {
                let mut token: HANDLE = null_mut();
                if OpenProcessToken(process, TOKEN_QUERY, &mut token) == 0 {
                    return None;
                }
                let mut len = 0u32;
                GetTokenInformation(token, TokenUser, null_mut(), 0, &mut len);
                // u64s: TOKEN_USER holds a pointer, so the buffer must be aligned.
                let mut buf = vec![0u64; (len as usize).div_ceil(8)];
                let ok = len > 0 && GetTokenInformation(token, TokenUser, buf.as_mut_ptr().cast(), len, &mut len) != 0;
                CloseHandle(token);
                if !ok {
                    return None;
                }
                let user = &*buf.as_ptr().cast::<TOKEN_USER>();
                let mut s = null_mut();
                if ConvertSidToStringSidW(user.User.Sid, &mut s) == 0 {
                    return None;
                }
                let n = (0..).take_while(|&i| *s.add(i) != 0).count();
                let sid = String::from_utf16_lossy(std::slice::from_raw_parts(s, n));
                LocalFree(s.cast());
                Some(sid)
            }
        }

        /// This process's user.
        pub fn own_sid() -> Option<String> {
            static SID: OnceLock<Option<String>> = OnceLock::new();
            // SAFETY: the current-process pseudo handle needs no closing.
            SID.get_or_init(|| process_sid(unsafe { GetCurrentProcess() })).clone()
        }

        /// Whether the process serving `pipe` runs as this user.
        pub fn server_is_ours(pipe: &std::fs::File) -> bool {
            let Some(own) = own_sid() else { return false };
            // SAFETY: `pipe` is an open pipe handle; the process handle is closed.
            unsafe {
                let mut pid = 0u32;
                if GetNamedPipeServerProcessId(pipe.as_raw_handle(), &mut pid) == 0 {
                    return false;
                }
                let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
                if process.is_null() {
                    return false;
                }
                let sid = process_sid(process);
                CloseHandle(process);
                sid.as_deref() == Some(own.as_str())
            }
        }

        /// Wait (at most `wait`) for an instance of the pipe to come free.
        pub fn wait_pipe(name: &str, wait: std::time::Duration) {
            let ms = wait.as_millis().clamp(1, u32::MAX as u128) as u32;
            // SAFETY: a NUL-terminated wide string that outlives the call.
            unsafe { WaitNamedPipeW(wide(name).as_ptr(), ms) };
        }

        /// A security descriptor whose DACL lets in this user and no one else.
        pub struct OwnerOnly(PSECURITY_DESCRIPTOR);

        impl OwnerOnly {
            pub fn new() -> std::io::Result<Self> {
                let sid = own_sid().ok_or_else(|| std::io::Error::other("could not tell which user this is"))?;
                // Protected DACL, one entry: full access for this user.
                let sddl = wide(&format!("D:P(A;;GA;;;{sid})"));
                let mut sd: PSECURITY_DESCRIPTOR = null_mut();
                // SAFETY: a NUL-terminated string in, a LocalAlloc'd descriptor out.
                let ok = unsafe {
                    ConvertStringSecurityDescriptorToSecurityDescriptorW(sddl.as_ptr(), SDDL_REVISION_1, &mut sd, null_mut())
                };
                if ok == 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(Self(sd))
            }

            pub fn attributes(&self) -> SECURITY_ATTRIBUTES {
                SECURITY_ATTRIBUTES {
                    nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
                    lpSecurityDescriptor: self.0,
                    bInheritHandle: 0,
                }
            }
        }

        impl Drop for OwnerOnly {
            fn drop(&mut self) {
                // SAFETY: allocated by ConvertStringSecurityDescriptor…W.
                unsafe { LocalFree(self.0) };
            }
        }
    }
}

/// Remove what `Listener::bind` made (the socket file), on the way out.
pub fn cleanup() {
    transport::cleanup();
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    /// The socket the tests use in place of the real one (set here, not
    /// through XDG_RUNTIME_DIR: other tests read the environment alongside).
    pub static SOCKET: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();

    /// A command goes through the listener and its reply comes back; a
    /// second daemon is refused while the first listens.
    #[test]
    fn round_trip() {
        let dir = std::env::temp_dir().join(format!("fsc-ipc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        SOCKET.set(dir.join("fsc.sock")).unwrap();
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
