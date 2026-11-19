//! Colours. Foreground, background and accent come from the desktop's Qt
//! palette (qt6ct on Hyprland, the system's on Windows and macOS) and its
//! accent; the built-in monochrome palette is only the fallback. The accent
//! is used only for state.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemeMode {
    Dark,
    Light,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rgb(pub f64, pub f64, pub f64);

impl Rgb {
    pub fn from_hex(s: &str) -> Option<Rgb> {
        let s = s.trim().trim_start_matches('#');
        if s.len() != 6 {
            return None;
        }
        let n = u32::from_str_radix(s, 16).ok()?;
        Some(Rgb(
            ((n >> 16) & 0xff) as f64 / 255.0,
            ((n >> 8) & 0xff) as f64 / 255.0,
            (n & 0xff) as f64 / 255.0,
        ))
    }

    pub fn bytes(self) -> [u8; 3] {
        [(self.0 * 255.0) as u8, (self.1 * 255.0) as u8, (self.2 * 255.0) as u8]
    }
}

pub const NOTHING_RED: Rgb = Rgb(0xD7 as f64 / 255.0, 0x19 as f64 / 255.0, 0x21 as f64 / 255.0);

impl Rgb {
    /// Relative luminance, 0 (black) to 1 (white).
    pub fn luma(self) -> f64 {
        0.2126 * self.0 + 0.7152 * self.1 + 0.0722 * self.2
    }

    /// `#rrggbb`.
    pub fn hex(self) -> String {
        let [r, g, b] = self.bytes();
        format!("#{r:02x}{g:02x}{b:02x}")
    }
}

/// The desktop's colours, as its toolkit palette has them.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct SystemColors {
    pub bg: Option<Rgb>,
    pub fg: Option<Rgb>,
    pub accent: Option<Rgb>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Palette {
    pub mode: ThemeMode,
    /// Text, lit dots, icon.
    pub fg: Rgb,
    /// Panel tint (opacity is decided per region by the panel).
    pub bg: Rgb,
    pub accent: Rgb,
    pub error: Rgb,
}

impl Palette {
    pub fn new(mode: ThemeMode, accent: Rgb) -> Self {
        match mode {
            ThemeMode::Dark => Palette {
                mode,
                fg: Rgb(0.96, 0.96, 0.96),
                bg: Rgb(0.03, 0.03, 0.03),
                accent,
                error: NOTHING_RED,
            },
            ThemeMode::Light => Palette {
                mode,
                fg: Rgb(0.05, 0.05, 0.05),
                bg: Rgb(0.95, 0.95, 0.94),
                accent,
                error: NOTHING_RED,
            },
        }
    }

    /// Colours from the desktop's palette. `accent`: the desktop's or the
    /// configured accent; `None` falls back to the palette's own.
    /// `fallback` is used for anything the palette doesn't give.
    pub fn from_system(sys: SystemColors, accent: Option<Rgb>, fallback: ThemeMode) -> Palette {
        let base = Palette::new(fallback, accent.unwrap_or(NOTHING_RED));
        let (bg, fg) = match (sys.bg, sys.fg) {
            (Some(bg), Some(fg)) => (bg, fg),
            // Half a theme would risk unreadable text: all or nothing.
            _ => (base.bg, base.fg),
        };
        let accent = accent.or(sys.accent).unwrap_or(base.accent);
        // No toolkit palette has an error colour.
        let error = base.error;
        let mode = if bg.luma() < 0.5 { ThemeMode::Dark } else { ThemeMode::Light };
        Palette { mode, fg, bg, accent, error }
    }

    /// What goes on the accent: dark text on a light accent, white otherwise.
    pub fn on_accent(&self) -> Rgb {
        if self.accent.luma() > 0.6 { Rgb(0.067, 0.067, 0.067) } else { Rgb(1.0, 1.0, 1.0) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palette_from_system() {
        let sys = SystemColors { bg: Some(Rgb(1.0, 1.0, 1.0)), fg: Some(Rgb(0.0, 0.0, 0.0)), accent: Some(Rgb(0.0, 0.0, 1.0)) };
        let p = Palette::from_system(sys, None, ThemeMode::Dark);
        assert_eq!(p.mode, ThemeMode::Light);
        assert_eq!(p.accent, Rgb(0.0, 0.0, 1.0));
        // A pinned or desktop accent wins over the palette's.
        assert_eq!(Palette::from_system(sys, Some(NOTHING_RED), ThemeMode::Dark).accent, NOTHING_RED);
        // Half a palette: the fallback's colours, all of them.
        let half = SystemColors { bg: Some(Rgb(1.0, 1.0, 1.0)), ..Default::default() };
        let p = Palette::from_system(half, None, ThemeMode::Dark);
        assert_eq!((p.bg, p.mode), (Palette::new(ThemeMode::Dark, NOTHING_RED).bg, ThemeMode::Dark));
        assert_eq!(Rgb(0.0, 0.5, 1.0).hex(), "#007fff");
    }
}
