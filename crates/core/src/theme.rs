//! Design tokens (design §08 colours, §11 `config.toml` defaults).

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Opaque sRGB colour.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Color {
    pub const fn hex(v: u32) -> Self {
        Self {
            r: (v >> 16) as u8,
            g: (v >> 8) as u8,
            b: v as u8,
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        let h = s.strip_prefix('#')?;
        if h.len() != 6 {
            return None;
        }
        u32::from_str_radix(h, 16).ok().map(Self::hex)
    }
}

impl fmt::Debug for Color {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
    }
}

impl Serialize for Color {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&format!("{self:?}"))
    }
}

impl<'de> Deserialize<'de> for Color {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Color::parse(&s).ok_or_else(|| serde::de::Error::custom(format!("bad colour {s}")))
    }
}

/// `hint.*` colour tokens.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Tokens {
    pub bg: Color,
    pub fg: Color,
    pub fg_dim: Color,
    pub accent: Color,
    pub border: Color,
    pub outline: Color,
    pub vision: Color,
    pub flash_bg: Color,
    pub flash_fg: Color,
    pub drag_bg: Color,
    pub drag_fg: Color,
    pub scroll: Color,
    pub disabled_bg: Color,
    pub disabled_fg: Color,
}

impl Tokens {
    /// Catppuccin Mocha (dark, default).
    pub fn mocha() -> Self {
        Self {
            bg: Color::hex(0x11111b),          // crust
            fg: Color::hex(0xcdd6f4),          // text
            fg_dim: Color::hex(0xa6adc8),      // subtext0
            accent: Color::hex(0xcba6f7),      // mauve
            border: Color::hex(0x6c7086),      // overlay0
            outline: Color::hex(0xb4befe),     // lavender
            vision: Color::hex(0x94e2d5),      // teal
            flash_bg: Color::hex(0xa6e3a1),    // green
            flash_fg: Color::hex(0x11111b),    // crust
            drag_bg: Color::hex(0xfab387),     // peach
            drag_fg: Color::hex(0x11111b),     // crust
            scroll: Color::hex(0x89b4fa),      // blue
            disabled_bg: Color::hex(0x181825), // mantle
            disabled_fg: Color::hex(0x6c7086), // overlay0
        }
    }

    /// Catppuccin Latte (light).
    pub fn latte() -> Self {
        Self {
            bg: Color::hex(0xeff1f5),          // base
            fg: Color::hex(0x4c4f69),          // text
            fg_dim: Color::hex(0x5c5f77),      // subtext1
            accent: Color::hex(0x8839ef),      // mauve
            border: Color::hex(0x7c7f93),      // overlay2
            outline: Color::hex(0x7287fd),     // lavender
            vision: Color::hex(0x179299),      // teal
            flash_bg: Color::hex(0x40a02b),    // green
            flash_fg: Color::hex(0x11111b),    // mocha crust
            drag_bg: Color::hex(0xfe640b),     // peach
            drag_fg: Color::hex(0x11111b),     // mocha crust
            scroll: Color::hex(0x1e66f5),      // blue
            disabled_bg: Color::hex(0xe6e9ef), // mantle
            disabled_fg: Color::hex(0x8c8fa1), // overlay1
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SizeUnit {
    #[default]
    Logical,
    Mm,
}

/// `[hint]` layout defaults.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HintLayout {
    /// Em size in logical px.
    pub size: f32,
    pub min_size: f32,
    pub size_unit: SizeUnit,
    pub size_mm: f32,
    /// `[y, x]` logical px.
    pub padding: [i32; 2],
    pub radius: f32,
    /// Logical border; device border is `max(1, floor(scale))`.
    pub border_width: i32,
    pub offset: [i32; 2],
    pub gap: i32,
    pub center_below: i32,
    pub alphabet: String,
    pub uppercase: bool,
    /// Glyph advance as a fraction of the em size (monospace).
    pub advance_em: f32,
}

impl Default for HintLayout {
    fn default() -> Self {
        Self {
            size: 11.0,
            min_size: 10.0,
            size_unit: SizeUnit::Logical,
            size_mm: 3.0,
            padding: [2, 3],
            radius: 3.0,
            border_width: 1,
            offset: [-3, -3],
            gap: 2,
            center_below: 20,
            alphabet: "asdfghjkl".into(),
            uppercase: false,
            advance_em: 0.6,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Corner {
    #[default]
    BottomRight,
    BottomLeft,
}

/// `[chip]` defaults.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChipLayout {
    pub corner: Corner,
    pub inset: i32,
    pub height: i32,
}

impl Default for ChipLayout {
    fn default() -> Self {
        Self {
            corner: Corner::BottomRight,
            inset: 12,
            height: 22,
        }
    }
}

/// `[timing]` defaults plus the showing watchdog.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TimingLayout {
    pub appear_ms: u32,
    pub fade_ms: u32,
    pub flash_ms: u32,
    pub max_showing_ms: u32,
}

impl Default for TimingLayout {
    fn default() -> Self {
        Self {
            appear_ms: 0,
            fade_ms: 60,
            flash_ms: 80,
            max_showing_ms: 15_000,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Theme {
    pub tokens: Tokens,
    pub hint: HintLayout,
    pub chip: ChipLayout,
    pub timing: TimingLayout,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            tokens: Tokens::mocha(),
            hint: HintLayout::default(),
            chip: ChipLayout::default(),
            timing: TimingLayout::default(),
        }
    }
}

/// Badge font size in device px for one output.
///
/// `phys_width_mm` and `device_width` come from `wl_output.geometry` / mode;
/// in `mm` mode a zero physical width falls back to logical sizing.
pub fn font_device_px(hint: &HintLayout, scale: f32, phys_width_mm: i32, device_width: i32) -> f32 {
    let logical = (hint.size.max(hint.min_size) * scale).round();
    match hint.size_unit {
        SizeUnit::Logical => logical,
        SizeUnit::Mm if phys_width_mm <= 0 || device_width <= 0 => logical,
        SizeUnit::Mm => (hint.size_mm / phys_width_mm as f32 * device_width as f32).round(),
    }
}

/// Factor applied to badge size, padding and radius so they track the font
/// size in `mm` mode; always 1 in logical mode.
pub fn size_factor(hint: &HintLayout, scale: f32, font_px: f32) -> f32 {
    match hint.size_unit {
        SizeUnit::Logical => 1.0,
        SizeUnit::Mm => font_px / (hint.size.max(hint.min_size) * scale).round(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colours() {
        assert_eq!(format!("{:?}", Tokens::mocha().bg), "#11111b");
        assert_eq!(Color::parse("#cdd6f4"), Some(Tokens::mocha().fg));
        assert_eq!(Tokens::latte().accent, Color::hex(0x8839ef));
    }

    #[test]
    fn mm_mode_two_outputs() {
        let h = HintLayout {
            size_unit: SizeUnit::Mm,
            size_mm: 3.0,
            ..Default::default()
        };
        assert_eq!(font_device_px(&h, 1.0, 527, 1920), 11.0);
        assert_eq!(font_device_px(&h, 2.0, 302, 2880), 29.0);
    }

    #[test]
    fn mm_mode_zero_falls_back() {
        let h = HintLayout {
            size_unit: SizeUnit::Mm,
            ..Default::default()
        };
        assert_eq!(font_device_px(&h, 2.0, 0, 2880), 22.0);
    }

    #[test]
    fn logical_font() {
        let h = HintLayout::default();
        assert_eq!(font_device_px(&h, 1.0, 0, 0), 11.0);
        assert_eq!(font_device_px(&h, 1.5, 0, 0), 17.0);
        assert_eq!(font_device_px(&h, 2.0, 0, 0), 22.0);
    }
}
