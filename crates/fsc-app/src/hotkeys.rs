//! Global keys, where nothing else binds them: macOS and Windows. (On Linux
//! the compositor does: Hyprland's `bind`s run `fsc type` / `fsc listen`.)
//!
//! `[keys]` in the config: `type` opens the popup to type; `talk` listens
//! while held and stops when let go (a tap listens until you stop talking),
//! just as the Linux binds do with `fsc listen` and `fsc release`.

use global_hotkey::hotkey::HotKey;
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};

use fsc_core::config::KeysConfig;
use fsc_core::ipc::Command;

use crate::AppEvent;

/// Keeps the keys registered while it lives.
pub struct Hotkeys {
    _manager: GlobalHotKeyManager,
}

/// Register the keys. Call on the main thread, before its event loop runs
/// (macOS delivers them through it). A key another app already has is
/// skipped and logged.
pub fn start(keys: &KeysConfig, tx: async_channel::Sender<AppEvent>) -> Option<Hotkeys> {
    let manager = match GlobalHotKeyManager::new() {
        Ok(m) => m,
        Err(e) => {
            eprintln!("fsc: global keys unavailable: {e}");
            return None;
        }
    };
    let register = |what: &str, spec: &str| -> Option<u32> {
        let spec = spec.trim();
        if spec.is_empty() {
            return None;
        }
        let key: HotKey = match spec.parse() {
            Ok(k) => k,
            Err(e) => {
                eprintln!("fsc: [keys] {what} = {spec:?} isn't a key: {e}");
                return None;
            }
        };
        match manager.register(key) {
            Ok(()) => {
                eprintln!("fsc: {spec} to {what}");
                Some(key.id())
            }
            Err(e) => {
                eprintln!("fsc: {spec} is taken (another app may use it; change [keys] {what}): {e}");
                None
            }
        }
    };
    let (type_id, talk_id) = (register("type", &keys.type_key), register("talk", &keys.talk));
    GlobalHotKeyEvent::set_event_handler(Some(move |e: GlobalHotKeyEvent| {
        let cmd = match e.state {
            HotKeyState::Pressed if Some(e.id) == type_id => Command::Type,
            HotKeyState::Pressed if Some(e.id) == talk_id => Command::Listen,
            HotKeyState::Released if Some(e.id) == talk_id => Command::Release,
            _ => return,
        };
        let _ = tx.try_send(AppEvent::Command(cmd, None));
    }));
    Some(Hotkeys { _manager: manager })
}
