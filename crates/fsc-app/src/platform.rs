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
        let mut cmd = tokio::process::Command::new("notify-send");
        fsc_core::own_env::restore(cmd.as_std_mut());
        let _ = cmd.args(["--app-name=Fishsticks & Custard", "Fishsticks & Custard", &body]).status().await;
    });
    // macOS: through AppleScript, the text passed as an argument so nothing
    // in it is read as script. (Shown as from Script Editor until the app is
    // signed and can post its own.)
    #[cfg(target_os = "macos")]
    tokio::spawn(async move {
        let script = ["on run argv", "display notification (item 1 of argv) with title \"Fishsticks & Custard\"", "end run"];
        let mut cmd = tokio::process::Command::new("/usr/bin/osascript");
        for line in script {
            cmd.args(["-e", line]);
        }
        let _ = cmd.arg("--").arg(&body).status().await;
    });
    // TODO(windows): a toast.
    #[cfg(windows)]
    eprintln!("fsc: {body}");
}

/// Start `prog` with `args`, detached: its own process group, so it
/// outlives fscd and isn't sent fscd's signals. Reaped in the background.
pub fn spawn_detached(prog: &str, args: &[String]) -> std::io::Result<()> {
    let mut cmd = std::process::Command::new(prog);
    // Without fscd's own Qt settings: HyprCustard, for one, is a Qt app.
    fsc_core::own_env::restore(&mut cmd);
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

/// The program and arguments that open the text file `path` to edit: the
/// default text editor on macOS (a `.toml` may have no app of its own),
/// Notepad on Windows, the desktop's choice on Linux.
pub fn text_opener(path: &Path) -> (&'static str, Vec<String>) {
    let p = path.display().to_string();
    if cfg!(target_os = "macos") {
        ("open", vec!["-t".into(), p])
    } else if cfg!(windows) {
        ("notepad", vec![p])
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

/// macOS: an app started from Finder, the Dock or at login gets launchd's
/// bare PATH (/usr/bin:/bin:/usr/sbin:/sbin): no Homebrew, no ~/.local/bin,
/// nothing the shell's profile adds. Then `claude` and `codex`, and the
/// `node` and `git` they run, aren't found. So take the login shell's PATH
/// (as an interactive login shell has it, where nvm and the like set it up),
/// once, at startup. Says what it did, for the log.
///
/// Must be called before any other thread exists: it sets PATH.
#[cfg(target_os = "macos")]
pub fn adopt_login_path() -> String {
    let current = std::env::var("PATH").unwrap_or_default();
    let home = std::path::PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
    let login = login_shell_path();
    let merged = merge_path(login.as_deref().ok(), &current, &home, |p| p.is_dir());
    // SAFETY: the caller guarantees no other threads exist yet. (If the
    // shell timed out, its output reader may still be waiting on the pipe;
    // it only reads that, never the environment.)
    unsafe { std::env::set_var("PATH", &merged) };
    match login {
        Ok(_) => format!("PATH from the login shell: {merged}"),
        Err(e) => format!("PATH: the login shell's isn't known ({e}); using {merged}"),
    }
}

/// The PATH an interactive login shell (`$SHELL`, else zsh) ends up with.
/// Read from `env` between markers, since a profile may print things, and
/// fish keeps PATH as a list that only `env` shows joined.
#[cfg(target_os = "macos")]
fn login_shell_path() -> Result<String, String> {
    use std::io::Read;
    use std::time::{Duration, Instant};
    const MARK: &str = "_FSC_ENV_";
    let shell = std::env::var("SHELL").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "/bin/zsh".into());
    let mut child = std::process::Command::new(&shell)
        .args(["-i", "-l", "-c", &format!("echo {MARK}; /usr/bin/env; echo {MARK}")])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("{shell}: {e}"))?;
    // Read as it's written: a chatty profile could fill the pipe, and the
    // shell would wait for room that's never made.
    let reader = child.stdout.take().map(|mut s| {
        std::thread::spawn(move || {
            let mut out = String::new();
            let _ = s.read_to_string(&mut out);
            out
        })
    });
    // A profile that waits for input or hangs mustn't stop fscd starting.
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if start.elapsed() < Duration::from_secs(5) => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                // Not joined: something the shell started could keep the
                // pipe open. That reader touches nothing else.
                return Err(format!("{shell} took too long"));
            }
        }
    }
    let out = reader.and_then(|r| r.join().ok()).unwrap_or_default();
    let mut parts = out.split(MARK);
    parts.next();
    let env = parts.next().ok_or_else(|| format!("{shell} printed no environment"))?;
    env.lines()
        .find_map(|l| l.strip_prefix("PATH="))
        .map(str::to_string)
        .ok_or_else(|| format!("{shell} has no PATH"))
}

/// The login shell's PATH (if known), then what fscd was given, then the
/// usual homes of Homebrew and user-installed CLIs that exist (`is_dir`),
/// each once, in that order.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn merge_path(login: Option<&str>, current: &str, home: &Path, is_dir: impl Fn(&Path) -> bool) -> String {
    let usual = ["/opt/homebrew/bin", "/opt/homebrew/sbin", "/usr/local/bin"]
        .into_iter()
        .map(std::path::PathBuf::from)
        .chain([".local/bin", ".cargo/bin", ".bun/bin", ".npm-global/bin"].into_iter().map(|d| home.join(d)))
        .filter(|d| is_dir(d))
        .map(|d| d.display().to_string());
    let mut out: Vec<String> = Vec::new();
    let given = login.into_iter().chain([current]).flat_map(|p| p.split(':')).map(str::to_string);
    for dir in given.chain(usual) {
        if !dir.is_empty() && !out.contains(&dir) {
            out.push(dir);
        }
    }
    out.join(":")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// PATH as macOS has it: Unix paths, ':' between them.
    #[cfg(unix)]
    #[test]
    fn path_merges_login_given_and_usual_once() {
        let home = Path::new("/Users/u");
        let exists = |p: &Path| p == Path::new("/opt/homebrew/bin") || p == Path::new("/Users/u/.local/bin");
        let merged = merge_path(Some("/Users/u/.local/bin:/opt/homebrew/bin:/usr/bin"), "/usr/bin:/bin", home, exists);
        assert_eq!(merged, "/Users/u/.local/bin:/opt/homebrew/bin:/usr/bin:/bin");
        // Without the login shell's, the usual places that exist are added.
        assert_eq!(merge_path(None, "/usr/bin:/bin", home, exists), "/usr/bin:/bin:/opt/homebrew/bin:/Users/u/.local/bin");
    }
}
