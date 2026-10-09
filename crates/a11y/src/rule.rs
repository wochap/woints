//! `Collection.GetMatches` rule, encoded by hand.
//!
//! The wire format is `(aiia{ss}iaiiasib)`: states and roles are *bitsets*
//! packed into `i32` words. `atspi-common`'s `ObjectMatchRule` sends roles as
//! a list of role numbers, which at-spi2-core reads as a bitset, so it is not
//! used here.

use std::collections::HashMap;

use serde::Serialize;
use zvariant::Type;

/// AT-SPI role numbers (`AtspiRole`).
pub mod role {
    pub const CHECK_BOX: u32 = 7;
    pub const CHECK_MENU_ITEM: u32 = 8;
    pub const COMBO_BOX: u32 = 11;
    pub const LIST_ITEM: u32 = 32;
    pub const MENU_ITEM: u32 = 35;
    pub const PAGE_TAB: u32 = 37;
    pub const PASSWORD_TEXT: u32 = 40;
    pub const PUSH_BUTTON: u32 = 43;
    pub const RADIO_BUTTON: u32 = 44;
    pub const RADIO_MENU_ITEM: u32 = 45;
    pub const SLIDER: u32 = 51;
    pub const SPIN_BUTTON: u32 = 52;
    pub const TOGGLE_BUTTON: u32 = 62;
    pub const ENTRY: u32 = 79;
    pub const LINK: u32 = 88;
    pub const TREE_ITEM: u32 = 91;
    pub const LIST: u32 = 31;
    pub const MENU: u32 = 33;
    pub const SCROLL_BAR: u32 = 48;
    pub const EMBEDDED: u32 = 78;
    pub const SCROLL_PANE: u32 = 49;
    pub const TREE: u32 = 65;
    pub const DOCUMENT_FRAME: u32 = 82;
    pub const DOCUMENT_WEB: u32 = 95;
}

/// AT-SPI state numbers (`AtspiStateType`).
pub mod state {
    pub const ACTIVE: u32 = 1;
    pub const EDITABLE: u32 = 7;
    pub const EXPANDED: u32 = 10;
    pub const SENSITIVE: u32 = 24;
    pub const SHOWING: u32 = 25;
    pub const VISIBLE: u32 = 30;
}

pub const INTERACTIVE_ROLES: &[u32] = &[
    role::PUSH_BUTTON,
    role::TOGGLE_BUTTON,
    role::LINK,
    role::ENTRY,
    role::CHECK_BOX,
    role::RADIO_BUTTON,
    role::COMBO_BOX,
    role::MENU_ITEM,
    role::CHECK_MENU_ITEM,
    role::RADIO_MENU_ITEM,
    role::PAGE_TAB,
    role::LIST_ITEM,
    role::TREE_ITEM,
    role::SPIN_BUTTON,
    role::SLIDER,
    role::PASSWORD_TEXT,
    role::SCROLL_PANE,
    role::LIST,
    role::TREE,
    role::DOCUMENT_WEB,
    role::DOCUMENT_FRAME,
];

/// Scroll containers (design D18): collected with the interactive roles,
/// hinted only by `scroll`.
pub const SCROLL_CONTAINER_ROLES: &[u32] = &[
    role::SCROLL_PANE,
    role::LIST,
    role::TREE,
    role::DOCUMENT_WEB,
    role::DOCUMENT_FRAME,
];

/// Roles whose subtrees are page content (design D19).
pub const WEB_CONTENT_ROLES: &[u32] = &[role::DOCUMENT_WEB, role::DOCUMENT_FRAME, role::EMBEDDED];

/// Canonical libatspi name of a role number, `unknown` when out of range.
/// `atspi-common` 0.14 names 43 `button`; libatspi says `push button`.
pub fn role_name(id: u32) -> &'static str {
    match id {
        role::PUSH_BUTTON => "push button",
        _ => atspi_common::Role::try_from(id).map_or("unknown", |r| r.name()),
    }
}

pub fn is_interactive(role: u32) -> bool {
    INTERACTIVE_ROLES.contains(&role)
}

pub fn is_scroll_container(role: u32) -> bool {
    SCROLL_CONTAINER_ROLES.contains(&role)
}

/// [`is_scroll_container`] for a canonical role name carried on an element.
pub fn is_scroll_container_name(name: &str) -> bool {
    SCROLL_CONTAINER_ROLES.iter().any(|&r| role_name(r) == name)
}

pub fn is_web_content(role: u32) -> bool {
    WEB_CONTENT_ROLES.contains(&role)
}

/// `AtspiCollectionMatchType`.
pub const MATCH_ALL: i32 = 1;
pub const MATCH_ANY: i32 = 2;
/// `AtspiCollectionSortOrder::CANONICAL`.
pub const SORT_CANONICAL: u32 = 1;

/// Pack bit numbers into `words` little-endian `i32` words.
pub fn bitset(bits: &[u32], words: usize) -> Vec<i32> {
    let mut v = vec![0u32; words];
    for &b in bits {
        let (w, o) = ((b / 32) as usize, b % 32);
        if w < words {
            v[w] |= 1 << o;
        }
    }
    v.into_iter().map(|w| w as i32).collect()
}

#[derive(Debug, Clone, PartialEq, Serialize, Type)]
pub struct MatchRule {
    pub states: Vec<i32>,
    pub states_match: i32,
    pub attributes: HashMap<String, String>,
    pub attributes_match: i32,
    pub roles: Vec<i32>,
    pub roles_match: i32,
    pub interfaces: Vec<String>,
    pub interfaces_match: i32,
    pub invert: bool,
}

impl MatchRule {
    /// SHOWING + VISIBLE + SENSITIVE, any interactive role.
    pub fn interactive() -> Self {
        Self {
            states: bitset(&[state::SHOWING, state::VISIBLE, state::SENSITIVE], 2),
            states_match: MATCH_ALL,
            attributes: HashMap::new(),
            attributes_match: MATCH_ALL,
            roles: bitset(INTERACTIVE_ROLES, 4),
            roles_match: MATCH_ANY,
            interfaces: Vec::new(),
            interfaces_match: MATCH_ALL,
            invert: false,
        }
    }
}

/// Bit test on a `GetState` reply (`au`, two words).
pub fn has_state(words: &[u32], s: u32) -> bool {
    words
        .get((s / 32) as usize)
        .is_some_and(|w| w & (1 << (s % 32)) != 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use atspi_common::{Role, State, StateSet};
    use zvariant::serialized::{Context, Format};
    use zvariant::{LE, to_bytes};

    #[test]
    fn signature() {
        assert_eq!(MatchRule::SIGNATURE.to_string(), "(aiia{ss}iaiiasib)");
    }

    #[test]
    fn numbers_agree_with_atspi() {
        for (n, r) in [
            (role::PUSH_BUTTON, Role::Button),
            (role::ENTRY, Role::Entry),
            (role::LINK, Role::Link),
            (role::TREE_ITEM, Role::TreeItem),
            (role::PASSWORD_TEXT, Role::PasswordText),
            (role::LIST_ITEM, Role::ListItem),
        ] {
            assert_eq!(r as u32, n, "{r:?}");
        }
        let want = StateSet::new(State::Showing | State::Visible | State::Sensitive).bits();
        let got = bitset(&[state::SHOWING, state::VISIBLE, state::SENSITIVE], 2);
        assert_eq!(
            u64::from(got[0] as u32) | (u64::from(got[1] as u32) << 32),
            want
        );
    }

    #[test]
    fn fixed_bits() {
        let r = MatchRule::interactive();
        // SENSITIVE 24, SHOWING 25, VISIBLE 30.
        assert_eq!(r.states, vec![0x4300_0000, 0]);
        assert_eq!(r.states_match, 1);
        // word0: 7 8 11 31; word1: 32..62; word2: 65 79 82 88 91 95.
        let w0: u32 = (1 << 7) | (1 << 8) | (1 << 11) | (1 << 31);
        let w1: u32 = [32u32, 35, 37, 40, 43, 44, 45, 49, 51, 52, 62]
            .iter()
            .map(|b| 1u32 << (b - 32))
            .sum();
        let w2: u32 = [65u32, 79, 82, 88, 91, 95]
            .iter()
            .map(|b| 1u32 << (b - 64))
            .sum();
        assert_eq!(r.roles, vec![w0 as i32, w1 as i32, w2 as i32, 0]);
        assert_eq!(r.roles_match, 2);
        assert!(r.attributes.is_empty() && r.interfaces.is_empty() && !r.invert);
    }

    type Wire = (
        Vec<i32>,
        i32,
        HashMap<String, String>,
        i32,
        Vec<i32>,
        i32,
        Vec<String>,
        i32,
        bool,
    );

    #[test]
    fn wire_encoding() {
        let ctx = Context::new(Format::DBus, LE, 0);
        let bytes = to_bytes(ctx, &MatchRule::interactive()).unwrap();
        let (decoded, _): (Wire, _) = bytes.deserialize().unwrap();
        assert_eq!(decoded.0, vec![0x4300_0000, 0]);
        assert_eq!(decoded.1, MATCH_ALL);
        assert_eq!(decoded.4.len(), 4);
        assert_eq!(
            decoded.4[2] as u32,
            (1 << 1) | (1 << 15) | (1 << 18) | (1 << 24) | (1 << 27) | (1 << 31)
        );
        assert_eq!(decoded.5, MATCH_ANY);
        assert!(!decoded.8);
    }

    #[test]
    fn state_bits() {
        let w = [(1u32 << state::SHOWING) | (1 << state::VISIBLE), 0];
        assert!(has_state(&w, state::SHOWING));
        assert!(!has_state(&w, state::EXPANDED));
        assert!(!has_state(&[], state::SHOWING));
    }

    #[test]
    fn canonical_role_names() {
        for (id, name) in [
            (role::PUSH_BUTTON, "push button"),
            (role::LIST_ITEM, "list item"),
            (role::MENU_ITEM, "menu item"),
            (role::COMBO_BOX, "combo box"),
            (role::LIST, "list"),
            (role::MENU, "menu"),
            (role::SCROLL_BAR, "scroll bar"),
            (role::DOCUMENT_WEB, "document web"),
            (role::ENTRY, "entry"),
            (role::PASSWORD_TEXT, "password text"),
        ] {
            assert_eq!(role_name(id), name);
        }
        assert_eq!(role_name(43), "push button");
        assert_eq!(role_name(10_000), "unknown");
    }

    #[test]
    fn role_predicates() {
        assert!(is_interactive(role::PUSH_BUTTON) && !is_interactive(29));
        assert!(!is_interactive(10_000));
        assert!(is_web_content(role::DOCUMENT_WEB) && is_web_content(role::EMBEDDED));
        assert!(is_scroll_container(role::LIST) && !is_scroll_container(role::LIST_ITEM));
        assert!(is_scroll_container_name("scroll pane") && !is_scroll_container_name("list item"));
    }
}
