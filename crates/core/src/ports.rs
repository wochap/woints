//! Port traits implemented by adapter crates (design D1).

use std::future::Future;
use std::time::Instant;

use serde::{Deserialize, Serialize};

use crate::geom::{Global, OutputGeom, OutputId, Rect};
use crate::model::{Action, Element, Scope, Target, Timings};
use crate::theme::Theme;
use crate::vision::Frame;

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

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CaptureError {
    /// The compositor cannot capture this target (failed, unmapped, protocol missing).
    #[error("capture unavailable: {0}")]
    Unavailable(String),
    #[error("capture timed out")]
    Timeout,
}

/// Captures one target as an RGBA [`Frame`] (design D13). `outputs` is the
/// Hyprland layout used to place region captures.
pub trait FrameCapture {
    fn capture(
        &self,
        target: &Target,
        outputs: &[OutputGeom],
    ) -> impl Future<Output = Result<Frame, CaptureError>> + Send;
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
    /// Drag source pinned in phase 2: `drag_bg`/`drag_fg`.
    Drag,
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
    /// Element came from vision: dashed `hint.vision` border and outline.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vision: Option<Rect<Global>>,
}

/// The mode chip contents (design §05).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Chip {
    pub mode: String,
    /// Dimmed text after the mode (`pick destination`, `hjkl ←↓↑→`).
    #[serde(default)]
    pub sub: Option<String>,
    /// Peach square before the mode (drag phase 2).
    #[serde(default)]
    pub swatch: bool,
    pub typed: String,
    pub scope: Scope,
    pub output: OutputId,
}

/// A rect outlined on one output (drag source, design §07).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Outline {
    pub rect: Rect<Global>,
    pub output: OutputId,
}

/// Scrollbar `Value` of a scroll container: current, minimum, maximum.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ScrollValue {
    pub current: f64,
    pub min: f64,
    pub max: f64,
}

/// Scroll mode frame (design §06): 2 px stroke and an optional rail.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScrollFrame {
    pub rect: Rect<Global>,
    pub output: OutputId,
    pub value: Option<ScrollValue>,
}

/// Everything the overlay needs to draw one frame.
#[derive(Debug, Clone, PartialEq)]
pub struct Scene {
    /// Outputs (Hyprland layout, fractional scale) the badges refer to.
    pub outputs: Vec<OutputGeom>,
    pub badges: Vec<Badge>,
    pub chip: Option<Chip>,
    /// Drag source outline in phase 2.
    pub pinned: Option<Outline>,
    /// Scroll mode frame; badges are hidden while set.
    pub scroll: Option<ScrollFrame>,
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
    /// This actuator cannot perform the action; the pointer actuator may.
    #[error("no actuator for {0}")]
    NeedsPointer(Action),
    /// The element does not support the action (message lists what it does).
    #[error("{0}")]
    Unsupported(String),
    #[error("{0}")]
    Failed(String),
}

/// Performs an action on an element. Actuators are chained: the next one is
/// tried when one returns [`ActError::NeedsPointer`].
pub trait Actuator {
    fn act(
        &self,
        element: &Element,
        action: Action,
    ) -> impl Future<Output = Result<(), ActError>> + Send;
}

/// Protocols the overlay's Wayland connection offers to actuators.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OverlayCaps {
    /// `zwlr_virtual_pointer_manager_v1` bound and a pointer created.
    pub pointer: bool,
    /// Data control protocol name, when advertised.
    pub clipboard: Option<&'static str>,
}
