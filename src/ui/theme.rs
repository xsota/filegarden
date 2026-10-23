//! Colors, glyphs and the fade curve, adapted to what the terminal can show.

use ratatui::style::{Color, Modifier, Style};
use ratatui::symbols::border;

use crate::change::ChangeKind;

/// How long the glow of a change lasts (seconds).
pub const GLOW: f64 = 1.8;
/// The initial white-hot flash that marks the exact moment of change.
pub const FLASH: f64 = 0.14;
/// With animations off, changes keep their color this long, then switch off.
pub const HOLD: f64 = 3.0;
/// The change marker (`+ ~ - →`) stays next to a tree entry this long.
pub const MARKER_TTL: f64 = 10.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorMode {
    TrueColor,
    Ansi256,
    Ansi16,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    /// `t = 1` gives `self`, `t = 0` gives `other`.
    pub fn mix(self, other: Rgb, t: f64) -> Rgb {
        let t = t.clamp(0.0, 1.0);
        let m = |a: u8, b: u8| (a as f64 * t + b as f64 * (1.0 - t)).round() as u8;
        Rgb(m(self.0, other.0), m(self.1, other.1), m(self.2, other.2))
    }
}

pub mod palette {
    use super::Rgb;
    pub const CREATE: Rgb = Rgb(92, 230, 128);
    pub const MODIFY: Rgb = Rgb(250, 208, 84);
    pub const DELETE: Rgb = Rgb(255, 92, 92);
    pub const RENAME: Rgb = Rgb(92, 168, 255);
    pub const FLASH: Rgb = Rgb(255, 255, 240);
    pub const TEXT: Rgb = Rgb(196, 202, 212);
    pub const FILE: Rgb = Rgb(160, 166, 178);
    pub const DIR: Rgb = Rgb(138, 180, 248);
    pub const DIM: Rgb = Rgb(98, 104, 118);
    pub const FAINT: Rgb = Rgb(58, 63, 74);
    pub const BORDER: Rgb = Rgb(70, 76, 90);
    pub const ACCENT: Rgb = Rgb(126, 214, 146);
    pub const CELL_IDLE: Rgb = Rgb(64, 72, 86);
    pub const CELL_WARM: Rgb = Rgb(104, 112, 128);
}

#[derive(Debug, Clone, Copy)]
pub struct Theme {
    pub colors: ColorMode,
    pub ascii: bool,
    pub animate: bool,
}

impl Theme {
    pub fn detect(no_color: bool, ascii: bool, animate: bool, forced: Option<ColorMode>) -> Self {
        let colors = if no_color || std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty()) {
            ColorMode::None
        } else {
            forced.unwrap_or_else(detect_colors)
        };
        Theme { colors, ascii: ascii || !unicode_supported(), animate }
    }

    pub fn kind_rgb(kind: ChangeKind) -> Rgb {
        match kind {
            ChangeKind::Create => palette::CREATE,
            ChangeKind::Modify => palette::MODIFY,
            ChangeKind::Delete => palette::DELETE,
            ChangeKind::Rename => palette::RENAME,
        }
    }

    pub fn color(&self, c: Rgb) -> Color {
        match self.colors {
            ColorMode::TrueColor => Color::Rgb(c.0, c.1, c.2),
            ColorMode::Ansi256 => Color::Indexed(rgb_to_256(c)),
            ColorMode::Ansi16 => rgb_to_16(c),
            ColorMode::None => Color::Reset,
        }
    }

    pub fn fg(&self, c: Rgb) -> Style {
        Style::default().fg(self.color(c))
    }

    /// Glow intensity for something that happened `age` seconds ago: 1 → 0.
    pub fn glow(&self, age: f64) -> f64 {
        if age < 0.0 {
            return 1.0;
        }
        if !self.animate {
            return if age < HOLD { 1.0 } else { 0.0 };
        }
        let t = (1.0 - age / GLOW).clamp(0.0, 1.0);
        // Ease out: bright for a moment, then a long soft tail.
        t * t * (3.0 - 2.0 * t) * 0.35 + t * t * 0.65
    }

    /// Color of something that changed `age` seconds ago, fading back to `base`.
    pub fn heat(&self, kind: ChangeKind, base: Rgb, age: f64) -> Rgb {
        let kc = Self::kind_rgb(kind);
        if self.animate && (0.0..FLASH).contains(&age) {
            return palette::FLASH.mix(kc, 1.0 - age / FLASH);
        }
        kc.mix(base, self.glow(age))
    }

    /// Style for a glowing element: colored, and bold while hot (helps without colors).
    pub fn heat_style(&self, kind: ChangeKind, base: Rgb, age: f64) -> Style {
        let mut s = self.fg(self.heat(kind, base, age));
        if self.glow(age) > 0.45 {
            s = s.add_modifier(Modifier::BOLD);
        }
        s
    }

    pub fn border_set(&self) -> border::Set {
        if self.ascii {
            border::Set {
                top_left: "+",
                top_right: "+",
                bottom_left: "+",
                bottom_right: "+",
                vertical_left: "|",
                vertical_right: "|",
                horizontal_top: "-",
                horizontal_bottom: "-",
            }
        } else {
            border::ROUNDED
        }
    }

    pub fn sym<'a>(&self, unicode: &'a str, ascii: &'a str) -> &'a str {
        if self.ascii {
            ascii
        } else {
            unicode
        }
    }
}

fn detect_colors() -> ColorMode {
    let colorterm = std::env::var("COLORTERM").unwrap_or_default().to_lowercase();
    if colorterm.contains("truecolor") || colorterm.contains("24bit") {
        return ColorMode::TrueColor;
    }
    if cfg!(windows) || std::env::var_os("WT_SESSION").is_some() {
        // Windows 10+ consoles and Windows Terminal handle 24-bit color.
        return ColorMode::TrueColor;
    }
    let term = std::env::var("TERM").unwrap_or_default();
    if term == "dumb" {
        ColorMode::None
    } else if term.contains("256") {
        ColorMode::Ansi256
    } else {
        ColorMode::Ansi16
    }
}

fn unicode_supported() -> bool {
    if cfg!(windows) {
        return true;
    }
    for var in ["LC_ALL", "LC_CTYPE", "LANG"] {
        if let Ok(v) = std::env::var(var) {
            if !v.is_empty() {
                let v = v.to_lowercase();
                return v.contains("utf-8") || v.contains("utf8");
            }
        }
    }
    true
}

pub fn rgb_to_256(c: Rgb) -> u8 {
    let Rgb(r, g, b) = c;
    // Grayscale ramp for near-neutral colors.
    let (mx, mn) = (r.max(g).max(b), r.min(g).min(b));
    if mx - mn < 12 {
        let avg = (r as u16 + g as u16 + b as u16) / 3;
        if avg < 8 {
            return 16;
        }
        if avg > 238 {
            return 231;
        }
        return 232 + ((avg - 8) * 24 / 231).min(23) as u8;
    }
    let q = |v: u8| -> u8 {
        if v < 48 {
            0
        } else if v < 115 {
            1
        } else {
            ((v as u16 - 35) / 40) as u8
        }
    };
    16 + 36 * q(r) + 6 * q(g) + q(b)
}

pub fn rgb_to_16(c: Rgb) -> Color {
    const TABLE: [(Rgb, Color); 16] = [
        (Rgb(0, 0, 0), Color::Black),
        (Rgb(170, 0, 0), Color::Red),
        (Rgb(0, 170, 0), Color::Green),
        (Rgb(170, 140, 0), Color::Yellow),
        (Rgb(30, 80, 200), Color::Blue),
        (Rgb(170, 0, 170), Color::Magenta),
        (Rgb(0, 170, 170), Color::Cyan),
        (Rgb(170, 170, 170), Color::Gray),
        (Rgb(85, 85, 85), Color::DarkGray),
        (Rgb(255, 85, 85), Color::LightRed),
        (Rgb(85, 255, 85), Color::LightGreen),
        (Rgb(255, 230, 85), Color::LightYellow),
        (Rgb(85, 150, 255), Color::LightBlue),
        (Rgb(255, 85, 255), Color::LightMagenta),
        (Rgb(85, 255, 255), Color::LightCyan),
        (Rgb(255, 255, 255), Color::White),
    ];
    let d = |a: Rgb| {
        let dr = a.0 as i32 - c.0 as i32;
        let dg = a.1 as i32 - c.1 as i32;
        let db = a.2 as i32 - c.2 as i32;
        dr * dr * 3 + dg * dg * 4 + db * db * 2
    };
    TABLE.iter().min_by_key(|(rgb, _)| d(*rgb)).map(|(_, col)| *col).unwrap_or(Color::Reset)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn theme(animate: bool) -> Theme {
        Theme { colors: ColorMode::TrueColor, ascii: false, animate }
    }

    #[test]
    fn glow_decays_monotonically_to_zero() {
        let t = theme(true);
        let mut prev = 1.0;
        for i in 0..=40 {
            let g = t.glow(i as f64 * GLOW / 40.0);
            assert!(g <= prev + 1e-9);
            prev = g;
        }
        assert_eq!(t.glow(GLOW), 0.0);
        assert_eq!(t.glow(0.0), 1.0);
    }

    #[test]
    fn no_animation_holds_then_stops() {
        let t = theme(false);
        assert_eq!(t.glow(HOLD - 0.1), 1.0);
        assert_eq!(t.glow(HOLD), 0.0);
        assert_eq!(t.heat(ChangeKind::Create, palette::FILE, 0.01), palette::CREATE);
    }

    #[test]
    fn heat_returns_to_base() {
        let t = theme(true);
        assert_eq!(t.heat(ChangeKind::Delete, palette::FILE, 10.0), palette::FILE);
        assert_eq!(t.heat(ChangeKind::Delete, palette::FILE, 0.0), palette::FLASH);
    }

    #[test]
    fn palette_conversion() {
        assert_eq!(rgb_to_256(Rgb(0, 0, 0)), 16);
        assert_eq!(rgb_to_256(Rgb(255, 0, 0)), 196);
        assert_eq!(rgb_to_16(palette::DELETE), Color::LightRed);
        assert_eq!(rgb_to_16(palette::CREATE), Color::LightGreen);
    }
}
