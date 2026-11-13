//! Colours and stylesheet. Monochrome with one accent colour, used only
//! for state.

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

#[derive(Debug, Clone, Copy)]
pub struct Palette {
    pub mode: ThemeMode,
    /// Text, lit dots, icon.
    pub fg: Rgb,
    /// Panel tint (opacity is decided per region by the panel).
    pub bg: Rgb,
    pub accent: Rgb,
}

impl Palette {
    pub fn new(mode: ThemeMode, accent: Rgb) -> Self {
        match mode {
            ThemeMode::Dark => Palette {
                mode,
                fg: Rgb(0.96, 0.96, 0.96),
                bg: Rgb(0.03, 0.03, 0.03),
                accent,
            },
            ThemeMode::Light => Palette {
                mode,
                fg: Rgb(0.05, 0.05, 0.05),
                bg: Rgb(0.95, 0.95, 0.94),
                accent,
            },
        }
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
    color: {red};
    font-size: 12px;
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
            sel = accent(0.35),
            faint = fg(0.14),
            field = fg(0.04),
            border = fg(0.22),
            border_focus = fg(0.55),
            placeholder = fg(0.35),
        )
    }
}
