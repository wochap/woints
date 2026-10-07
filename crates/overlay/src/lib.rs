//! Layer-shell overlay (design D5). Runs Wayland on its own thread with
//! calloop; the daemon talks to it through [`OverlayHandle`].

pub mod capture;
pub mod render;
mod wayland;

use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;

use smithay_client_toolkit::reexports::calloop::channel::{Sender, channel};
use tokio::sync::{mpsc, oneshot};

use woints_core::act::{Axis, ButtonState, VirtualPointer};
use woints_core::geom::{OutputGeom, OutputId, global_to_output, pick_output};
use woints_core::model::{Surface, Target};
use woints_core::ports::{
    ActError, CaptureError, CommitTimings, FrameCapture, Overlay, OverlayCaps, OverlayError,
    OverlayEvent, Scene,
};
use woints_core::vision::Frame;

use capture::{CaptureReq, toplevel_handle};
use wayland::{Command, PointerOp};

/// Largest relative deviation of a toplevel frame's scale from its output's.
pub const SCALE_TOLERANCE: f32 = 0.02;

/// Handle to the overlay thread. Dropping it shuts the thread down.
pub struct OverlayHandle {
    commands: Sender<Command>,
    alive: Arc<AtomicBool>,
    caps: OverlayCaps,
    thread: Option<JoinHandle<()>>,
}

/// Cloneable sender for the overlay thread's virtual pointer and clipboard.
#[derive(Clone)]
pub struct PointerLink {
    commands: Sender<Command>,
    alive: Arc<AtomicBool>,
}

impl PointerLink {
    async fn op(&self, op: PointerOp) -> Result<(), ActError> {
        if !self.alive.load(Ordering::SeqCst) {
            return Err(ActError::Failed("overlay unavailable".into()));
        }
        let (tx, rx) = oneshot::channel();
        self.commands
            .send(Command::Pointer(op, tx))
            .map_err(|_| ActError::Failed("overlay unavailable".into()))?;
        rx.await
            .map_err(|_| ActError::Failed("overlay unavailable".into()))?
            .map_err(ActError::Failed)
    }

    /// Set the clipboard to `text` through data control.
    pub async fn set_clipboard(&self, text: String) -> Result<(), String> {
        let (tx, rx) = oneshot::channel();
        self.commands
            .send(Command::Clipboard(text, tx))
            .map_err(|_| "overlay unavailable".to_owned())?;
        rx.await.map_err(|_| "overlay unavailable".to_owned())?
    }
}

impl VirtualPointer for PointerLink {
    async fn button(&self, code: u32, state: ButtonState) -> Result<(), ActError> {
        self.op(PointerOp::Button(code, state)).await
    }
    async fn motion(&self, dx: f64, dy: f64) -> Result<(), ActError> {
        self.op(PointerOp::Motion(dx, dy)).await
    }
    async fn axis(&self, axis: Axis, px: f64, lines: i32) -> Result<(), ActError> {
        self.op(PointerOp::Axis(axis, px, lines)).await
    }
    async fn motion_absolute(
        &self,
        output: &OutputId,
        x: u32,
        y: u32,
        w: u32,
        h: u32,
    ) -> Result<(), ActError> {
        self.op(PointerOp::Absolute {
            output: output.clone(),
            x,
            y,
            w,
            h,
        })
        .await
    }
    async fn roundtrip(&self) -> Result<(), ActError> {
        self.op(PointerOp::Roundtrip).await
    }
}

/// Cloneable sender for frame captures on the overlay thread.
#[derive(Clone)]
pub struct CaptureLink {
    commands: Sender<Command>,
    alive: Arc<AtomicBool>,
}

impl CaptureLink {
    async fn request(&self, req: CaptureReq) -> Result<Frame, CaptureError> {
        let gone = || CaptureError::Unavailable("overlay unavailable".into());
        if !self.alive.load(Ordering::SeqCst) {
            return Err(gone());
        }
        let (tx, rx) = oneshot::channel();
        self.commands
            .send(Command::Capture(req, tx))
            .map_err(|_| gone())?;
        rx.await.map_err(|_| gone())?
    }
}

/// One toplevel via `hyprland_toplevel_export_v1`, cursor excluded.
#[derive(Clone)]
pub struct ToplevelExportCapture(pub CaptureLink);

impl FrameCapture for ToplevelExportCapture {
    async fn capture(
        &self,
        target: &Target,
        outputs: &[OutputGeom],
    ) -> Result<Frame, CaptureError> {
        let Surface::Toplevel { address } = &target.surface else {
            return Err(CaptureError::Unavailable("not a toplevel".into()));
        };
        let handle = toplevel_handle(address)
            .ok_or_else(|| CaptureError::Unavailable(format!("bad address {address}")))?;
        let frame = self
            .0
            .request(CaptureReq::Toplevel {
                handle,
                rect: target.rect,
            })
            .await?;
        if let Some(o) = outputs.iter().find(|o| o.id == target.output) {
            let dev = (frame.scale - o.scale).abs() / o.scale.max(f32::EPSILON);
            if dev > SCALE_TOLERANCE {
                // TODO(verify): decorations or shadow in export frames.
                tracing::warn!(
                    frame_w = frame.width,
                    frame_h = frame.height,
                    rect = ?target.rect,
                    frame_scale = frame.scale,
                    output_scale = o.scale,
                    "TODO(verify): toplevel export size differs from logical × scale"
                );
                return Err(CaptureError::Unavailable("export scale mismatch".into()));
            }
        }
        Ok(frame)
    }
}

/// A global rect via `zwlr_screencopy_v1.capture_output_region` on the output
/// holding its centre, cursor excluded.
#[derive(Clone)]
pub struct ScreencopyRegionCapture(pub CaptureLink);

impl FrameCapture for ScreencopyRegionCapture {
    async fn capture(
        &self,
        target: &Target,
        outputs: &[OutputGeom],
    ) -> Result<Frame, CaptureError> {
        self.0.request(region_req(target, outputs)?).await
    }
}

/// Region request for `target` on the output holding its centre.
pub fn region_req(target: &Target, outputs: &[OutputGeom]) -> Result<CaptureReq, CaptureError> {
    let id = pick_output(target.rect, outputs, Some(&target.output))
        .ok_or_else(|| CaptureError::Unavailable("no output".into()))?;
    let out = outputs
        .iter()
        .find(|o| o.id == id)
        .ok_or_else(|| CaptureError::Unavailable(format!("unknown output {id}")))?;
    Ok(CaptureReq::Region {
        output: id.clone(),
        local: global_to_output(target.rect, out),
        rect: target.rect,
    })
}

/// Start the overlay thread. Returns the handle and the event receiver
/// (keys, Escape, thread death).
pub fn spawn() -> Result<(OverlayHandle, mpsc::UnboundedReceiver<OverlayEvent>), OverlayError> {
    let (cmd_tx, cmd_rx) = channel::<Command>();
    let (ev_tx, ev_rx) = mpsc::unbounded_channel();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let alive = Arc::new(AtomicBool::new(true));
    let alive_t = alive.clone();
    let thread = std::thread::Builder::new()
        .name("woints-overlay".into())
        .spawn(move || {
            let gone_tx = ev_tx.clone();
            let ready_fallback = ready_tx.clone();
            let res = std::panic::catch_unwind(AssertUnwindSafe(|| {
                wayland::run(cmd_rx, ev_tx, ready_tx)
            }));
            alive_t.store(false, Ordering::SeqCst);
            let reason = match res {
                Ok(Ok(())) => "overlay stopped".to_owned(),
                Ok(Err(e)) => e,
                Err(p) => {
                    let msg = p
                        .downcast_ref::<&str>()
                        .map(|s| (*s).to_owned())
                        .or_else(|| p.downcast_ref::<String>().cloned())
                        .unwrap_or_else(|| "unknown panic".into());
                    format!("overlay thread panicked: {msg}")
                }
            };
            tracing::error!("{reason}");
            let _ = ready_fallback.send(Err(reason.clone()));
            let _ = gone_tx.send(OverlayEvent::Gone(reason));
        })
        .map_err(|e| OverlayError::Other(e.to_string()))?;
    match ready_rx.recv() {
        Ok(Ok(caps)) => Ok((
            OverlayHandle {
                commands: cmd_tx,
                alive,
                caps,
                thread: Some(thread),
            },
            ev_rx,
        )),
        Ok(Err(e)) => Err(OverlayError::Other(e)),
        Err(_) => Err(OverlayError::Unavailable),
    }
}

impl OverlayHandle {
    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }

    /// Virtual pointer and clipboard availability, fixed at start-up.
    pub fn caps(&self) -> OverlayCaps {
        self.caps
    }

    pub fn capture_link(&self) -> CaptureLink {
        CaptureLink {
            commands: self.commands.clone(),
            alive: self.alive.clone(),
        }
    }

    pub fn pointer_link(&self) -> PointerLink {
        PointerLink {
            commands: self.commands.clone(),
            alive: self.alive.clone(),
        }
    }

    async fn send_show(&self, scene: Scene) -> Result<CommitTimings, OverlayError> {
        if !self.is_alive() {
            return Err(OverlayError::Unavailable);
        }
        let (tx, rx) = oneshot::channel();
        self.commands
            .send(Command::Show {
                scene: Box::new(scene),
                reply: tx,
            })
            .map_err(|_| OverlayError::Unavailable)?;
        rx.await.map_err(|_| OverlayError::Unavailable)?
    }
}

impl Overlay for OverlayHandle {
    async fn show(&self, scene: Scene) -> Result<CommitTimings, OverlayError> {
        self.send_show(scene).await
    }

    async fn update(&self, scene: Scene) -> Result<CommitTimings, OverlayError> {
        self.send_show(scene).await
    }

    fn hide(&self) -> Result<(), OverlayError> {
        self.commands
            .send(Command::Hide)
            .map_err(|_| OverlayError::Unavailable)
    }

    async fn hide_and_wait(&self) -> Result<(), OverlayError> {
        if !self.is_alive() {
            return Err(OverlayError::Unavailable);
        }
        let (tx, rx) = oneshot::channel();
        self.commands
            .send(Command::HideAck(tx))
            .map_err(|_| OverlayError::Unavailable)?;
        rx.await.map_err(|_| OverlayError::Unavailable)
    }
}

impl Drop for OverlayHandle {
    fn drop(&mut self) {
        let _ = self.commands.send(Command::Shutdown);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use woints_core::geom::{Point, Rect, Size};

    #[test]
    fn region_on_negative_origin_output() {
        let outputs = [
            OutputGeom {
                id: OutputId("A".into()),
                origin: Point::new(-1920, 0),
                logical_size: Size { w: 1920, h: 1080 },
                scale: 1.0,
            },
            OutputGeom {
                id: OutputId("B".into()),
                origin: Point::new(0, 0),
                logical_size: Size { w: 1440, h: 900 },
                scale: 2.0,
            },
        ];
        let target = Target {
            surface: Surface::Layer {
                address: "0x1".into(),
                namespace: "bar".into(),
            },
            rect: Rect::new(-1800, 100, 400, 200),
            output: OutputId("B".into()),
            pid: None,
            class: String::new(),
        };
        assert_eq!(
            region_req(&target, &outputs).unwrap(),
            CaptureReq::Region {
                output: OutputId("A".into()),
                local: Rect::new(120, 100, 400, 200),
                rect: target.rect,
            }
        );
    }
}
