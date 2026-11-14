//! Colours and stylesheet. Foreground, background and accent come from the
//! GTK theme and the desktop; the built-in monochrome palette is only the
//! fallback. The accent is used only for state.

use gtk::prelude::*;

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

    pub fn css(self, a: f64) -> String {
        format!(
            "rgba({}, {}, {}, {a:.3})",
            (self.0 * 255.0).round() as u8,
            (self.1 * 255.0).round() as u8,
            (self.2 * 255.0).round() as u8
        )
    }

    pub fn bytes(self) -> [u8; 3] {
        [(self.0 * 255.0) as u8, (self.1 * 255.0) as u8, (self.2 * 255.0) as u8]
    }
}

pub const NOTHING_RED: Rgb = Rgb(0xD7 as f64 / 255.0, 0x19 as f64 / 255.0, 0x21 as f64 / 255.0);

impl Rgb {
    fn from_rgba(c: gtk::gdk::RGBA) -> Rgb {
        Rgb(c.red() as f64, c.green() as f64, c.blue() as f64)
    }

    /// Relative luminance, 0 (black) to 1 (white).
    pub fn luma(self) -> f64 {
        0.2126 * self.0 + 0.7152 * self.1 + 0.0722 * self.2
    }
}

/// The first of `names` the current GTK theme defines (`@define-color`).
#[allow(deprecated)] // lookup_color has no replacement until GTK 5
fn theme_color(w: &gtk::Widget, names: &[&str]) -> Option<Rgb> {
    let ctx = w.style_context();
    names.iter().find_map(|n| ctx.lookup_color(n)).map(Rgb::from_rgba)
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

    /// Colours from the GTK theme `w` is styled with. `accent`: the desktop's
    /// or the configured accent; `None` falls back to the theme's.
    /// `fallback` is used for anything the theme doesn't define.
    pub fn from_theme(w: &gtk::Widget, accent: Option<Rgb>, fallback: ThemeMode) -> Palette {
        let base = Palette::new(fallback, accent.unwrap_or(NOTHING_RED));
        let bg = theme_color(w, &["window_bg_color", "theme_bg_color"]);
        let fg = theme_color(w, &["window_fg_color", "theme_fg_color"]);
        let (bg, fg) = match (bg, fg) {
            (Some(bg), Some(fg)) => (bg, fg),
            // Half a theme would risk unreadable text: all or nothing.
            _ => (base.bg, base.fg),
        };
        let accent = accent
            .or_else(|| theme_color(w, &["accent_bg_color", "accent_color", "theme_selected_bg_color"]))
            .unwrap_or(base.accent);
        let error = theme_color(w, &["error_color", "error_bg_color"]).unwrap_or(base.error);
        let mode = if bg.luma() < 0.5 { ThemeMode::Dark } else { ThemeMode::Light };
        Palette { mode, fg, bg, accent, error }
    }

    pub fn css(&self, font: &str) -> String {
        let fg = |a: f64| self.fg.css(a);
        let accent = |a: f64| self.accent.css(a);
        format!(
            r#"
window.fsc {{
    background: transparent;
    font-family: "{font}", monospace;
}}
window.fsc * {{
    font-family: "{font}", monospace;
}}
.fsc .meta {{
    color: {meta};
    font-size: 10px;
    letter-spacing: 2px;
    text-shadow: 0 0 3px {halo};
}}
.fsc .prompt {{
    color: {dim};
    font-size: 13px;
}}
.fsc .reply {{
    color: {text};
    font-size: 15px;
}}
.fsc .tool {{
    color: {dim};
    font-size: 11px;
    letter-spacing: 1px;
}}
.fsc .note {{
    color: {dim};
    font-size: 11px;
    letter-spacing: 1px;
}}
.fsc .error {{
    color: {error};
    font-size: 12px;
}}
.fsc .ask {{
    border-left: 2px solid {red};
    padding: 2px 0 2px 12px;
}}
.fsc .ask.answered {{
    border-left-color: {faint};
}}
.fsc .ask-action {{
    color: {text};
    font-size: 14px;
}}
.fsc .ask-detail {{
    color: {dim};
    font-size: 12px;
    background-color: {field};
    border-radius: 8px;
    padding: 6px 10px;
}}
.fsc button.ask-button {{
    background-color: transparent;
    background-image: none;
    color: {text};
    border: 1px solid {border};
    border-radius: 999px;
    padding: 2px 16px;
    min-height: 24px;
    font-size: 11px;
    letter-spacing: 2px;
    box-shadow: none;
    text-shadow: none;
    outline: none;
}}
.fsc button.ask-button:hover {{
    border-color: {border_focus};
    background-color: {field};
}}
.fsc button.ask-button.allow {{
    background-color: {red};
    border-color: {red};
    color: {on_accent};
}}
.fsc button.ask-button.allow:hover {{
    background-color: {red_hover};
}}
.fsc label selection {{
    background-color: {sel};
    color: {text};
}}
.fsc scrolledwindow, .fsc viewport {{
    background: transparent;
}}
.fsc scrollbar {{
    background: transparent;
    border: none;
}}
.fsc scrollbar slider {{
    background-color: {faint};
    min-width: 3px;
    border-radius: 99px;
}}
.fsc .divider {{
    background-color: {faint};
    min-height: 1px;
}}
.fsc entry {{
    background-color: {field};
    background-image: none;
    color: {text};
    caret-color: {red};
    border: 1px solid {border};
    border-radius: 999px;
    padding: 8px 18px;
    min-height: 26px;
    box-shadow: none;
    outline: none;
    font-size: 14px;
}}
.fsc entry:focus-within,
.fsc entry:hover,
.fsc entry:hover:not(:focus-within) {{
    background-color: {field};
    box-shadow: none;
    outline: 0 solid transparent;
    outline-offset: 0;
}}
.fsc entry:focus-within {{
    border-color: {border_focus};
}}
.fsc entry > text > placeholder {{
    color: {placeholder};
}}
.fsc entry > text > selection {{
    background-color: {sel};
}}
"#,
            meta = fg(0.72),
            halo = self.bg.css(0.85),
            dim = fg(0.55),
            text = fg(0.95),
            red = accent(1.0),
            red_hover = accent(0.85),
            error = self.error.css(1.0),
            on_accent = if self.accent.luma() > 0.6 { "#111111" } else { "#ffffff" },
            sel = accent(0.35),
            faint = fg(0.14),
            field = fg(0.04),
            border = fg(0.22),
            border_focus = fg(0.55),
            placeholder = fg(0.35),
        )
    }
}
