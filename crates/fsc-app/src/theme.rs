//! Follow the desktop's light/dark preference through the XDG settings
//! portal (`org.freedesktop.appearance color-scheme`). On Hyprland this is
//! served by xdg-desktop-portal-gtk from
//! `gsettings org.gnome.desktop.interface color-scheme`, so it changes live
//! when you flip it with nwg-look or gsettings.

use ashpd::desktop::settings::{ColorScheme, Settings};
use futures_util::StreamExt;

use crate::AppEvent;
use crate::style::ThemeMode;

fn map(cs: ColorScheme) -> ThemeMode {
    match cs {
        ColorScheme::PreferDark => ThemeMode::Dark,
        // The spec says "no preference" means light.
        ColorScheme::PreferLight | ColorScheme::NoPreference => ThemeMode::Light,
    }
}

pub async fn watch(tx: async_channel::Sender<AppEvent>) {
    let settings = match Settings::new().await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("fsc: settings portal unavailable ({e}); staying dark");
            return;
        }
    };
    match settings.color_scheme().await {
        Ok(cs) => {
            let _ = tx.send(AppEvent::Theme(map(cs))).await;
        }
        Err(e) => eprintln!("fsc: could not read color-scheme: {e}"),
    }
    match settings.receive_color_scheme_changed().await {
        Ok(mut stream) => {
            while let Some(cs) = stream.next().await {
                if tx.send(AppEvent::Theme(map(cs))).await.is_err() {
                    break;
                }
            }
        }
        Err(e) => eprintln!("fsc: could not watch color-scheme: {e}"),
    }
}
