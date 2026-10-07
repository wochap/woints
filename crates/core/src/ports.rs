//! Port traits implemented by adapter crates (design D1).

use std::future::Future;
use std::pin::Pin;
use std::time::Instant;

use futures_core::Stream;
use serde::{Deserialize, Serialize};

use crate::geom::{Global, OutputGeom, OutputId, Rect};
use crate::model::{ActionKind, Element, Scope, Target};
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

pub type ElementStream<'a> = Pin<Box<dyn Stream<Item = Element> + Send + 'a>>;

/// Produces elements for one target. Streaming and budgeted so a slow
/// provider can be cut off at the deadline with what it found so far.
pub trait ElementProvider {
    fn name(&self) -> &'static str;
    fn collect<'a>(&'a self, target: &'a Target, budget: Budget) -> ElementStream<'a>;
}

/// One badge to draw, already placed, in global logical coordinates.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Badge {
    pub label: String,
    pub rect: Rect<Global>,
    pub output: OutputId,
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
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("action failed: {0}")]
pub struct ActError(pub String);

/// Performs an action on an element (implemented in M2/M3).
pub trait Actuator {
    fn act(
        &self,
        element: &Element,
        action: ActionKind,
    ) -> impl Future<Output = Result<(), ActError>> + Send;
}
