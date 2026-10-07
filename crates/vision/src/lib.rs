//! Vision element provider (milestone 4): capture a target's frame, detect
//! clickable-looking regions with classical CV, hand them to the merge step.

pub mod capture;
pub mod chain;
pub mod detect;
#[cfg(feature = "onnx")]
pub mod onnx;
pub mod policy;
pub mod prep;
pub mod provider;

pub use capture::{CaptureError, CompositeCapture, FrameCapture};
pub use chain::{ChainDetector, NOT_COMPILED, ONNX_COMPILED, OnnxSlot, OnnxStatus, check_config};
pub use detect::{ClassicalDetector, DetectParams, Detector};
pub use policy::{Decision, decide_vision};
pub use provider::VisionProvider;
pub use woints_core::vision::{Detection, DetectionClass, Frame, FramePx};
