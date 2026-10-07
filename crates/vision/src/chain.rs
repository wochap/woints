//! `vision.detector` selection (M6 D5) and the long-lived ONNX session slot.

use std::sync::{Arc, Mutex};

use woints_core::config::{DetectorKind, Ep, VisionConfig};
use woints_core::merge::MergeParams;
use woints_core::model::Timings;
use woints_core::vision::{Detection, Frame};

use crate::detect::{ClassicalDetector, DetectParams, Detector};
use crate::prep;

/// Whether this build has the `onnx` feature.
pub const ONNX_COMPILED: bool = cfg!(feature = "onnx");

/// Doctor line for builds without the feature.
pub const NOT_COMPILED: &str =
    "onnx: not compiled in (build with --features woints-vision/onnx or nix build .#woints-onnx)";

/// Config errors that depend on how this crate was built.
pub fn check_config(v: &VisionConfig) -> Result<(), String> {
    if v.detector.uses_onnx() && !ONNX_COMPILED {
        return Err(format!(
            "vision.detector = \"{}\" requires a build with the onnx feature",
            v.detector.as_str()
        ));
    }
    Ok(())
}

pub type SharedDetector = Arc<Mutex<Box<dyn Detector>>>;

/// Facts about the loaded model, for `status` and `doctor`.
#[derive(Debug, Clone, PartialEq)]
pub struct OnnxStatus {
    pub path: String,
    pub input_shape: Vec<i64>,
    pub output_names: Vec<String>,
    pub ep_requested: Ep,
    pub ep_active: Ep,
    pub warmup_ms: f64,
}

/// The ONNX detector across config reloads: rebuilt only when the session
/// key (`model_path`, `ep`, `threads`) changes.
#[derive(Default)]
pub struct OnnxSlot {
    key: Option<(String, Ep, usize)>,
    pub state: Option<Result<(SharedDetector, OnnxStatus), String>>,
}

impl OnnxSlot {
    /// Load, reload or drop the session for `v`.
    pub fn sync(&mut self, v: &VisionConfig) {
        if !v.detector.uses_onnx() || !ONNX_COMPILED {
            self.key = None;
            self.state = None;
            return;
        }
        let key = v.onnx.session_key();
        if self.key.as_ref() == Some(&key) {
            return;
        }
        let reload = self.key.is_some();
        self.key = Some(key);
        self.state = Some(load(v));
        match &self.state {
            Some(Ok(_)) if reload => tracing::info!("onnx: reloaded"),
            Some(Err(e)) => tracing::error!("onnx: {e}"),
            _ => {}
        }
    }

    pub fn detector(&self) -> Option<SharedDetector> {
        match &self.state {
            Some(Ok((d, _))) => Some(d.clone()),
            _ => None,
        }
    }

    /// `status` line: `None` when ONNX is not configured.
    pub fn summary(&self) -> Option<String> {
        match &self.state {
            None => None,
            Some(Ok((_, s))) => Some(format!("loaded {} ep={}", s.path, s.ep_active.as_str())),
            Some(Err(e)) => Some(format!("unavailable ({e})")),
        }
    }
}

#[cfg(feature = "onnx")]
fn load(v: &VisionConfig) -> Result<(SharedDetector, OnnxStatus), String> {
    let d = crate::onnx::OnnxDetector::new(&v.onnx)?;
    let i = d.info();
    let status = OnnxStatus {
        path: i.path.display().to_string(),
        input_shape: i.input_shape.clone(),
        output_names: i.output_names.clone(),
        ep_requested: i.ep_requested,
        ep_active: i.ep_active,
        warmup_ms: i.warmup_ms,
    };
    Ok((Arc::new(Mutex::new(Box::new(d))), status))
}

#[cfg(not(feature = "onnx"))]
fn load(_: &VisionConfig) -> Result<(SharedDetector, OnnxStatus), String> {
    Err("not compiled in".into())
}

/// Runs the detectors `vision.detector` selects; falls back to classical
/// when ONNX is unavailable.
pub struct ChainDetector {
    kind: DetectorKind,
    onnx: Option<SharedDetector>,
    classical: ClassicalDetector,
    merge: MergeParams,
    timings: Timings,
}

impl ChainDetector {
    pub fn new(kind: DetectorKind, onnx: Option<SharedDetector>, merge: MergeParams) -> Self {
        Self {
            kind,
            onnx,
            classical: ClassicalDetector::new(),
            merge,
            timings: Timings::default(),
        }
    }

    /// `status` `detector` line.
    pub fn describe(kind: DetectorKind, onnx_ok: bool) -> String {
        if kind.uses_onnx() && !onnx_ok {
            "classical (onnx unavailable)".into()
        } else {
            kind.as_str().into()
        }
    }

    fn run_onnx(&mut self, frame: &Frame, p: &DetectParams) -> Option<Vec<Detection>> {
        let d = self.onnx.as_ref()?;
        let mut d = d.lock().unwrap_or_else(|e| e.into_inner());
        let out = d.detect(frame, p);
        self.timings.0.extend(d.take_timings().0);
        Some(out)
    }
}

impl Detector for ChainDetector {
    fn detect(&mut self, frame: &Frame, p: &DetectParams) -> Vec<Detection> {
        self.timings = Timings::default();
        if self.kind == DetectorKind::Classical {
            return self.classical.detect(frame, p);
        }
        match self.run_onnx(frame, p) {
            None => self.classical.detect(frame, p),
            Some(o) if self.kind == DetectorKind::Onnx => o,
            Some(o) => {
                let c = self.classical.detect(frame, p);
                prep::chain(o, c, &self.merge)
            }
        }
    }

    fn take_timings(&mut self) -> Timings {
        std::mem::take(&mut self.timings)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use woints_core::geom::Rect;
    use woints_core::vision::{DetectionClass, DetectionSource};

    struct Fixed(Rect<woints_core::vision::FramePx>);

    impl Detector for Fixed {
        fn detect(&mut self, _: &Frame, _: &DetectParams) -> Vec<Detection> {
            vec![Detection {
                rect_px: self.0,
                confidence: 0.7,
                class: DetectionClass::Box,
                source: DetectionSource::Onnx,
            }]
        }
    }

    fn blank() -> Frame {
        Frame {
            rgba: vec![255; 200 * 100 * 4],
            width: 200,
            height: 100,
            rect: Rect::new(0, 0, 200, 100),
            scale: 1.0,
        }
    }

    #[test]
    fn falls_back_to_classical() {
        let mut c = ChainDetector::new(DetectorKind::OnnxClassical, None, MergeParams::default());
        assert!(c.detect(&blank(), &DetectParams::default()).is_empty());
        assert_eq!(
            ChainDetector::describe(DetectorKind::OnnxClassical, false),
            "classical (onnx unavailable)"
        );
    }

    #[test]
    fn onnx_only() {
        let d: SharedDetector = Arc::new(Mutex::new(Box::new(Fixed(Rect::new(1, 2, 30, 40)))));
        let mut c = ChainDetector::new(DetectorKind::Onnx, Some(d), MergeParams::default());
        let out = c.detect(&blank(), &DetectParams::default());
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].source, DetectionSource::Onnx);
    }

    #[test]
    fn default_build_rejects_onnx() {
        let v = VisionConfig {
            detector: DetectorKind::Onnx,
            ..VisionConfig::default()
        };
        if ONNX_COMPILED {
            assert!(check_config(&v).is_ok());
        } else {
            assert_eq!(
                check_config(&v).unwrap_err(),
                "vision.detector = \"onnx\" requires a build with the onnx feature"
            );
        }
    }

    #[test]
    fn slot_missing_model() {
        let mut s = OnnxSlot::default();
        let mut v = VisionConfig {
            detector: DetectorKind::Onnx,
            ..VisionConfig::default()
        };
        v.onnx.model_path = "/nonexistent/m.onnx".into();
        s.sync(&v);
        if ONNX_COMPILED {
            assert_eq!(
                s.summary().unwrap(),
                "unavailable (model not found: /nonexistent/m.onnx)"
            );
        } else {
            assert!(s.summary().is_none());
        }
        assert!(s.detector().is_none());
    }
}
