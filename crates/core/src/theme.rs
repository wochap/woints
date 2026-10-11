//! Design tokens (design §08 colours, §11 `config.toml` defaults).

use std::fmt;
use std::path::PathBuf;

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

    /// `#rrggbb` or `#rrggbbaa`; badges are opaque, so alpha is accepted and dropped.
    pub fn parse(s: &str) -> Option<Self> {
        let h = s.strip_prefix('#')?;
        if !(h.len() == 6 || h.len() == 8) || !h.chars().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        u32::from_str_radix(&h[..6], 16).ok().map(Self::hex)
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
#[serde(deny_unknown_fields)]
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
    pub chip_bg: Color,
    pub chip_fg: Color,
    pub chip_border: Color,
    pub chip_accent: Color,
}

impl Tokens {
    /// Catppuccin Mocha based palette for dark colour schemes.
    pub fn dark() -> Self {
        Self {
            bg: Color::hex(0xa6e3a1),
            fg: Color::hex(0x181825),
            fg_dim: Color::hex(0x585b70),
            accent: Color::hex(0xb4befe),
            border: Color::hex(0x181825),
            outline: Color::hex(0xb4befe),
            vision: Color::hex(0x94e2d5),
            flash_bg: Color::hex(0xfab387),
            flash_fg: Color::hex(0x181825),
            drag_bg: Color::hex(0xfab387),
            drag_fg: Color::hex(0x181825),
            scroll: Color::hex(0x89b4fa),
            disabled_bg: Color::hex(0x313244),
            disabled_fg: Color::hex(0x7f849c),
            chip_bg: Color::hex(0x1e1e2e),
            chip_fg: Color::hex(0xcdd6f4),
            chip_border: Color::hex(0xcdd6f4),
            chip_accent: Color::hex(0xb4befe),
        }
    }

    /// Catppuccin Latte based palette for light colour schemes.
    pub fn light() -> Self {
        Self {
            bg: Color::hex(0x40a02b),
            fg: Color::hex(0xe6e9ef),
            fg_dim: Color::hex(0xacb0be),
            accent: Color::hex(0x7287fd),
            border: Color::hex(0xe6e9ef),
            outline: Color::hex(0x7287fd),
            vision: Color::hex(0x179299),
            flash_bg: Color::hex(0xfe640b),
            flash_fg: Color::hex(0x11111b),
            drag_bg: Color::hex(0xfe640b),
            drag_fg: Color::hex(0x11111b),
            scroll: Color::hex(0x1e66f5),
            disabled_bg: Color::hex(0xccd0da),
            disabled_fg: Color::hex(0x5c5f77),
            chip_bg: Color::hex(0xeff1f5),
            chip_fg: Color::hex(0x4c4f69),
            chip_border: Color::hex(0x4c4f69),
            chip_accent: Color::hex(0x8839ef),
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

/// Which `[theme.*]` palette badges use; `auto` follows the system scheme.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeName {
    #[default]
    Auto,
    Dark,
    Light,
}

/// System colour scheme.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Scheme {
    #[default]
    Dark,
    Light,
}

/// Where the detected [`Scheme`] came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SchemeSource {
    /// XDG settings portal answered with a preference.
    Portal,
    /// Portal unreachable, failing or without preference: dark.
    #[default]
    Fallback,
}

impl Scheme {
    pub fn as_str(self) -> &'static str {
        match self {
            Scheme::Dark => "dark",
            Scheme::Light => "light",
        }
    }
}

impl SchemeSource {
    pub fn as_str(self) -> &'static str {
        match self {
            SchemeSource::Portal => "portal",
            SchemeSource::Fallback => "fallback",
        }
    }
}

/// `[hint]` layout defaults.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HintLayout {
    pub font: String,
    pub font_weight: u16,
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
    /// Display alphabet, resolved from `[keys]` (not a `[hint]` key).
    #[serde(skip)]
    pub alphabet: String,
    pub uppercase: bool,
    /// Widest displayed glyph advance as a fraction of the em size, measured
    /// from the loaded font (not a `[hint]` key).
    #[serde(skip)]
    pub advance_em: f32,
    /// File and face index of the resolved `font`; `None` is the embedded
    /// font (not a `[hint]` key).
    #[serde(skip)]
    pub font_file: Option<(PathBuf, u32)>,
    pub prefix_style: PrefixStyle,
    pub eliminated: Eliminated,
    /// Dash pattern of vision badge borders and outlines, logical px.
    pub vision_dash: [f32; 2],
    pub outline: bool,
    pub outline_width: i32,
    pub theme: ThemeName,
}

impl Default for HintLayout {
    fn default() -> Self {
        Self {
            font: "Atkinson Hyperlegible Mono".into(),
            font_weight: 700,
            size: 11.0,
            min_size: 10.0,
            size_unit: SizeUnit::Logical,
            size_mm: 3.0,
            padding: [4, 4],
            radius: 3.0,
            border_width: 1,
            offset: [-3, -3],
            gap: 2,
            center_below: 20,
            alphabet: "asdfghjkl".into(),
            uppercase: true,
            advance_em: 0.6,
            font_file: None,
            prefix_style: PrefixStyle::DimMatched,
            eliminated: Eliminated::Hide,
            vision_dash: [2.0, 2.0],
            outline: false,
            outline_width: 1,
            theme: ThemeName::Auto,
        }
    }
}

/// How typed glyphs are coloured inside surviving badges.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PrefixStyle {
    /// Matched glyphs in `accent`, remaining in `fg_dim`.
    HighlightMatched,
    /// Vimium style: matched glyphs in `fg_dim`, remaining in `fg`.
    #[default]
    DimMatched,
}

/// What happens to badges that no longer match the typed buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Eliminated {
    #[default]
    Hide,
    Fade,
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
#[serde(default)]
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
#[serde(default)]
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
            tokens: Tokens::dark(),
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
        assert_eq!(format!("{:?}", Tokens::dark().bg), "#a6e3a1");
        assert_eq!(Color::parse("#181825"), Some(Tokens::dark().fg));
        assert_eq!(Tokens::light().chip_accent, Color::hex(0x8839ef));
        assert_eq!(Color::parse("#181825cc"), Some(Tokens::dark().fg));
        assert_eq!(Color::parse("red"), None);
        assert_eq!(Color::parse("#12345g"), None);
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
