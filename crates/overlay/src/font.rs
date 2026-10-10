//! Hint font lookup and loading (design D1–D3): system family by name with
//! the embedded Atkinson Hyperlegible Mono Bold as fallback.

use std::path::PathBuf;

use fontdb::{Database, Source, Style};
use fontdue::{Font, FontSettings};

use crate::render::FONT_BYTES;

pub use fontdb;

/// Family name of the embedded font.
pub const EMBEDDED_FAMILY: &str = "Atkinson Hyperlegible Mono";
/// Weight of the embedded font.
pub const EMBEDDED_WEIGHT: u16 = 700;

/// Pixel size advances are measured at.
const MEASURE_PX: f32 = 100.0;

/// The face chosen for `hint.font` / `hint.font_weight`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedFont {
    /// `hint.font` as configured.
    pub requested: String,
    pub family: String,
    pub weight: u16,
    /// Font file; `None` means the embedded font.
    pub path: Option<PathBuf>,
    pub index: u32,
    /// An installed family matched `requested`.
    pub found: bool,
}

impl ResolvedFont {
    /// The embedded font standing in for `requested`.
    pub fn embedded(requested: &str) -> Self {
        Self {
            requested: requested.to_owned(),
            family: EMBEDDED_FAMILY.into(),
            weight: EMBEDDED_WEIGHT,
            path: None,
            index: 0,
            found: false,
        }
    }

    /// No installed match for a family other than the embedded one.
    pub fn missing(&self) -> bool {
        !self.found && !self.requested.eq_ignore_ascii_case(EMBEDDED_FAMILY)
    }

    /// `(path, index)` of the face file, `None` for the embedded font.
    pub fn file(&self) -> Option<(PathBuf, u32)> {
        self.path.clone().map(|p| (p, self.index))
    }
}

/// System font database, scanning fontconfig directories (slow; see D5).
pub fn system_db() -> Database {
    let mut db = Database::new();
    db.load_system_fonts();
    db
}

/// Pick the face for `family` / `weight` (design D2): normal style, exact
/// case-insensitive family, else the shortest family starting with `family`,
/// then the nearest weight.
pub fn resolve(db: &Database, family: &str, weight: u16) -> ResolvedFont {
    let want = family.to_lowercase();
    let faces: Vec<_> = db.faces().filter(|f| f.style == Style::Normal).collect();
    let names = |f: &fontdb::FaceInfo| {
        f.families
            .iter()
            .map(|(n, _)| n.clone())
            .collect::<Vec<_>>()
    };
    let exact = faces
        .iter()
        .flat_map(|f| names(f))
        .find(|n| n.to_lowercase() == want);
    let chosen = exact.or_else(|| {
        faces
            .iter()
            .flat_map(|f| names(f))
            .filter(|n| !want.is_empty() && n.to_lowercase().starts_with(&want))
            .min_by_key(|n| (n.len(), n.clone()))
    });
    let Some(name) = chosen else {
        return ResolvedFont::embedded(family);
    };
    let Some(face) = faces
        .iter()
        .filter(|f| f.families.iter().any(|(n, _)| *n == name))
        .min_by_key(|f| (f.weight.0.abs_diff(weight), f.weight.0))
    else {
        return ResolvedFont::embedded(family);
    };
    let path = match &face.source {
        Source::File(p) | Source::SharedFile(p, _) => Some(p.clone()),
        Source::Binary(_) => None,
    };
    ResolvedFont {
        requested: family.to_owned(),
        family: name,
        weight: face.weight.0,
        path,
        index: face.index,
        found: true,
    }
}

fn embedded_font() -> Font {
    Font::from_bytes(FONT_BYTES, FontSettings::default()).expect("embedded font must parse")
}

/// Load the face at `file`, or the embedded font for `None` or on error
/// (logged).
pub fn load_file(file: Option<&(PathBuf, u32)>) -> Font {
    let Some((path, index)) = file else {
        return embedded_font();
    };
    let settings = FontSettings {
        collection_index: *index,
        ..FontSettings::default()
    };
    match std::fs::read(path)
        .map_err(|e| e.to_string())
        .and_then(|b| Font::from_bytes(b, settings).map_err(str::to_owned))
    {
        Ok(f) => f,
        Err(e) => {
            tracing::warn!(path = %path.display(), "hint font: {e}; using embedded {EMBEDDED_FAMILY}");
            embedded_font()
        }
    }
}

/// Load the resolved face.
pub fn load(font: &ResolvedFont) -> Font {
    load_file(font.file().as_ref())
}

/// Largest advance width among `glyphs`, as a fraction of the em size.
pub fn advance_em(font: &Font, glyphs: &str) -> f32 {
    glyphs
        .chars()
        .map(|c| font.metrics(c, MEASURE_PX).advance_width / MEASURE_PX)
        .fold(0.0, f32::max)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use fontdb::{FaceInfo, ID, Language, Stretch, Weight};

    use super::*;

    fn face(family: &str, weight: u16, style: Style) -> FaceInfo {
        FaceInfo {
            id: ID::dummy(),
            source: Source::File(PathBuf::from(format!("/fonts/{family}-{weight}.ttf"))),
            index: 0,
            families: vec![(family.into(), Language::English_UnitedStates)],
            post_script_name: family.replace(' ', ""),
            style,
            weight: Weight(weight),
            stretch: Stretch::Normal,
            monospaced: true,
        }
    }

    fn db(faces: &[(&str, u16)]) -> Database {
        let mut db = Database::new();
        for &(f, w) in faces {
            db.push_face_info(face(f, w, Style::Normal));
        }
        db
    }

    #[test]
    fn font_exact_match() {
        let db = db(&[("Iosevka Nerd Font", 700), ("Iosevka Nerd Font Mono", 700)]);
        let r = resolve(&db, "iosevka nerd font mono", 700);
        assert_eq!(r.family, "Iosevka Nerd Font Mono");
        assert!(r.found && !r.missing());
        assert_eq!(
            r.path.as_deref(),
            Some(std::path::Path::new("/fonts/Iosevka Nerd Font Mono-700.ttf"))
        );
    }

    #[test]
    fn font_prefix_match_shortest() {
        let db = db(&[
            ("Iosevka Nerd Font Propo", 700),
            ("Iosevka Nerd Font", 700),
            ("Iosevka Nerd Font Mono", 700),
        ]);
        assert_eq!(resolve(&db, "Iosevka", 700).family, "Iosevka Nerd Font");
    }

    #[test]
    fn font_nearest_weight_normal_style() {
        let mut d = db(&[("Iosevka", 400), ("Iosevka", 800)]);
        d.push_face_info(face("Iosevka", 700, Style::Italic));
        let r = resolve(&d, "Iosevka", 700);
        assert_eq!(r.weight, 800);
        assert_eq!(resolve(&d, "Iosevka", 500).weight, 400);
    }

    #[test]
    fn font_not_found() {
        let db = db(&[("Iosevka", 700)]);
        let r = resolve(&db, "NoSuchFont", 700);
        assert_eq!(r, ResolvedFont::embedded("NoSuchFont"));
        assert!(r.missing());
        let r = resolve(&db, EMBEDDED_FAMILY, 700);
        assert!(!r.found && !r.missing());
    }

    #[test]
    fn font_binary_source_has_no_path() {
        let mut d = Database::new();
        let mut f = face("Mem", 400, Style::Normal);
        f.source = Source::Binary(Arc::new(FONT_BYTES));
        d.push_face_info(f);
        assert_eq!(resolve(&d, "Mem", 400).path, None);
    }

    #[test]
    fn font_embedded_advance() {
        let a = advance_em(&load_file(None), "ASDFGHJKL");
        assert!((a - 0.6).abs() < 0.05, "advance {a}");
    }

    #[test]
    fn font_missing_file_falls_back() {
        let f = load_file(Some(&(PathBuf::from("/nonexistent/font.ttf"), 0)));
        let a = advance_em(&f, "AS");
        assert!((a - 0.6).abs() < 0.05, "advance {a}");
    }
}
