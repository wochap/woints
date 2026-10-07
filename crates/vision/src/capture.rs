//! Capture adapters composed per target kind (design D13).

use std::time::Duration;

use woints_core::geom::OutputGeom;
use woints_core::model::{Surface, Target};
pub use woints_core::ports::{CaptureError, FrameCapture};
use woints_core::vision::Frame;

/// Toplevel export for toplevels, region capture for layers and as the
/// fallback when export is unavailable; each attempt bounded by `timeout`.
pub struct CompositeCapture<T, R> {
    pub toplevel: T,
    pub region: R,
    pub timeout: Duration,
}

impl<T: FrameCapture + Sync, R: FrameCapture + Sync> CompositeCapture<T, R> {
    async fn region(&self, target: &Target, outputs: &[OutputGeom]) -> Result<Frame, CaptureError> {
        tokio::time::timeout(self.timeout, self.region.capture(target, outputs))
            .await
            .map_err(|_| CaptureError::Timeout)?
    }
}

impl<T: FrameCapture + Sync, R: FrameCapture + Sync> FrameCapture for CompositeCapture<T, R> {
    async fn capture(
        &self,
        target: &Target,
        outputs: &[OutputGeom],
    ) -> Result<Frame, CaptureError> {
        match target.surface {
            Surface::Layer { .. } => self.region(target, outputs).await,
            Surface::Toplevel { .. } => {
                match tokio::time::timeout(self.timeout, self.toplevel.capture(target, outputs))
                    .await
                {
                    Ok(Ok(f)) => Ok(f),
                    Ok(Err(CaptureError::Unavailable(why))) => {
                        tracing::debug!("toplevel export unavailable ({why}); region capture");
                        self.region(target, outputs).await
                    }
                    Ok(Err(e)) => Err(e),
                    Err(_) => Err(CaptureError::Timeout),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use woints_core::geom::{OutputId, Rect};

    struct Fixed(Result<f32, CaptureError>);

    impl FrameCapture for Fixed {
        async fn capture(&self, t: &Target, _: &[OutputGeom]) -> Result<Frame, CaptureError> {
            self.0.clone().map(|scale| Frame {
                rgba: vec![],
                width: 0,
                height: 0,
                rect: t.rect,
                scale,
            })
        }
    }

    fn target(layer: bool) -> Target {
        Target {
            surface: if layer {
                Surface::Layer {
                    address: "0x2".into(),
                    namespace: "quickshell:bar".into(),
                }
            } else {
                Surface::Toplevel {
                    address: "0x1".into(),
                }
            },
            rect: Rect::new(0, 0, 10, 10),
            output: OutputId("A".into()),
            pid: None,
            class: String::new(),
        }
    }

    fn composite(top: Result<f32, CaptureError>) -> CompositeCapture<Fixed, Fixed> {
        CompositeCapture {
            toplevel: Fixed(top),
            region: Fixed(Ok(2.0)),
            timeout: Duration::from_millis(100),
        }
    }

    #[tokio::test]
    async fn routes_by_kind_and_falls_back() {
        let c = composite(Ok(1.0));
        assert_eq!(c.capture(&target(false), &[]).await.unwrap().scale, 1.0);
        assert_eq!(c.capture(&target(true), &[]).await.unwrap().scale, 2.0);
        let c = composite(Err(CaptureError::Unavailable("failed".into())));
        assert_eq!(c.capture(&target(false), &[]).await.unwrap().scale, 2.0);
        let c = composite(Err(CaptureError::Timeout));
        assert_eq!(
            c.capture(&target(false), &[]).await,
            Err(CaptureError::Timeout)
        );
    }
}
