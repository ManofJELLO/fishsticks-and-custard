//! Follow the desktop's light/dark preference through the XDG settings
//! portal (`org.freedesktop.appearance color-scheme`). On Hyprland this is
//! served by xdg-desktop-portal-gtk from
//! `gsettings org.gnome.desktop.interface color-scheme`, so it changes live
//! when you flip it with nwg-look or gsettings.

use ashpd::desktop::settings::{ColorScheme, Settings};
use futures_util::StreamExt;

use crate::AppEvent;
use crate::style::{Rgb, ThemeMode};

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

/// The desktop's accent colour (`org.freedesktop.appearance accent-color`),
/// live. Sends `None` when the desktop doesn't set one, so the Qt palette's
/// own accent is used.
pub async fn watch_accent(tx: async_channel::Sender<AppEvent>) {
    let Ok(settings) = Settings::new().await else { return };
    let rgb = |c: ashpd::desktop::Color| {
        let v = (c.red(), c.green(), c.blue());
        // The spec: out-of-range values mean "no accent set".
        let ok = [v.0, v.1, v.2].iter().all(|x| (0.0..=1.0).contains(x));
        ok.then_some(Rgb(v.0, v.1, v.2))
    };
    let first = settings.accent_color().await.ok().and_then(rgb);
    let _ = tx.send(AppEvent::Accent(first)).await;
    if let Ok(mut stream) = settings.receive_accent_color_changed().await {
        while let Some(c) = stream.next().await {
            if tx.send(AppEvent::Accent(rgb(c))).await.is_err() {
                break;
            }
        }
    }
}
