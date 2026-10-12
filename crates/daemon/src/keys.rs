//! Label glyphs of the active keyboard layout (`keys.mode = "auto"`).

use std::collections::HashMap;
use std::sync::Arc;

use xkbcommon::xkb;

use woints_core::config::HOME_ROW_KEYCODES;
use woints_core::ports::{KeyPress, KeySource, OverlayKeys};
use woints_hypr::{BindKey, Keyboard, SubmapKeys, SubmapTable, Translate};

/// The keyboard whose layout labels follow: Hyprland's main keyboard, else
/// the first with a layout.
pub fn main_keyboard(kbs: &[Keyboard]) -> Option<&Keyboard> {
    kbs.iter()
        .find(|k| k.main)
        .or_else(|| kbs.iter().find(|k| !k.layout.is_empty()))
}

fn keymap_for(kb: &Keyboard) -> Option<xkb::Keymap> {
    let ctx = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
    let options = (!kb.options.is_empty()).then(|| kb.options.clone());
    xkb::Keymap::new_from_names(
        &ctx,
        &kb.rules,
        &kb.model,
        &kb.layout,
        &kb.variant,
        options,
        xkb::KEYMAP_COMPILE_NO_FLAGS,
    )
}

/// Non-glyph keys a session reads: name and xkb keycode (evdev + 8).
const FIXED_KEYS: [(&str, u32); 6] = [
    ("BackSpace", 22),
    ("Tab", 23),
    ("Up", 111),
    ("Left", 113),
    ("Right", 114),
    ("Down", 116),
];

/// Scroll mode letters, bound whatever the label alphabet is.
const SCROLL_GLYPHS: &str = "hjkl";

/// Keysym and text of a key at one shift level.
type Glyph = (u32, Option<String>);

/// The submap's bind table for `kb`'s active layout: a key for each glyph of
/// `alphabet` and each scroll letter, plus the fixed keys. Also the
/// translation from a reported keycode and modifiers back to a [`KeyPress`].
pub fn bind_table(prefix: &str, kb: &Keyboard, alphabet: &str) -> Option<(SubmapTable, Translate)> {
    let keymap = keymap_for(kb)?;
    let layout = kb.active_layout_index;
    let sym_at = |kc: u32, level: u32| {
        keymap
            .key_get_syms_by_level(xkb::Keycode::new(kc), layout, level)
            .first()
            .copied()
    };
    let press = |sym: xkb::Keysym| {
        let utf8 = xkb::keysym_to_utf8(sym);
        let utf8 = utf8.trim_end_matches('\0').to_owned();
        let utf8 = (!utf8.is_empty() && !utf8.chars().any(char::is_control)).then_some(utf8);
        (sym.raw(), utf8)
    };
    let mut wanted: Vec<char> = Vec::new();
    for c in alphabet.chars().chain(SCROLL_GLYPHS.chars()) {
        let c = c.to_lowercase().next().unwrap_or(c);
        if !wanted.contains(&c) {
            wanted.push(c);
        }
    }
    let mut keys: Vec<BindKey> = Vec::new();
    let mut found: Vec<char> = Vec::new();
    for kc in 8..=255 {
        let Some(sym) = sym_at(kc, 0) else { continue };
        let Some(c) = char::from_u32(xkb::keysym_to_utf32(sym)) else {
            continue;
        };
        if wanted.contains(&c) && !found.contains(&c) {
            found.push(c);
            keys.push(BindKey {
                keysym: xkb::keysym_get_name(sym),
                keycode: kc,
            });
        }
    }
    for (name, kc) in FIXED_KEYS {
        keys.push(BindKey {
            keysym: name.into(),
            keycode: kc,
        });
    }
    let presses: HashMap<u32, [Option<Glyph>; 2]> = keys
        .iter()
        .map(|k| {
            let at = |level| sym_at(k.keycode, level).map(press);
            (k.keycode, [at(0), at(1)])
        })
        .collect();
    let translate: Translate = Arc::new(move |keycode, mods| {
        let [base, shifted] = presses.get(&keycode)?;
        let (keysym, utf8) = if mods.shift {
            shifted.as_ref().or(base.as_ref())?
        } else {
            base.as_ref()?
        };
        Some(KeyPress {
            keycode,
            keysym: *keysym,
            utf8: utf8.clone(),
            mods,
        })
    });
    Some((SubmapTable::new(prefix, &keys), translate))
}

/// Where session keys come from, chosen at start-up and on reload.
#[derive(Clone)]
pub enum Keys {
    /// The overlay's exclusive keyboard focus; `reason` when `auto` fell back.
    Overlay {
        reason: Option<String>,
    },
    Submap(Arc<SubmapKeys>),
    /// `input.keys = "submap"` and the submap could not be installed:
    /// `hints` fails with the reason.
    Unavailable(String),
    #[cfg(test)]
    Test(Arc<tests::Counting>),
}

impl Keys {
    /// `submap <name>`, `overlay`, or `overlay (<reason>)`, for `status`.
    pub fn describe(&self) -> String {
        match self {
            Keys::Overlay { reason: None } => OverlayKeys.kind(),
            Keys::Overlay { reason: Some(r) } => format!("{} ({r})", OverlayKeys.kind()),
            Keys::Submap(k) => k.kind(),
            Keys::Unavailable(r) => format!("unavailable ({r})"),
            #[cfg(test)]
            Keys::Test(_) => "test".into(),
        }
    }

    /// The overlay surfaces take the keyboard.
    pub fn overlay(&self) -> bool {
        matches!(self, Keys::Overlay { .. })
    }

    pub async fn begin(&self, token: u64) {
        match self {
            Keys::Overlay { .. } => OverlayKeys.begin(token).await,
            Keys::Submap(k) => k.begin(token).await,
            Keys::Unavailable(_) => {}
            #[cfg(test)]
            Keys::Test(c) => {
                c.begins.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
        }
    }

    pub async fn end(&self) {
        match self {
            Keys::Overlay { .. } => OverlayKeys.end().await,
            Keys::Submap(k) => k.end().await,
            Keys::Unavailable(_) => {}
            #[cfg(test)]
            Keys::Test(c) => {
                c.ends.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
        }
    }
}

/// Unshifted glyph per [`HOME_ROW_KEYCODES`] entry for `kb`'s active layout.
pub fn layout_glyphs(kb: &Keyboard) -> Option<Vec<Option<char>>> {
    let keymap = keymap_for(kb)?;
    Some(
        HOME_ROW_KEYCODES
            .iter()
            .map(|&kc| {
                keymap
                    .key_get_syms_by_level(xkb::Keycode::new(kc), kb.active_layout_index, 0)
                    .first()
                    .and_then(|&ks| char::from_u32(xkb::keysym_to_utf32(ks)))
                    .filter(|c| *c != '\0')
            })
            .collect(),
    )
}

#[cfg(test)]
pub(crate) mod tests {
    use std::sync::atomic::AtomicU32;

    use super::*;

    /// A key source that counts `begin` and `end` calls.
    #[derive(Default)]
    pub struct Counting {
        pub begins: AtomicU32,
        pub ends: AtomicU32,
    }

    fn kb(layout: &str, variant: &str, index: u32) -> Keyboard {
        Keyboard {
            layout: layout.into(),
            variant: variant.into(),
            active_layout_index: index,
            main: true,
            ..Default::default()
        }
    }

    fn glyphs(k: &Keyboard) -> String {
        layout_glyphs(k)
            .unwrap()
            .into_iter()
            .map(|c| c.unwrap_or('?'))
            .collect()
    }

    #[test]
    fn qwerty_and_colemak_dh() {
        // Needs xkeyboard-config data, present wherever libxkbcommon is.
        if layout_glyphs(&kb("us", "", 0)).is_none() {
            return;
        }
        assert_eq!(glyphs(&kb("us", "", 0)), "asdfjkl;gh");
        assert_eq!(glyphs(&kb("us", "colemak_dh", 0)), "arstneiogm");
        assert_eq!(glyphs(&kb("ru,us", ",colemak_dh", 1)), "arstneiogm");
    }

    #[test]
    fn bind_table_follows_the_alphabet_and_layout() {
        let us = kb("us", "", 0);
        if layout_glyphs(&us).is_none() {
            return;
        }
        let (a, translate) = bind_table("woints-keys", &us, "asdf").unwrap();
        let (same, _) = bind_table("woints-keys", &us, "asdf").unwrap();
        let (other, _) = bind_table("woints-keys", &us, "qwer").unwrap();
        let (colemak, _) = bind_table("woints-keys", &kb("us", "colemak_dh", 0), "asdf").unwrap();
        assert_eq!(a, same);
        assert_ne!(a.name(), other.name());
        assert_ne!(a.name(), colemak.name());
        let mods = woints_core::ports::Modifiers::default();
        let p = translate(38, mods).unwrap();
        assert_eq!((p.keysym, p.utf8.as_deref()), (0x61, Some("a")));
        let shifted = woints_core::ports::Modifiers {
            shift: true,
            ..mods
        };
        assert_eq!(translate(38, shifted).unwrap().utf8.as_deref(), Some("A"));
        assert_eq!(translate(22, mods).unwrap().keysym, 0xff08);
        assert_eq!(translate(23, mods).unwrap().utf8, None);
        assert!(translate(24, mods).is_none());
    }
}
