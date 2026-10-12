//! Actuation types: pointer backend port, element-to-actuator routing,
//! modifier mapping and the `[input]` configuration (design D13–D20).

use std::future::Future;

use serde::{Deserialize, Serialize};

use crate::geom::{Global, OutputId, Point};
use crate::model::{Action, AtspiCap, Element};
use crate::ports::{ActError, Modifiers};

/// Linux input button codes.
pub const BTN_LEFT: u32 = 272;
pub const BTN_RIGHT: u32 = 273;
pub const BTN_MIDDLE: u32 = 274;

/// Logical pixels per scrolled line.
pub const LINE_PX: f64 = 15.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ButtonState {
    Released,
    Pressed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Axis {
    #[default]
    Vertical,
    Horizontal,
}

impl std::str::FromStr for Axis {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "vertical" | "v" => Ok(Axis::Vertical),
            "horizontal" | "h" => Ok(Axis::Horizontal),
            _ => Err(format!("unknown axis \"{s}\"")),
        }
    }
}

/// Raw `zwlr_virtual_pointer_v1` operations. Every call sends the event
/// followed by `frame`; [`VirtualPointer::roundtrip`] resolves once the
/// compositor has processed everything sent before it.
pub trait VirtualPointer {
    fn button(
        &self,
        code: u32,
        state: ButtonState,
    ) -> impl Future<Output = Result<(), ActError>> + Send;
    fn motion(&self, dx: f64, dy: f64) -> impl Future<Output = Result<(), ActError>> + Send;
    fn axis(
        &self,
        axis: Axis,
        px: f64,
        lines: i32,
    ) -> impl Future<Output = Result<(), ActError>> + Send;
    /// Output-local `motion_absolute` on a pointer bound to `output`.
    fn motion_absolute(
        &self,
        output: &OutputId,
        x: u32,
        y: u32,
        w: u32,
        h: u32,
    ) -> impl Future<Output = Result<(), ActError>> + Send;
    fn roundtrip(&self) -> impl Future<Output = Result<(), ActError>> + Send;
}

/// Places the cursor and sends pointer events (design D13). Event methods
/// send the event plus `frame`; the actuator calls [`PointerBackend::roundtrip`]
/// after each.
pub trait PointerBackend {
    fn move_to(&self, p: Point<Global>) -> impl Future<Output = Result<(), ActError>> + Send;
    fn cursor_pos(&self) -> impl Future<Output = Result<Point<Global>, ActError>> + Send;
    fn button(
        &self,
        code: u32,
        state: ButtonState,
    ) -> impl Future<Output = Result<(), ActError>> + Send;
    fn motion_rel(&self, dx: f64, dy: f64) -> impl Future<Output = Result<(), ActError>> + Send;
    fn axis(
        &self,
        axis: Axis,
        px: f64,
        lines: i32,
    ) -> impl Future<Output = Result<(), ActError>> + Send;
    fn roundtrip(&self) -> impl Future<Output = Result<(), ActError>> + Send;
}

/// `input.pointer`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PointerKind {
    #[default]
    Movecursor,
    Absolute,
    /// Pointer actions disabled.
    None,
}

impl PointerKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            PointerKind::Movecursor => "movecursor",
            PointerKind::Absolute => "absolute",
            PointerKind::None => "none",
        }
    }
}

/// `input.prefer`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Prefer {
    #[default]
    Atspi,
    Pointer,
}

/// Which actuator performs an action on an element.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActuatorKind {
    /// AT-SPI `DoAction`.
    DoAction,
    /// AT-SPI `GrabFocus`; a `false` reply falls back to a pointer click.
    GrabFocus,
    /// AT-SPI `Text.GetText`, then the clipboard.
    ReadText,
    Pointer,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ActuatorRouting {
    pub prefer: Prefer,
}

/// `element N does not support A; supported: ...`.
pub fn unsupported(e: &Element, a: Action) -> String {
    let list: Vec<&str> = e.actions.iter().map(Action::as_str).collect();
    format!(
        "element {} does not support {a}; supported: {}",
        e.id.0,
        list.join(",")
    )
}

impl ActuatorRouting {
    /// Pick the actuator for `action` on `e` (design D16).
    pub fn route(&self, e: &Element, action: Action) -> Result<ActuatorKind, ActError> {
        if !e.actions.contains(&action) {
            return Err(ActError::Unsupported(unsupported(e, action)));
        }
        let atspi = !e.caps.is_empty() && self.prefer == Prefer::Atspi;
        Ok(match action {
            Action::Click if atspi => match e.caps.first() {
                Some(AtspiCap::GrabFocus) => ActuatorKind::GrabFocus,
                _ if e.caps.contains(&AtspiCap::DoAction) => ActuatorKind::DoAction,
                _ => ActuatorKind::Pointer,
            },
            Action::Focus if atspi => ActuatorKind::GrabFocus,
            Action::Copy => ActuatorKind::ReadText,
            _ => ActuatorKind::Pointer,
        })
    }
}

/// `[modifiers]`: action selected by a modifier held on the final glyph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModifierMap {
    pub shift: Option<Action>,
    pub ctrl: Option<Action>,
    pub alt: Option<Action>,
    pub super_: Option<Action>,
}

impl Default for ModifierMap {
    fn default() -> Self {
        Self {
            shift: Some(Action::Right),
            ctrl: Some(Action::Middle),
            alt: Some(Action::Hover),
            super_: None,
        }
    }
}

impl ModifierMap {
    /// Apply `key = value` pairs over the defaults; values are action names
    /// or `"none"`.
    pub fn from_pairs<'a>(
        pairs: impl IntoIterator<Item = (&'a str, &'a str)>,
    ) -> Result<Self, String> {
        let mut m = Self::default();
        for (k, v) in pairs {
            let a = match v {
                "none" => None,
                _ => Some(
                    v.parse::<Action>()
                        .map_err(|e| format!("modifiers.{k}: {e}"))?,
                ),
            };
            match k {
                "shift" => m.shift = a,
                "ctrl" => m.ctrl = a,
                "alt" => m.alt = a,
                "super" => m.super_ = a,
                _ => return Err(format!("modifiers.{k}: unknown modifier")),
            }
        }
        Ok(m)
    }

    /// Action mapped by the held modifiers (shift, ctrl, alt, super order).
    pub fn mapped(&self, mods: Modifiers) -> Option<Action> {
        [
            (mods.shift, self.shift),
            (mods.ctrl, self.ctrl),
            (mods.alt, self.alt),
            (mods.logo, self.super_),
        ]
        .into_iter()
        .find_map(|(held, a)| if held { a } else { None })
    }

    /// Final action for a completed label (design D20): an explicit session
    /// action wins over modifiers.
    pub fn resolve(&self, session: Action, explicit: bool, mods: Modifiers) -> Action {
        match self.mapped(mods) {
            Some(a) if !explicit => a,
            Some(a) => {
                tracing::debug!(%session, ignored = %a, "explicit action wins over modifier");
                session
            }
            None => session,
        }
    }
}

/// `input.keys`: where session keys come from.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum KeysChoice {
    /// The Hyprland submap when it installs, else the overlay.
    #[default]
    Auto,
    Submap,
    Overlay,
}

/// Name of the Hyprland submap woints defines (`input.submap`).
pub const DEFAULT_SUBMAP: &str = "woints-keys";

/// `[input]` and `actions.default`, with their defaults.
#[derive(Debug, Clone, PartialEq)]
pub struct InputConfig {
    pub pointer: PointerKind,
    pub prefer: Prefer,
    pub restore_cursor: bool,
    pub double_click_gap_ms: u32,
    pub drag_steps: u32,
    pub scroll_step: u32,
    pub unmap_timeout_ms: u32,
    pub modifiers: ModifierMap,
    pub default_action: Action,
    pub keys: KeysChoice,
    pub submap: String,
}

impl Default for InputConfig {
    fn default() -> Self {
        Self {
            pointer: PointerKind::Movecursor,
            prefer: Prefer::Atspi,
            restore_cursor: false,
            double_click_gap_ms: 40,
            drag_steps: 8,
            scroll_step: 3,
            unmap_timeout_ms: 200,
            modifiers: ModifierMap::default(),
            default_action: Action::Click,
            keys: KeysChoice::Auto,
            submap: DEFAULT_SUBMAP.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::Rect;
    use crate::model::{ElementId, ElementSource};

    fn el(caps: Vec<AtspiCap>, role: &str) -> Element {
        Element {
            id: ElementId(3),
            rect: Rect::new(0, 0, 10, 10),
            role: role.into(),
            name: String::new(),
            source: ElementSource::Atspi,
            actions: Action::supported(&caps),
            caps,
            confidence: 1.0,
            handle: None,
        }
    }

    #[test]
    fn gtk3_button() {
        let r = ActuatorRouting::default();
        let b = el(vec![AtspiCap::DoAction], "push button");
        assert_eq!(r.route(&b, Action::Click), Ok(ActuatorKind::DoAction));
        assert_eq!(r.route(&b, Action::Right), Ok(ActuatorKind::Pointer));
        assert_eq!(r.route(&b, Action::Copy), Ok(ActuatorKind::ReadText));
    }

    #[test]
    fn vision_element() {
        let r = ActuatorRouting::default();
        let v = el(vec![], "vision");
        assert_eq!(r.route(&v, Action::Click), Ok(ActuatorKind::Pointer));
        assert_eq!(
            r.route(&v, Action::Focus).unwrap_err().to_string(),
            "element 3 does not support focus; supported: click,right,middle,double,hover,drag,scroll"
        );
    }

    #[test]
    fn entry_focus() {
        let r = ActuatorRouting::default();
        let e = el(vec![AtspiCap::GrabFocus, AtspiCap::DoAction], "entry");
        assert_eq!(r.route(&e, Action::Focus), Ok(ActuatorKind::GrabFocus));
        assert_eq!(r.route(&e, Action::Click), Ok(ActuatorKind::GrabFocus));
        let a = el(vec![AtspiCap::Accessible], "list item");
        assert_eq!(r.route(&a, Action::Click), Ok(ActuatorKind::Pointer));
    }

    #[test]
    fn prefer_pointer() {
        let r = ActuatorRouting {
            prefer: Prefer::Pointer,
        };
        let b = el(vec![AtspiCap::DoAction], "push button");
        assert_eq!(r.route(&b, Action::Click), Ok(ActuatorKind::Pointer));
        assert_eq!(r.route(&b, Action::Focus), Ok(ActuatorKind::Pointer));
    }

    fn mods(shift: bool, ctrl: bool, alt: bool, logo: bool) -> Modifiers {
        Modifiers {
            ctrl,
            alt,
            shift,
            logo,
        }
    }

    #[test]
    fn modifier_defaults() {
        let m = ModifierMap::default();
        let none = Modifiers::default();
        assert_eq!(m.resolve(Action::Click, false, none), Action::Click);
        let shift = mods(true, false, false, false);
        assert_eq!(m.resolve(Action::Click, false, shift), Action::Right);
        let ctrl = mods(false, true, false, false);
        assert_eq!(m.resolve(Action::Click, false, ctrl), Action::Middle);
        let alt = mods(false, false, true, false);
        assert_eq!(m.resolve(Action::Click, false, alt), Action::Hover);
        let logo = mods(false, false, false, true);
        assert_eq!(m.resolve(Action::Click, false, logo), Action::Click);
    }

    #[test]
    fn explicit_wins() {
        let m = ModifierMap::default();
        let shift = mods(true, false, false, false);
        assert_eq!(m.resolve(Action::Drag, true, shift), Action::Drag);
    }

    #[test]
    fn remap_and_validate() {
        let m = ModifierMap::from_pairs([("alt", "double"), ("shift", "none")]).unwrap();
        assert_eq!(
            m.resolve(Action::Click, false, mods(false, false, true, false)),
            Action::Double
        );
        assert_eq!(m.shift, None);
        assert_eq!(
            ModifierMap::from_pairs([("ctrl", "teleport")]).unwrap_err(),
            "modifiers.ctrl: unknown action \"teleport\""
        );
    }

    #[test]
    fn input_defaults() {
        let c = InputConfig::default();
        assert_eq!(c.pointer, PointerKind::Movecursor);
        assert_eq!(
            (c.drag_steps, c.scroll_step, c.unmap_timeout_ms),
            (8, 3, 200)
        );
        assert_eq!(c.default_action, Action::Click);
        assert_eq!("v".parse::<Axis>(), Ok(Axis::Vertical));
    }
}
