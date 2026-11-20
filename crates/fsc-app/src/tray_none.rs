//! No tray yet outside Linux (its tray is a StatusNotifierItem over D-Bus).
//! TODO(cross-platform): show `tray_menu` in the macOS menu bar (Qt's
//! SystemTrayIcon, native there) and the Windows notification area.

use tokio::sync::mpsc;

use crate::AppEvent;
use crate::icon::IconState;
pub use crate::tray_menu::{MenuState, TrayUpdate};

/// Drain the updates, so the controller's sends never pile up.
pub async fn run(_: IconState, _: MenuState, mut rx: mpsc::UnboundedReceiver<TrayUpdate>, _: async_channel::Sender<AppEvent>) {
    while rx.recv().await.is_some() {}
}
