//! Label glyphs of the active keyboard layout (`keys.mode = "auto"`).

use std::sync::{Arc, mpsc};

use xkbcommon::xkb;

use woints_core::config::HOME_ROW_KEYCODES;
use woints_core::ports::{KeyPress, KeySource, Modifiers, OverlayKeys};
pub use woints_hypr::MODULE_MISSING;
use woints_hypr::{Keyboard, SubmapKeys};

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

/// Whether `sym` is a modifier or lock key (Shift_L..Hyper_R, ISO level
/// shifts and locks, Mode_switch, Num_Lock).
fn is_modifier_sym(sym: u32) -> bool {
    matches!(sym, 0xffe1..=0xffee | 0xfe01..=0xfe13 | 0xff7e | 0xff7f)
}

/// Turns the key reports of one submap session into [`KeyPress`]es. An xkb
/// state of the main keyboard's active layout lives on its own thread (it
/// isn't `Send`), so modifiers, remapped modifiers and shift levels follow
/// the keymap. The thread ends when the translator is dropped.
pub struct Translator {
    tx: mpsc::Sender<(u32, bool)>,
    rx: mpsc::Receiver<Option<KeyPress>>,
}

impl Translator {
    pub fn new(kb: &Keyboard) -> Option<Self> {
        let kb = kb.clone();
        let (tx, requests) = mpsc::channel::<(u32, bool)>();
        let (replies, rx) = mpsc::channel();
        let (ready_tx, ready) = mpsc::channel();
        std::thread::spawn(move || {
            let Some(keymap) = keymap_for(&kb) else {
                let _ = ready_tx.send(false);
                return;
            };
            let mut state = xkb::State::new(&keymap);
            state.update_mask(0, 0, 0, 0, 0, kb.active_layout_index);
            let _ = ready_tx.send(true);
            for (keycode, pressed) in requests {
                if replies
                    .send(translate(&mut state, keycode, pressed))
                    .is_err()
                {
                    return;
                }
            }
        });
        ready.recv().unwrap_or(false).then_some(Self { tx, rx })
    }

    /// Feed one key report. `Some` for the press of a non-modifier key.
    pub fn feed(&mut self, keycode: u32, pressed: bool) -> Option<KeyPress> {
        self.tx.send((keycode, pressed)).ok()?;
        self.rx.recv().ok().flatten()
    }
}

fn translate(state: &mut xkb::State, keycode: u32, pressed: bool) -> Option<KeyPress> {
    let key = xkb::Keycode::new(keycode);
    let dir = if pressed {
        xkb::KeyDirection::Down
    } else {
        xkb::KeyDirection::Up
    };
    state.update_key(key, dir);
    if !pressed {
        return None;
    }
    let sym = state.key_get_one_sym(key);
    if is_modifier_sym(sym.raw()) {
        return None;
    }
    let utf8 = state.key_get_utf8(key);
    let utf8 = (!utf8.is_empty() && !utf8.chars().any(char::is_control)).then_some(utf8);
    let active = |name| state.mod_name_is_active(name, xkb::STATE_MODS_EFFECTIVE);
    Some(KeyPress {
        keycode,
        keysym: sym.raw(),
        utf8,
        mods: Modifiers {
            shift: active(xkb::MOD_NAME_SHIFT),
            ctrl: active(xkb::MOD_NAME_CTRL),
            alt: active(xkb::MOD_NAME_ALT),
            logo: active(xkb::MOD_NAME_LOGO),
        },
    })
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

    /// A translator on a `us` keymap, `None` without xkeyboard-config data.
    fn translator(options: &str) -> Option<Translator> {
        let mut k = kb("us", "", 0);
        k.options = options.into();
        Translator::new(&k)
    }

    #[test]
    fn shift_then_d_is_a_shifted_d() {
        let Some(mut t) = translator("") else { return };
        assert!(t.feed(50, true).is_none());
        let p = t.feed(40, true).unwrap();
        assert_eq!((p.keysym, p.utf8.as_deref()), (0x44, Some("D")));
        assert!(p.mods.shift && !p.mods.ctrl);
        assert!(t.feed(40, false).is_none());
        assert!(t.feed(50, false).is_none());
        let p = t.feed(40, true).unwrap();
        assert_eq!((p.keysym, p.utf8.as_deref()), (0x64, Some("d")));
        assert_eq!(p.mods, Modifiers::default());
    }

    #[test]
    fn remapped_modifier_is_respected() {
        let Some(mut t) = translator("caps:ctrl_modifier") else {
            return;
        };
        assert!(t.feed(66, true).is_none());
        let p = t.feed(40, true).unwrap();
        assert!(p.mods.ctrl && !p.mods.shift);
        assert_eq!(p.keysym, 0x64);
    }

    #[test]
    fn modifier_alone_emits_nothing() {
        let Some(mut t) = translator("") else { return };
        for kc in [50, 37, 64, 133] {
            assert!(t.feed(kc, true).is_none());
            assert!(t.feed(kc, false).is_none());
        }
    }

    #[test]
    fn fixed_keys_translate() {
        let Some(mut t) = translator("") else { return };
        assert_eq!(t.feed(22, true).unwrap().keysym, 0xff08);
        assert_eq!(t.feed(23, true).unwrap().utf8, None);
    }
}
