//! `config.toml`: typed sections, defaults, validation and loading (design D17).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::act::{InputConfig, ModifierMap, PointerKind, Prefer};
use crate::merge::MergeParams;
use crate::model::{Action, Scope};
use crate::theme::{ChipLayout, Color, HintLayout, Theme, ThemeName, TimingLayout, Tokens};

/// The shipped example, identical to the built-in defaults.
pub const DEFAULT_TOML: &str = include_str!("../../../config.example.toml");

/// Glob match with `*` (any run) and `?` (one char).
pub fn glob(pattern: &str, s: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let s: Vec<char> = s.chars().collect();
    let (mut pi, mut si) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while si < s.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == s[si]) {
            pi += 1;
            si += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some((pi, si));
            pi += 1;
        } else if let Some((sp, ss)) = star {
            pi = sp + 1;
            si = ss + 1;
            star = Some((sp, ss + 1));
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|c| *c == '*')
}

/// When the vision provider runs (`vision.mode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VisionMode {
    #[default]
    #[serde(rename = "auto", alias = "fallback")]
    Fallback,
    Always,
    Off,
}

/// Edge operator of the classical detector.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EdgeMode {
    #[default]
    Canny,
    Laplacian,
}

/// `[vision]` keys.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct VisionConfig {
    pub mode: VisionMode,
    pub min_atspi_elements: usize,
    /// Hyprland classes (glob, `*` and `?`) that always get vision.
    pub force_classes: Vec<String>,
    /// Smallest box side, logical px.
    pub min_box: i32,
    /// Largest box side, logical px.
    pub max_box: i32,
    pub max_aspect: f32,
    /// Tallest component merged into text lines, logical px.
    pub text_height: i32,
    pub downscale_width: u32,
    /// Canny `(low, high)` thresholds on gradient magnitude.
    pub canny: (f32, f32),
    pub edge: EdgeMode,
    pub budget_ms: u64,
    pub max_elements: usize,
    pub link_hue: bool,
}

impl Default for VisionConfig {
    fn default() -> Self {
        Self {
            mode: VisionMode::Fallback,
            min_atspi_elements: 3,
            force_classes: Vec::new(),
            min_box: 8,
            max_box: 600,
            max_aspect: 20.0,
            text_height: 15,
            downscale_width: 1920,
            canny: (70.0, 220.0),
            edge: EdgeMode::Canny,
            budget_ms: 250,
            max_elements: 150,
            link_hue: true,
        }
    }
}

/// `[theme.mocha]`, `[theme.latte]`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Themes {
    pub mocha: Tokens,
    pub latte: Tokens,
}

impl Default for Themes {
    fn default() -> Self {
        Self {
            mocha: Tokens::mocha(),
            latte: Tokens::latte(),
        }
    }
}

/// How the displayed label alphabet is chosen (`keys.mode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum KeysMode {
    /// Glyphs of the active layout on the home-row keycodes.
    #[default]
    Auto,
    /// `keys.alphabet` as written.
    Explicit,
}

/// Home-row keycodes (xkb, evdev + 8) in label order, then the extras.
pub const HOME_ROW_KEYCODES: [u32; 10] = [38, 39, 40, 41, 44, 45, 46, 47, 42, 43];

/// `[keys]`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct KeysConfig {
    pub mode: KeysMode,
    pub alphabet: String,
    pub exclude: Vec<String>,
}

impl Default for KeysConfig {
    fn default() -> Self {
        Self {
            mode: KeysMode::Auto,
            alphabet: "asdfghjkl".into(),
            exclude: Vec::new(),
        }
    }
}

impl KeysConfig {
    /// Display alphabet for `keys.mode = auto` from the layout's glyph per
    /// [`HOME_ROW_KEYCODES`] entry: lowercase alphanumerics only, minus
    /// `exclude`, first occurrence wins. Falls back to `alphabet` when fewer
    /// than four glyphs survive.
    pub fn auto_alphabet(&self, glyphs: &[Option<char>]) -> String {
        let excluded = |c: char| self.exclude.iter().any(|e| e.chars().eq([c]));
        let mut out = String::new();
        for c in glyphs.iter().flatten() {
            let c = c.to_lowercase().next().unwrap_or(*c);
            if c.is_alphanumeric() && !excluded(c) && !out.contains(c) {
                out.push(c);
            }
        }
        if out.chars().count() < 4 {
            self.alphabet.clone()
        } else {
            out
        }
    }
}

/// Compositor layer levels (`scope.levels`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LayerLevel {
    Background,
    Bottom,
    Top,
    Overlay,
}

impl LayerLevel {
    /// Hyprland `layers` level index.
    pub fn index(self) -> u8 {
        self as u8
    }
}

/// `[scope]`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ScopeConfig {
    pub default: Scope,
    pub levels: Vec<LayerLevel>,
    pub ignore_layers: Vec<String>,
}

impl Default for ScopeConfig {
    fn default() -> Self {
        Self {
            default: Scope::Focused,
            // Bars commonly live on `bottom` (quickshell's bar does).
            levels: vec![LayerLevel::Bottom, LayerLevel::Top, LayerLevel::Overlay],
            ignore_layers: vec!["awww-daemon".into(), "wayland-pipewire-idle-inhibit".into()],
        }
    }
}

/// `[input]`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct InputSection {
    pub pointer: PointerKind,
    pub prefer: PreferKey,
    pub restore_cursor: bool,
    pub double_click_gap_ms: u32,
    pub drag_steps: u32,
    pub scroll_step: u32,
    pub unmap_timeout_ms: u32,
    /// Modifier → action name or `"none"`.
    pub modifiers: BTreeMap<String, String>,
}

/// `input.prefer`: `"action"` (AT-SPI first) or `"pointer"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PreferKey {
    #[default]
    #[serde(alias = "atspi")]
    Action,
    Pointer,
}

impl Default for InputSection {
    fn default() -> Self {
        let i = InputConfig::default();
        let modifiers = [
            ("shift", "right"),
            ("ctrl", "middle"),
            ("alt", "hover"),
            ("super", "none"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect();
        Self {
            pointer: i.pointer,
            prefer: PreferKey::Action,
            restore_cursor: true,
            double_click_gap_ms: i.double_click_gap_ms,
            drag_steps: i.drag_steps,
            scroll_step: i.scroll_step,
            unmap_timeout_ms: i.unmap_timeout_ms,
            modifiers,
        }
    }
}

/// `[actions]`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ActionsConfig {
    pub default: Action,
}

/// `providers.quickshell.mode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProviderMode {
    #[default]
    Auto,
    Off,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct QuickshellConfig {
    pub mode: ProviderMode,
    /// Budget for one `qs ipc call`, ms; the child is killed on overrun.
    pub budget_ms: u64,
}

impl Default for QuickshellConfig {
    fn default() -> Self {
        Self {
            mode: ProviderMode::Auto,
            budget_ms: 40,
        }
    }
}

/// `[providers]`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Providers {
    pub quickshell: QuickshellConfig,
}

/// `[apps]`: per-class overrides (globs).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppsConfig {
    /// Windows of these classes are never hinted.
    pub disabled_classes: Vec<String>,
    /// Added to `vision.force_classes`.
    pub force_vision_classes: Vec<String>,
}

/// Every config section with its defaults.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub hint: HintLayout,
    pub timing: TimingLayout,
    pub chip: ChipLayout,
    pub theme: Themes,
    pub keys: KeysConfig,
    pub scope: ScopeConfig,
    pub vision: VisionConfig,
    pub merge: MergeParams,
    pub input: InputSection,
    pub actions: ActionsConfig,
    pub providers: Providers,
    pub apps: AppsConfig,
}

impl Config {
    /// Theme for drawing with `alphabet` as the displayed label glyphs.
    pub fn theme(&self, alphabet: &str) -> Theme {
        let mut hint = self.hint.clone();
        hint.alphabet = alphabet.to_owned();
        Theme {
            tokens: match self.hint.theme {
                ThemeName::Mocha => self.theme.mocha.clone(),
                ThemeName::Latte => self.theme.latte.clone(),
            },
            hint,
            chip: self.chip.clone(),
            timing: self.timing.clone(),
        }
    }

    /// `[input]` and `[actions]` as the actuators see them.
    pub fn input_config(&self) -> InputConfig {
        let i = &self.input;
        let pairs = i.modifiers.iter().map(|(k, v)| (k.as_str(), v.as_str()));
        InputConfig {
            pointer: i.pointer,
            prefer: match i.prefer {
                PreferKey::Action => Prefer::Atspi,
                PreferKey::Pointer => Prefer::Pointer,
            },
            restore_cursor: i.restore_cursor,
            double_click_gap_ms: i.double_click_gap_ms,
            drag_steps: i.drag_steps,
            scroll_step: i.scroll_step,
            unmap_timeout_ms: i.unmap_timeout_ms,
            // Validated on load.
            modifiers: ModifierMap::from_pairs(pairs).unwrap_or_default(),
            default_action: self.actions.default,
        }
    }

    /// `[vision]` with `apps.force_vision_classes` folded in.
    pub fn vision_config(&self) -> VisionConfig {
        let mut v = self.vision.clone();
        v.force_classes
            .extend(self.apps.force_vision_classes.iter().cloned());
        v
    }

    pub fn class_disabled(&self, class: &str) -> bool {
        self.apps.disabled_classes.iter().any(|g| glob(g, class))
    }

    /// Display alphabet: `keys.alphabet` in explicit mode, else derived
    /// from `layout` glyphs when known.
    pub fn alphabet(&self, layout: Option<&[Option<char>]>) -> String {
        match (self.keys.mode, layout) {
            (KeysMode::Auto, Some(g)) => self.keys.auto_alphabet(g),
            _ => {
                let excluded = |c: char| self.keys.exclude.iter().any(|e| e.chars().eq([c]));
                self.keys
                    .alphabet
                    .chars()
                    .filter(|&c| !excluded(c))
                    .collect()
            }
        }
    }
}

/// A successfully parsed file plus its non-fatal warnings.
#[derive(Debug, Clone, PartialEq)]
pub struct Loaded {
    pub config: Config,
    pub warnings: Vec<String>,
}

/// Parse and validate config text. Errors name the offending key
/// (`theme.mocha.bg: expected #rrggbb or #rrggbbaa`); unknown keys are
/// returned as warnings.
pub fn parse(text: &str) -> Result<Loaded, Vec<String>> {
    let mut table: toml::Table = text
        .parse()
        .map_err(|e: toml::de::Error| vec![format!("syntax: {}", e.message())])?;
    flatten_theme_hint(&mut table);
    let errors = validate(&table);
    if !errors.is_empty() {
        return Err(errors);
    }
    let mut merged = toml::Table::try_from(Config::default()).expect("defaults serialise");
    let mut warnings = Vec::new();
    unknown_keys(&table, &merged, "", &mut warnings);
    overlay(&mut merged, table);
    let config: Config = toml::Value::Table(merged)
        .try_into()
        .map_err(|e: toml::de::Error| vec![e.message().to_owned()])?;
    Ok(Loaded { config, warnings })
}

/// Deep-merge `user` over `base`, so partial sections keep their defaults.
fn overlay(base: &mut toml::Table, user: toml::Table) {
    for (k, v) in user {
        match (base.get_mut(&k), v) {
            (Some(toml::Value::Table(b)), toml::Value::Table(u)) => overlay(b, u),
            (_, v) => {
                base.insert(k, v);
            }
        }
    }
}

/// The design writes theme tokens as `hint.bg = ...` inside `[theme.mocha]`.
fn flatten_theme_hint(t: &mut toml::Table) {
    let Some(toml::Value::Table(themes)) = t.get_mut("theme") else {
        return;
    };
    for (_, theme) in themes.iter_mut() {
        if let toml::Value::Table(tt) = theme
            && let Some(toml::Value::Table(h)) = tt.remove("hint")
        {
            for (k, v) in h {
                tt.entry(k).or_insert(v);
            }
        }
    }
}

fn unknown_keys(user: &toml::Table, defaults: &toml::Table, prefix: &str, out: &mut Vec<String>) {
    for (k, v) in user {
        let path = if prefix.is_empty() {
            k.clone()
        } else {
            format!("{prefix}.{k}")
        };
        match (v, defaults.get(k)) {
            (_, None) => out.push(format!("{path}: unknown key")),
            (toml::Value::Table(u), Some(toml::Value::Table(d))) => unknown_keys(u, d, &path, out),
            _ => {}
        }
    }
}

fn get<'a>(t: &'a toml::Table, path: &str) -> Option<&'a toml::Value> {
    let mut parts = path.split('.');
    let mut v = t.get(parts.next()?)?;
    for p in parts {
        v = v.as_table()?.get(p)?;
    }
    Some(v)
}

/// Rules serde types cannot express, each reported as `<key>: <message>`.
fn validate(t: &toml::Table) -> Vec<String> {
    let mut errs = Vec::new();
    if let Some(v) = get(t, "keys.alphabet") {
        match v.as_str() {
            Some(s) => {
                let mut uniq: Vec<char> = s.chars().collect();
                uniq.sort_unstable();
                uniq.dedup();
                if uniq.len() < 4 {
                    errs.push("keys.alphabet: needs at least 4 unique glyphs".into());
                }
            }
            None => errs.push("keys.alphabet: expected a string".into()),
        }
    }
    if let Some(v) = get(t, "merge.iou_threshold") {
        match v.as_float().or_else(|| v.as_integer().map(|i| i as f64)) {
            Some(x) if x > 0.0 && x <= 1.0 => {}
            _ => errs.push("merge.iou_threshold: expected a number in (0, 1]".into()),
        }
    }
    if let Some(v) = get(t, "vision.min_atspi_elements") {
        if v.as_integer().is_none_or(|i| i < 0) {
            errs.push("vision.min_atspi_elements: expected an integer >= 0".into());
        }
    }
    if let Some(toml::Value::Table(themes)) = t.get("theme") {
        for (name, theme) in themes {
            let Some(tt) = theme.as_table() else {
                errs.push(format!("theme.{name}: expected a table"));
                continue;
            };
            for (k, v) in tt {
                if v.as_str().is_none_or(|s| Color::parse(s).is_none()) {
                    errs.push(format!("theme.{name}.{k}: expected #rrggbb or #rrggbbaa"));
                }
            }
        }
    }
    if let Some(v) = get(t, "input.modifiers") {
        match v.as_table() {
            Some(m) => {
                for (k, a) in m {
                    let Some(a) = a.as_str() else {
                        errs.push(format!("input.modifiers.{k}: expected an action name"));
                        continue;
                    };
                    if let Err(e) = ModifierMap::from_pairs([(k.as_str(), a)]) {
                        errs.push(format!("input.{e}"));
                    }
                }
            }
            None => errs.push("input.modifiers: expected a table".into()),
        }
    }
    if let Some(v) = get(t, "scope.default") {
        if v.as_str().is_none_or(|s| s.parse::<Scope>().is_err()) {
            errs.push(
                "scope.default: expected \"focused\", \"focused+layers\", \"layers\" or \"all\""
                    .into(),
            );
        }
    }
    errs
}

/// `$XDG_CONFIG_HOME/woints/config.toml`, else `~/.config/woints/config.toml`.
pub fn default_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("woints").join("config.toml"))
}

/// Where a config came from.
#[derive(Debug, Clone, PartialEq)]
pub enum Source {
    Defaults,
    File(PathBuf),
}

/// Read and parse `path`. A missing file yields the defaults.
pub fn load(path: &Path) -> Result<(Loaded, Source), Vec<String>> {
    match std::fs::read_to_string(path) {
        Ok(text) => parse(&text).map(|l| (l, Source::File(path.to_owned()))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok((
            Loaded {
                config: Config::default(),
                warnings: Vec::new(),
            },
            Source::Defaults,
        )),
        Err(e) => Err(vec![format!("{}: {e}", path.display())]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn err(text: &str) -> Vec<String> {
        parse(text).unwrap_err()
    }

    #[test]
    fn example_file_equals_defaults() {
        let l = parse(DEFAULT_TOML).unwrap();
        assert_eq!(l.warnings, Vec::<String>::new());
        assert_eq!(l.config, Config::default());
    }

    /// `[hint]`, `[timing]`, `[chip]` and both palettes as written by the
    /// design's `buildToml()` (§11).
    #[test]
    fn design_parity() {
        let html = include_str!("../../../design/project/woints Overlay Spec.dc.html");
        let start = html.find("L.push('# woints").unwrap() + "L.push(".len();
        let end = start + html[start..].find("');").unwrap() + 1;
        let lines: Vec<&str> = html[start..end]
            .trim_matches(['\'', ','])
            .split("','")
            .collect();
        let l = parse(&lines.join("\n")).unwrap();
        assert_eq!(l.warnings, ["hint.alphabet: unknown key"]);
        let d = Config::default();
        assert_eq!(l.config.hint, d.hint);
        assert_eq!(l.config.timing, d.timing);
        assert_eq!(l.config.chip, d.chip);
        for (name, tokens) in [("MOCHA", &d.theme.mocha), ("LATTE", &d.theme.latte)] {
            let tag = format!("const {name} = {{");
            let s = html.find(&tag).unwrap() + tag.len();
            let e = s + html[s..].find('}').unwrap();
            let toml: String = html[s..e]
                .split(',')
                .map(|kv| {
                    let (k, v) = kv.split_once(':').unwrap();
                    format!("{k} = \"{}\"\n", v.trim_matches('\''))
                })
                .collect();
            let parsed: Tokens = toml::from_str(&toml).unwrap();
            assert_eq!(&parsed, tokens, "{name}");
        }
        assert_eq!(d.keys.alphabet, "asdfghjkl");
    }

    #[test]
    fn partial_file() {
        let l = parse("[hint]\ntheme = \"latte\"\n").unwrap();
        let mut d = Config::default();
        d.hint.theme = ThemeName::Latte;
        assert_eq!(l.config, d);
        assert_eq!(l.config.theme("asdf").tokens, Tokens::latte());
    }

    #[test]
    fn design_style_theme_keys() {
        let l = parse("[theme.mocha]\nhint.bg = \"#000000\"\n").unwrap();
        assert_eq!(l.config.theme.mocha.bg, Color::hex(0));
        assert_eq!(l.config.theme.mocha.fg, Tokens::mocha().fg);
    }

    #[test]
    fn bad_colour() {
        assert_eq!(
            err("[theme.mocha]\nbg = \"red\"\n"),
            ["theme.mocha.bg: expected #rrggbb or #rrggbbaa"]
        );
        assert!(parse("[theme.mocha]\nbg = \"#11111bcc\"\n").is_ok());
    }

    #[test]
    fn bad_alphabet() {
        assert_eq!(
            err("[keys]\nalphabet = \"aaab\"\n"),
            ["keys.alphabet: needs at least 4 unique glyphs"]
        );
    }

    #[test]
    fn bad_iou() {
        for v in ["0", "0.0", "1.5", "-1"] {
            assert_eq!(
                err(&format!("[merge]\niou_threshold = {v}\n")),
                ["merge.iou_threshold: expected a number in (0, 1]"],
                "{v}"
            );
        }
        assert!(parse("[merge]\niou_threshold = 1\n").is_ok());
    }

    #[test]
    fn bad_min_atspi() {
        assert_eq!(
            err("[vision]\nmin_atspi_elements = -1\n"),
            ["vision.min_atspi_elements: expected an integer >= 0"]
        );
    }

    #[test]
    fn bad_modifier() {
        assert_eq!(
            err("[input.modifiers]\nctrl = \"teleport\"\n"),
            ["input.modifiers.ctrl: unknown action \"teleport\""]
        );
        assert_eq!(
            err("[input]\nmodifiers = { hyper = \"click\" }\n"),
            ["input.modifiers.hyper: unknown modifier"]
        );
    }

    #[test]
    fn bad_scope_and_syntax() {
        assert_eq!(err("[scope]\ndefault = \"everything\"\n").len(), 1);
        assert!(err("[hint\n")[0].starts_with("syntax: "));
        assert!(!err("[hint]\nsize = \"big\"\n").is_empty());
    }

    #[test]
    fn unknown_keys_warn() {
        let l = parse("[hint]\nsparkle = true\n[future]\nx = 1\n").unwrap();
        assert_eq!(
            l.warnings,
            ["future: unknown key", "hint.sparkle: unknown key"]
        );
    }

    #[test]
    fn input_and_vision_views() {
        let l = parse(
            "[input]\nprefer = \"pointer\"\nmodifiers = { alt = \"double\", shift = \"none\" }\n\
             [apps]\nforce_vision_classes = [\"steam*\"]\n[actions]\ndefault = \"right\"\n",
        )
        .unwrap();
        let i = l.config.input_config();
        assert_eq!(i.prefer, Prefer::Pointer);
        assert_eq!(i.modifiers.alt, Some(Action::Double));
        assert_eq!(i.modifiers.shift, None);
        assert_eq!(i.modifiers.ctrl, Some(Action::Middle));
        assert_eq!(i.default_action, Action::Right);
        assert!(i.restore_cursor);
        assert_eq!(l.config.vision_config().force_classes, ["steam*"]);
    }

    #[test]
    fn scope_section() {
        let l = parse("[scope]\ndefault = \"focused+layers\"\nlevels = [\"top\"]\n").unwrap();
        assert_eq!(l.config.scope.default, Scope::FocusedAndLayers);
        assert_eq!(l.config.scope.levels, [LayerLevel::Top]);
        assert_eq!(LayerLevel::Overlay.index(), 3);
    }

    #[test]
    fn alphabets() {
        let mut c = Config::default();
        // Colemak-DH home row: a r s t | n e i o, extras g m.
        let colemak: Vec<Option<char>> = "arstneiogm".chars().map(Some).collect();
        assert_eq!(c.alphabet(Some(&colemak)), "arstneiogm");
        // `;` on keycode 47 is dropped.
        let qwerty: Vec<Option<char>> = "asdfjkl;gh".chars().map(Some).collect();
        assert_eq!(c.alphabet(Some(&qwerty)), "asdfjklgh");
        c.keys.exclude = vec!["g".into()];
        assert_eq!(c.alphabet(Some(&qwerty)), "asdfjklh");
        assert_eq!(c.alphabet(None), "asdfhjkl");
        c.keys.mode = KeysMode::Explicit;
        assert_eq!(c.alphabet(Some(&colemak)), "asdfhjkl");
    }

    #[test]
    fn missing_file_is_defaults() {
        let (l, src) = load(Path::new("/nonexistent/woints/config.toml")).unwrap();
        assert_eq!(src, Source::Defaults);
        assert_eq!(l.config, Config::default());
    }

    #[test]
    fn globs() {
        assert!(glob("steam*", "steam_app_1"));
        assert!(glob("f?ot", "foot"));
        assert!(!glob("foot", "footclient"));
    }
}
