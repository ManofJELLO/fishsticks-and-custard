//! Where fscd keeps things that aren't settings or models, per platform.

use std::path::PathBuf;

/// The directory for state that should outlive a restart but isn't worth
/// backing up, like the log: `$XDG_STATE_HOME/fsc` (`~/.local/state/fsc`)
/// on Linux, the local app-data folder elsewhere.
pub fn state_dir() -> Option<PathBuf> {
    if cfg!(target_os = "linux") {
        let state = std::env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))?;
        return Some(state.join("fsc"));
    }
    directories::BaseDirs::new().map(|d| d.data_local_dir().join("fsc").join("state"))
}
