//! Environment variables fscd sets for its own toolkit (Qt's platform and
//! logging rules). The programs it starts (the agent, the settings app, an
//! editor, notify-send) get the environment as it was before, so a Qt app
//! opened from the tray looks and logs as it would anywhere else.

use std::ffi::{OsStr, OsString};
use std::sync::Mutex;

/// Each variable fscd set, with the value it had before (`None`: unset).
static SAVED: Mutex<Vec<(&'static str, Option<OsString>)>> = Mutex::new(Vec::new());

/// Set `name` for fscd itself, remembering what it was for child programs.
///
/// # Safety
/// As for `std::env::set_var`: no other thread may be reading or writing
/// the environment, so call it before any threads start.
pub unsafe fn set(name: &'static str, value: impl AsRef<OsStr>) {
    let before = std::env::var_os(name);
    let mut saved = SAVED.lock().unwrap_or_else(|e| e.into_inner());
    if !saved.iter().any(|(n, _)| *n == name) {
        saved.push((name, before));
    }
    // SAFETY: the caller's promise.
    unsafe { std::env::set_var(name, value) };
}

/// Give `cmd` the environment as it was before fscd's own changes. Call it
/// before setting any variables the child should have, since it overrides them.
pub fn restore(cmd: &mut std::process::Command) {
    let saved = SAVED.lock().unwrap_or_else(|e| e.into_inner());
    for (name, before) in saved.iter() {
        match before {
            Some(v) => cmd.env(name, v),
            None => cmd.env_remove(name),
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A child sees each variable as it was, not as fscd set it. (The
    /// environment itself is left alone: other tests run alongside.)
    #[test]
    fn child_gets_the_old_value() {
        SAVED.lock().unwrap().extend([("FSC_TEST_UNSET", None), ("FSC_TEST_SET", Some("user's".into()))]);
        let mut cmd = std::process::Command::new("true");
        restore(&mut cmd);
        let mut got: Vec<_> = cmd.get_envs().filter(|(k, _)| k.to_string_lossy().starts_with("FSC_TEST_")).collect();
        got.sort();
        assert_eq!(
            got,
            vec![(OsStr::new("FSC_TEST_SET"), Some(OsStr::new("user's"))), (OsStr::new("FSC_TEST_UNSET"), None)]
        );
    }
}
