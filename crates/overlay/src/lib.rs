//! Layer-shell overlay (design D5). Runs Wayland on its own thread with
//! calloop; the daemon talks to it through [`OverlayHandle`].

pub mod render;
mod wayland;

use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;

use smithay_client_toolkit::reexports::calloop::channel::{Sender, channel};
use tokio::sync::{mpsc, oneshot};

use woints_core::ports::{CommitTimings, Overlay, OverlayError, OverlayEvent, Scene};

use wayland::Command;

/// Handle to the overlay thread. Dropping it shuts the thread down.
pub struct OverlayHandle {
    commands: Sender<Command>,
    alive: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
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
        Ok(Ok(())) => Ok((
            OverlayHandle {
                commands: cmd_tx,
                alive,
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
