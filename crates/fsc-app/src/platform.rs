//! The few things fscd asks of the desktop that differ per platform:
//! notifications, opening a file or program detached, and quit signals.

use std::path::Path;

/// A desktop notification (`notify-send` on Linux). Long text is cut short.
pub fn notify(text: &str) {
    let mut body: String = text.trim().chars().take(280).collect();
    if text.trim().chars().count() > 280 {
        body.push('…');
    }
    if body.is_empty() {
        return;
    }
    #[cfg(target_os = "linux")]
    tokio::spawn(async move {
        let _ = tokio::process::Command::new("notify-send")
            .args(["--app-name=Fishsticks & Custard", "Fishsticks & Custard", &body])
            .status()
            .await;
    });
    // TODO(cross-platform): the Qt tray's balloon message on Windows/macOS.
    #[cfg(not(target_os = "linux"))]
    eprintln!("fsc: {body}");
}

/// Start `prog` with `args`, detached: its own process group, so it
/// outlives fscd and isn't sent fscd's signals. Reaped in the background.
pub fn spawn_detached(prog: &str, args: &[String]) -> std::io::Result<()> {
    let mut cmd = std::process::Command::new(prog);
    cmd.args(args).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let mut child = cmd.spawn()?;
    std::thread::spawn(move || child.wait());
    Ok(())
}

/// The program and arguments that open `path` in the desktop's app for it.
pub fn opener(path: &Path) -> (&'static str, Vec<String>) {
    let p = path.display().to_string();
    if cfg!(target_os = "macos") {
        ("open", vec![p])
    } else if cfg!(windows) {
        ("explorer", vec![p])
    } else {
        ("xdg-open", vec![p])
    }
}

/// Wait for a request to quit: SIGINT or SIGTERM on Unix, Ctrl+C elsewhere.
/// Returns false if they can't be watched.
pub async fn quit_signal() -> bool {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        use std::sync::OnceLock;
        use tokio::sync::Mutex;
        // Kept between calls, so a second signal is caught too.
        static SIGNALS: OnceLock<Option<Mutex<(tokio::signal::unix::Signal, tokio::signal::unix::Signal)>>> = OnceLock::new();
        let Some(sigs) = SIGNALS.get_or_init(|| match (signal(SignalKind::interrupt()), signal(SignalKind::terminate())) {
            (Ok(i), Ok(t)) => Some(Mutex::new((i, t))),
            _ => None,
        }) else {
            return false;
        };
        let mut g = sigs.lock().await;
        let (int, term) = &mut *g;
        tokio::select! { _ = int.recv() => {}, _ = term.recv() => {} }
        true
    }
    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c().await.is_ok()
    }
}
