//! Vision as a budgeted element provider (design D16, D20).

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use woints_core::config::VisionConfig;
use woints_core::geom::OutputGeom;
use woints_core::merge::to_elements;
use woints_core::model::{Target, Timings};
use woints_core::ports::{
    Budget, CollectReport, Collected, ElementProvider, FrameCapture, ProviderError,
};

use crate::detect::{DetectParams, Detector};

pub struct VisionProvider<C> {
    capture: C,
    detector: Arc<Mutex<Box<dyn Detector>>>,
    config: VisionConfig,
    /// Layout used by [`ElementProvider::collect`]; [`Self::collect_on`] takes it directly.
    outputs: Mutex<Vec<OutputGeom>>,
}

impl<C: FrameCapture + Sync> VisionProvider<C> {
    pub fn new(capture: C, detector: Box<dyn Detector>, config: VisionConfig) -> Self {
        Self {
            capture,
            detector: Arc::new(Mutex::new(detector)),
            config,
            outputs: Mutex::new(Vec::new()),
        }
    }

    pub fn config(&self) -> &VisionConfig {
        &self.config
    }

    pub fn set_outputs(&self, outputs: Vec<OutputGeom>) {
        *self.outputs.lock().unwrap_or_else(|e| e.into_inner()) = outputs;
    }

    /// The request deadline: `vision.budget_ms` from now.
    pub fn budget(&self) -> Budget {
        Budget {
            deadline: Instant::now() + Duration::from_millis(self.config.budget_ms),
        }
    }

    /// Capture → detect for `target`, recording `capture` and `detect` stages.
    pub async fn collect_on(
        &self,
        target: &Target,
        outputs: &[OutputGeom],
        budget: Budget,
    ) -> Result<Collected, ProviderError> {
        let mut timings = Timings::default();
        let t = Instant::now();
        let frame = self
            .capture
            .capture(target, outputs)
            .await
            .map_err(|e| ProviderError::Unavailable(e.to_string()));
        timings.record("capture", t.elapsed());
        let frame = Arc::new(frame?);

        let t = Instant::now();
        let params = DetectParams::from_config(&self.config, Some(budget.deadline));
        let detector = self.detector.clone();
        let f = frame.clone();
        let detections = tokio::task::spawn_blocking(move || {
            let mut d = detector.lock().unwrap_or_else(|e| e.into_inner());
            d.detect(&f, &params)
        })
        .await
        .map_err(|e| ProviderError::Other(format!("detector: {e}")))?;
        timings.record("detect", t.elapsed());
        let truncated = budget.expired();
        if truncated {
            tracing::warn!("vision_budget_exceeded");
        }
        Ok(Collected {
            elements: to_elements(&detections, &frame),
            report: CollectReport {
                provider: "vision".into(),
                truncated,
                ..Default::default()
            },
            timings,
        })
    }
}

impl<C: FrameCapture + Sync + Send> ElementProvider for VisionProvider<C> {
    fn name(&self) -> &'static str {
        "vision"
    }

    async fn collect(&self, target: &Target, budget: Budget) -> Result<Collected, ProviderError> {
        let outputs = self
            .outputs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        self.collect_on(target, &outputs, budget).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use woints_core::geom::{OutputId, Rect};
    use woints_core::model::{Action, ElementSource, Surface};
    use woints_core::ports::CaptureError;
    use woints_core::vision::{Detection, DetectionClass, Frame};

    struct Blank;

    impl FrameCapture for Blank {
        async fn capture(&self, t: &Target, _: &[OutputGeom]) -> Result<Frame, CaptureError> {
            Ok(Frame {
                rgba: vec![0; 4 * 4 * 4],
                width: 4,
                height: 4,
                rect: t.rect,
                scale: 2.0,
            })
        }
    }

    /// Fixed detections: the pipeline must not care which detector ran.
    struct Fixed;

    impl Detector for Fixed {
        fn detect(&mut self, _: &Frame, _: &DetectParams) -> Vec<Detection> {
            vec![Detection {
                rect_px: Rect::new(20, 10, 40, 20),
                confidence: 0.9,
                class: DetectionClass::Image,
            }]
        }
    }

    #[tokio::test]
    async fn trait_object_swap() {
        let p = VisionProvider::new(Blank, Box::new(Fixed), VisionConfig::default());
        let target = Target {
            surface: Surface::Toplevel {
                address: "0x1".into(),
            },
            rect: Rect::new(100, 100, 200, 100),
            output: OutputId("A".into()),
            pid: None,
            class: "game".into(),
        };
        let c = p.collect(&target, p.budget()).await.unwrap();
        assert_eq!(c.elements.len(), 1);
        let e = &c.elements[0];
        assert_eq!(e.rect, Rect::new(110, 105, 20, 10));
        assert_eq!(e.source, ElementSource::Vision);
        assert_eq!(e.role, "image");
        assert_eq!(e.actions, Action::POINTER.to_vec());
        let stages: Vec<_> = c.timings.0.iter().map(|s| s.stage.as_str()).collect();
        assert_eq!(stages, ["capture", "detect"]);
    }
}
