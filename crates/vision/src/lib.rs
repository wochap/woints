//! Vision element provider (milestone 4): capture a target's frame, detect
//! clickable-looking regions with classical CV, hand them to the merge step.

pub mod capture;
pub mod detect;
pub mod policy;
pub mod provider;

pub use capture::{CaptureError, CompositeCapture, FrameCapture};
pub use detect::{ClassicalDetector, DetectParams, Detector};
pub use policy::{Decision, decide_vision};
pub use provider::VisionProvider;
pub use woints_core::vision::{Detection, DetectionClass, Frame, FramePx};
