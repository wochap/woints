//! Label glyphs of the active keyboard layout (`keys.mode = "auto"`).

use xkbcommon::xkb;

use woints_core::config::HOME_ROW_KEYCODES;
use woints_hypr::Keyboard;

/// The keyboard whose layout labels follow: Hyprland's main keyboard, else
/// the first with a layout.
pub fn main_keyboard(kbs: &[Keyboard]) -> Option<&Keyboard> {
    kbs.iter()
        .find(|k| k.main)
        .or_else(|| kbs.iter().find(|k| !k.layout.is_empty()))
}

/// Unshifted glyph per [`HOME_ROW_KEYCODES`] entry for `kb`'s active layout.
pub fn layout_glyphs(kb: &Keyboard) -> Option<Vec<Option<char>>> {
    let ctx = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
    let options = (!kb.options.is_empty()).then(|| kb.options.clone());
    let keymap = xkb::Keymap::new_from_names(
        &ctx,
        &kb.rules,
        &kb.model,
        &kb.layout,
        &kb.variant,
        options,
        xkb::KEYMAP_COMPILE_NO_FLAGS,
    )?;
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
mod tests {
    use super::*;

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
}
