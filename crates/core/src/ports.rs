//! Port traits implemented by adapter crates (design D1).

use std::future::Future;
use std::time::Instant;

use serde::{Deserialize, Serialize};

use crate::geom::{Global, OutputGeom, OutputId, Rect};
use crate::model::{ActionKind, Element, Scope, Target, Timings};
use crate::theme::Theme;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SelectError {
    #[error("scope {0} is not supported yet")]
    Unsupported(Scope),
    #[error("compositor query failed: {0}")]
    Backend(String),
}

/// Resolves a [`Scope`] into concrete targets.
pub trait TargetSelector {
    fn select(&self, scope: Scope)
    -> impl Future<Output = Result<Vec<Target>, SelectError>> + Send;
}

/// Time a provider may spend on one target.
#[derive(Debug, Clone, Copy)]
pub struct Budget {
    pub deadline: Instant,
}

impl Budget {
    pub fn expired(&self) -> bool {
        Instant::now() >= self.deadline
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProviderError {
    #[error("provider unavailable: {0}")]
    Unavailable(String),
    #[error("{0}")]
    Other(String),
}

/// How a provider arrived at its elements, for `status` and `doctor`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CollectReport {
    pub provider: String,
    /// Pairing reason: `pid`, `fuzzy`, `no-a11y-app`, `frame-guess`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paired: Option<String>,
    /// Strategy that produced the elements: `collection`, `walk`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub used: Option<String>,
    /// Why a strategy was skipped or nothing was found (`collection-empty`, ...).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reasons: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub toolkit: Option<String>,
    pub truncated: bool,
}

/// Elements for one target plus the provider's own stage timings.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Collected {
    pub elements: Vec<Element>,
    pub report: CollectReport,
    pub timings: Timings,
}

/// Produces elements for one target within `budget`; a provider that runs
/// out of time returns what it has with `report.truncated` set.
pub trait ElementProvider {
    fn name(&self) -> &'static str;
    fn collect(
        &self,
        target: &Target,
        budget: Budget,
    ) -> impl Future<Output = Result<Collected, ProviderError>> + Send;
}

/// How a badge is drawn while the user types.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BadgeStyle {
    #[default]
    Normal,
    /// Selected: `flash_bg`/`flash_fg`.
    Flash,
    /// Eliminated with `hint.eliminated = "fade"`: 20 % alpha.
    Faded,
}

/// One badge to draw, already placed, in global logical coordinates.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Badge {
    pub label: String,
    pub rect: Rect<Global>,
    pub output: OutputId,
    /// Leading glyphs of `label` matched by the typed buffer.
    #[serde(default)]
    pub matched: usize,
    #[serde(default)]
    pub style: BadgeStyle,
}

/// The mode chip contents (design §05).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Chip {
    pub mode: String,
    pub typed: String,
    pub scope: Scope,
    pub output: OutputId,
}

/// Everything the overlay needs to draw one frame.
#[derive(Debug, Clone, PartialEq)]
pub struct Scene {
    /// Outputs (Hyprland layout, fractional scale) the badges refer to.
    pub outputs: Vec<OutputGeom>,
    pub badges: Vec<Badge>,
    pub chip: Option<Chip>,
    pub theme: Theme,
}

/// Per-stage timings reported by the overlay for one show/update.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct CommitTimings {
    pub render_ms: f64,
    pub commit_ms: f64,
}

/// A key press captured while the overlay holds the keyboard.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyPress {
    /// xkb keycode (evdev + 8): identifies the physical key.
    pub keycode: u32,
    pub keysym: u32,
    pub utf8: Option<String>,
    pub mods: Modifiers,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Modifiers {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub logo: bool,
}

/// Events flowing from the overlay to the daemon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OverlayEvent {
    Key(KeyPress),
    /// Escape pressed; the overlay has already unmapped.
    Escaped,
    /// The overlay thread stopped (Wayland disconnect or panic).
    Gone(String),
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OverlayError {
    #[error("overlay unavailable")]
    Unavailable,
    #[error("no such output: {0}")]
    NoOutput(OutputId),
    #[error("{0}")]
    Other(String),
}

/// Draws scenes; key events are delivered on the channel handed out by the
/// adapter's constructor as [`OverlayEvent`]s.
pub trait Overlay {
    /// Map surfaces for every output in `scene` and resolve once committed.
    fn show(
        &self,
        scene: Scene,
    ) -> impl Future<Output = Result<CommitTimings, OverlayError>> + Send;
    /// Redraw already mapped surfaces.
    fn update(
        &self,
        scene: Scene,
    ) -> impl Future<Output = Result<CommitTimings, OverlayError>> + Send;
    /// Unmap and destroy all surfaces.
    fn hide(&self) -> Result<(), OverlayError>;
    /// Unmap and resolve once the compositor has the unmap, so keyboard focus
    /// is back with the application before an action fires.
    fn hide_and_wait(&self) -> impl Future<Output = Result<(), OverlayError>> + Send;
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ActError {
    /// This actuator cannot perform the action; a pointer actuator (M3) may.
    #[error("no actuator for {0}")]
    NeedsPointer(ActionKind),
    #[error("action failed: {0}")]
    Failed(String),
}

/// Performs an action on an element. Actuators are chained: the next one is
/// tried when one returns [`ActError::NeedsPointer`].
pub trait Actuator {
    fn act(
        &self,
        element: &Element,
        action: ActionKind,
    ) -> impl Future<Output = Result<(), ActError>> + Send;
}
