//! fscd's log: everything written to stderr (our messages, GTK's, the
//! speech libraries', panics) goes to `~/.local/state/fsc/fscd.log` with a
//! time on each line, so a bug report has something to show even when fscd
//! was started by Hyprland with its output thrown away. When stderr is a
//! terminal, it still shows there too.
//!
//! The previous run's log is kept as `fscd.log.old`, since a freeze or crash
//! is only reported after the restart that would otherwise wipe it. Lines are
//! synced to disk as they come, so a hard freeze loses as little as
//! possible. The log restarts if it grows past `MAX_BYTES`.

use std::fs::File;
use std::io::{BufRead, BufReader, Seek, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;

const MAX_BYTES: u64 = 8 << 20;

/// The pipe's read end, for `drain` to see what's still in it (-1: none).
static READ_FD: AtomicI32 = AtomicI32::new(-1);
/// The copying thread holds text it hasn't written yet.
static PENDING: AtomicBool = AtomicBool::new(false);

/// Wait (a second at most) until everything written to stderr is in the
/// log: before exiting, and after a panic's message, which would otherwise
/// die in the pipe with the process.
pub fn drain() {
    let fd = READ_FD.load(Ordering::Relaxed);
    if fd < 0 {
        return;
    }
    let until = std::time::Instant::now() + std::time::Duration::from_secs(1);
    while std::time::Instant::now() < until {
        let mut queued: libc::c_int = 0;
        // SAFETY: FIONREAD writes one int; `fd` stays open for the process.
        let ok = unsafe { libc::ioctl(fd, libc::FIONREAD, &mut queued) } == 0;
        if !ok || (queued == 0 && !PENDING.load(Ordering::Acquire)) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    // The thread may have just taken the last bytes off the pipe.
    std::thread::sleep(std::time::Duration::from_millis(20));
}

/// Where the log goes: `$XDG_STATE_HOME/fsc/fscd.log`.
pub fn path() -> Option<PathBuf> {
    let state = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))?;
    Some(state.join("fsc/fscd.log"))
}

/// Send stderr to the log from now on. Call it once, early, from the
/// running daemon only (a second launch must not rotate the first one's
/// log). On failure stderr is left as it was.
pub fn start() -> Option<PathBuf> {
    match open() {
        Ok(path) => Some(path),
        Err(e) => {
            eprintln!("fsc: no log file: {e}");
            None
        }
    }
}

fn open() -> std::io::Result<PathBuf> {
    let path = path().ok_or_else(|| std::io::Error::other("no home directory"))?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    if path.exists() {
        std::fs::rename(&path, path.with_extension("log.old"))?;
    }
    // Only you can read it: it names the tools the agent used and any errors.
    let file = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&path)?;

    let mut fds = [0; 2];
    // SAFETY: plain libc calls on descriptors we own; fd 2 is replaced
    // atomically by dup2, so no write to stderr is lost or misdirected.
    let (read, terminal) = unsafe {
        if libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) != 0 {
            return Err(std::io::Error::last_os_error());
        }
        let terminal = if libc::isatty(2) == 1 {
            let fd = libc::fcntl(2, libc::F_DUPFD_CLOEXEC, 3);
            (fd >= 0).then(|| File::from(OwnedFd::from_raw_fd(fd)))
        } else {
            None
        };
        if libc::dup2(fds[1], 2) < 0 {
            let e = std::io::Error::last_os_error();
            libc::close(fds[0]);
            libc::close(fds[1]);
            return Err(e);
        }
        libc::close(fds[1]);
        (File::from(OwnedFd::from_raw_fd(fds[0])), terminal)
    };
    READ_FD.store(read.as_raw_fd(), Ordering::Relaxed);
    std::thread::Builder::new().name("fsc-log".into()).spawn(move || copy(read, file, terminal))?;
    let report = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        report(info);
        drain();
    }));
    eprintln!(
        "fscd {} started {}",
        env!("CARGO_PKG_VERSION"),
        gtk::glib::DateTime::now_local().ok().and_then(|t| t.format("%Y-%m-%d %H:%M:%S %Z").ok()).unwrap_or_default()
    );
    Ok(path)
}

/// Copy stderr's lines to the log (and the terminal), each with the time.
fn copy(read: File, mut file: File, mut terminal: Option<File>) {
    let mut reader = BufReader::new(read);
    let mut line = Vec::new();
    let mut written = 0u64;
    loop {
        line.clear();
        match reader.read_until(b'\n', &mut line) {
            Ok(0) | Err(_) => return,
            Ok(_) => PENDING.store(true, Ordering::Release),
        }
        if let Some(t) = terminal.as_mut() {
            let _ = t.write_all(&line);
        }
        if !line.ends_with(b"\n") {
            line.push(b'\n');
        }
        if written > MAX_BYTES && file.set_len(0).is_ok() && file.rewind().is_ok() {
            written = 0;
            let _ = file.write_all(b"(log restarted: it passed 8 MB)\n");
        }
        let time = gtk::glib::DateTime::now_local()
            .ok()
            .and_then(|t| t.format("%H:%M:%S").ok())
            .map(|t| format!("{t} "))
            .unwrap_or_default();
        if file.write_all(time.as_bytes()).and_then(|()| file.write_all(&line)).is_ok() {
            written += (time.len() + line.len()) as u64;
        }
        // Sync once the burst is over, not after every line of it.
        if reader.buffer().is_empty() {
            let _ = file.sync_data();
        }
        PENDING.store(!reader.buffer().is_empty(), Ordering::Release);
    }
}
