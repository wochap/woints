//! Shared daemon state and request handlers.

use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::{Mutex, Notify, mpsc};

use woints_core::geom::OutputId;
use woints_core::model::{Scope, Timings};
use woints_core::ports::{Overlay, OverlayError, OverlayEvent};
use woints_core::proto::{
    BenchReply, BenchRow, HintsReply, OutputInfo, Request, Response, SnapshotReply, State,
    StateReply, StatusReply,
};
use woints_core::theme::Theme;
use woints_hypr::{Hypr, Monitor};
use woints_overlay::OverlayHandle;
use woints_overlay::render::{OutputSurface, Renderer};

use crate::pipeline::{self, HyprState};
use crate::session::{Effect, Input, Session};
use crate::store::SnapshotStore;

/// How long `hints` waits for the overlay to commit.
const SHOW_TIMEOUT: Duration = Duration::from_secs(2);

pub struct Daemon {
    pub started: Instant,
    pub theme: Theme,
    hypr: Option<Hypr>,
    hypr_version: Mutex<Option<String>>,
    monitors: Mutex<Option<Vec<Monitor>>>,
    overlay: Mutex<OverlaySlot>,
    overlay_events: mpsc::UnboundedSender<OverlayEvent>,
    session: Mutex<Session>,
    store: Mutex<SnapshotStore>,
    last_timings: Mutex<Timings>,
    renderer: Mutex<Renderer>,
    pub shutdown: Notify,
}

enum OverlaySlot {
    /// Never started or start failed; retry on next `hints`.
    None,
    Running(OverlayHandle),
    /// Thread died; next `hints` answers "overlay unavailable" and a restart is attempted.
    Dead,
}

impl Daemon {
    pub fn new(hypr: Option<Hypr>) -> (Arc<Self>, mpsc::UnboundedReceiver<OverlayEvent>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let d = Arc::new(Self {
            started: Instant::now(),
            theme: Theme::default(),
            hypr,
            hypr_version: Mutex::new(None),
            monitors: Mutex::new(None),
            overlay: Mutex::new(OverlaySlot::None),
            overlay_events: tx,
            session: Mutex::new(Session::Idle),
            store: Mutex::new(SnapshotStore::default()),
            last_timings: Mutex::new(Timings::default()),
            renderer: Mutex::new(Renderer::new()),
            shutdown: Notify::new(),
        });
        (d, rx)
    }

    /// Start the overlay thread and forward its events into the daemon channel.
    pub async fn start_overlay(&self) -> Result<(), OverlayError> {
        let (handle, mut rx) = tokio::task::spawn_blocking(woints_overlay::spawn)
            .await
            .map_err(|e| OverlayError::Other(e.to_string()))??;
        let tx = self.overlay_events.clone();
        tokio::spawn(async move {
            while let Some(ev) = rx.recv().await {
                if tx.send(ev).is_err() {
                    break;
                }
            }
        });
        *self.overlay.lock().await = OverlaySlot::Running(handle);
        tracing::info!("overlay thread running");
        Ok(())
    }

    pub async fn overlay_gone(&self, reason: &str) {
        tracing::error!("overlay unavailable: {reason}");
        let mut slot = self.overlay.lock().await;
        if let OverlaySlot::Running(h) = std::mem::replace(&mut *slot, OverlaySlot::Dead) {
            tokio::task::spawn_blocking(move || drop(h));
        }
        drop(slot);
        self.apply(Input::Escape).await;
    }

    async fn hide_overlay(&self) {
        if let OverlaySlot::Running(h) = &*self.overlay.lock().await {
            let _ = h.hide();
        }
    }

    /// Feed the session FSM and perform its effect.
    pub async fn apply(&self, input: Input) {
        let mut s = self.session.lock().await;
        let (next, effect) = s.transition(input, Instant::now());
        *s = next;
        drop(s);
        if effect == Effect::Hide {
            self.hide_overlay().await;
        }
    }

    pub async fn state(&self) -> State {
        self.session.lock().await.state()
    }

    pub async fn invalidate_monitors(&self) {
        *self.monitors.lock().await = None;
    }

    async fn monitors(&self) -> Result<Vec<Monitor>, String> {
        let mut cache = self.monitors.lock().await;
        if let Some(m) = &*cache {
            return Ok(m.clone());
        }
        let hypr = self.hypr.as_ref().ok_or("not running under Hyprland")?;
        let m = hypr.monitors().await.map_err(|e| e.to_string())?;
        *cache = Some(m.clone());
        Ok(m)
    }

    async fn hyprland_version(&self) -> Option<String> {
        let mut v = self.hypr_version.lock().await;
        if v.is_none() {
            if let Some(h) = &self.hypr {
                *v = h.version().await.ok().map(|v| v.version);
            }
        }
        v.clone()
    }

    /// `hypr.query` stage.
    async fn query(&self, timings: &mut Timings) -> Result<HyprState, String> {
        let t = Instant::now();
        let hypr = self.hypr.as_ref().ok_or("not running under Hyprland")?;
        let monitors = self.monitors().await?;
        let active = hypr.active_window().await.map_err(|e| e.to_string())?;
        let mons: Vec<&Monitor> = monitors.iter().filter(|m| !m.disabled).collect();
        let state = HyprState {
            outputs: mons.iter().map(|m| m.geom()).collect(),
            focused_output: mons
                .iter()
                .find(|m| m.focused)
                .map(|m| OutputId(m.name.clone())),
            active,
        };
        timings.record("hypr.query", t.elapsed());
        Ok(state)
    }

    async fn pipeline(&self, scope: Scope) -> Result<pipeline::Output, String> {
        let mut timings = Timings::default();
        let h = self.query(&mut timings).await?;
        pipeline::run(h, scope, &self.theme, timings)
            .await
            .map_err(|e| e.to_string())
    }

    pub async fn handle(&self, req: Request) -> Response {
        match req {
            Request::Hints { scope, action: _ } => self.hints(scope).await,
            Request::Cancel => {
                self.apply(Input::Cancel).await;
                Response::with(&StateReply { state: State::Idle })
            }
            Request::Status => self.status().await,
            Request::Quit => {
                self.apply(Input::Quit).await;
                self.shutdown.notify_one();
                Response::ok()
            }
            Request::Snapshot { scope } => self.snapshot(scope).await,
            Request::Bench { iterations, scope } => self.bench(iterations, scope).await,
        }
    }

    async fn hints(&self, scope: Scope) -> Response {
        self.apply(Input::HintsRequested).await;
        let mut out = match self.pipeline(scope).await {
            Ok(o) => o,
            Err(e) => return Response::err(e),
        };
        if out.targets.is_empty() {
            return Response::with(&HintsReply {
                state: State::Idle,
                targets: None,
                elements: 0,
            });
        }

        let commit = {
            let mut slot = self.overlay.lock().await;
            match &*slot {
                OverlaySlot::Dead => {
                    *slot = OverlaySlot::None;
                    drop(slot);
                    // Answer this request as unavailable; the next one restarts it.
                    tracing::info!("overlay will be restarted on the next hints request");
                    return Response::err(OverlayError::Unavailable.to_string());
                }
                OverlaySlot::None => {
                    drop(slot);
                    if let Err(e) = self.start_overlay().await {
                        tracing::error!("overlay start failed: {e}");
                        return Response::err(OverlayError::Unavailable.to_string());
                    }
                    slot = self.overlay.lock().await;
                }
                OverlaySlot::Running(_) => {}
            }
            let OverlaySlot::Running(h) = &*slot else {
                return Response::err(OverlayError::Unavailable.to_string());
            };
            tokio::time::timeout(SHOW_TIMEOUT, h.show(out.scene.clone())).await
        };
        let ct = match commit {
            Ok(Ok(ct)) => ct,
            Ok(Err(e)) => return Response::err(e.to_string()),
            Err(_) => {
                self.hide_overlay().await;
                return Response::err("overlay did not commit in time");
            }
        };
        out.timings
            .record("render", Duration::from_secs_f64(ct.render_ms / 1000.0));
        out.timings
            .record("commit", Duration::from_secs_f64(ct.commit_ms / 1000.0));

        let n_targets = out.targets.len();
        let n_elements = out.elements.len();
        let id = self.store.lock().await.insert(out.targets, out.elements);
        *self.last_timings.lock().await = out.timings;
        self.apply(Input::Shown { snapshot_id: id }).await;
        Response::with(&HintsReply {
            state: State::Showing,
            targets: Some(n_targets),
            elements: n_elements,
        })
    }

    async fn snapshot(&self, scope: Scope) -> Response {
        match self.pipeline(scope).await {
            Ok(o) => {
                let id = self
                    .store
                    .lock()
                    .await
                    .insert(o.targets.clone(), o.elements.clone());
                Response::with(&SnapshotReply {
                    snapshot_id: id,
                    targets: o.targets,
                    elements: o.elements,
                    timings: o.timings,
                })
            }
            Err(e) => Response::err(e),
        }
    }

    async fn status(&self) -> Response {
        let outputs = self
            .monitors()
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|m| OutputInfo {
                name: OutputId(m.name.clone()),
                scale: m.scale,
                rect: m.logical_rect(),
                focused: m.focused,
            })
            .collect();
        let overlay =
            matches!(&*self.overlay.lock().await, OverlaySlot::Running(h) if h.is_alive());
        Response::with(&StatusReply {
            state: self.state().await,
            version: env!("CARGO_PKG_VERSION").into(),
            uptime_ms: self.started.elapsed().as_millis() as u64,
            outputs,
            hyprland_version: self.hyprland_version().await,
            overlay,
            last_timings: self.last_timings.lock().await.clone(),
        })
    }

    /// Snapshot + headless render, `iterations` times; never maps a surface.
    async fn bench(&self, iterations: u32, scope: Scope) -> Response {
        let iterations = iterations.clamp(1, 10_000);
        let mut runs: Vec<Timings> = Vec::with_capacity(iterations as usize);
        for _ in 0..iterations {
            // Force a fresh monitor query each run so `hypr.query` is honest.
            self.invalidate_monitors().await;
            let mut o = match self.pipeline(scope).await {
                Ok(o) => o,
                Err(e) => return Response::err(e),
            };
            let t = Instant::now();
            let mut r = self.renderer.lock().await;
            for g in &o.scene.outputs {
                if o.scene.badges.iter().any(|b| b.output == g.id) {
                    let pm = r.render(
                        &o.scene,
                        &OutputSurface {
                            geom: g.clone(),
                            phys_width_mm: 0,
                        },
                    );
                    std::hint::black_box(pm);
                }
            }
            drop(r);
            o.timings.record("render", t.elapsed());
            runs.push(o.timings);
        }
        Response::with(&BenchReply {
            iterations,
            rows: percentiles(&runs),
        })
    }
}

/// Nearest-rank percentile of sorted samples.
pub fn pct(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let rank = ((p / 100.0) * sorted.len() as f64).ceil() as usize;
    sorted[rank.clamp(1, sorted.len()) - 1]
}

/// p50/p95 per stage (in first-seen order) plus `total`.
pub fn percentiles(runs: &[Timings]) -> Vec<BenchRow> {
    let mut stages: Vec<String> = Vec::new();
    for r in runs {
        for s in &r.0 {
            if !stages.contains(&s.stage) {
                stages.push(s.stage.clone());
            }
        }
    }
    let mut rows = Vec::new();
    let mut row = |name: &str, mut v: Vec<f64>| {
        v.sort_by(|a, b| a.total_cmp(b));
        rows.push(BenchRow {
            stage: name.to_owned(),
            p50_ms: pct(&v, 50.0),
            p95_ms: pct(&v, 95.0),
        });
    };
    for s in &stages {
        row(s, runs.iter().filter_map(|r| r.get(s)).collect());
    }
    row("total", runs.iter().map(Timings::total_ms).collect());
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use woints_core::model::StageTiming;

    #[test]
    fn percentile_nearest_rank() {
        let v: Vec<f64> = (1..=20).map(f64::from).collect();
        assert_eq!(pct(&v, 50.0), 10.0);
        assert_eq!(pct(&v, 95.0), 19.0);
        assert_eq!(pct(&[3.0], 95.0), 3.0);
    }

    #[test]
    fn rows_per_stage_and_total() {
        let mk = |a: f64, b: f64| {
            Timings(vec![
                StageTiming {
                    stage: "hypr.query".into(),
                    ms: a,
                },
                StageTiming {
                    stage: "render".into(),
                    ms: b,
                },
            ])
        };
        let rows = percentiles(&[mk(1.0, 2.0), mk(3.0, 4.0)]);
        let names: Vec<_> = rows.iter().map(|r| r.stage.as_str()).collect();
        assert_eq!(names, ["hypr.query", "render", "total"]);
        assert_eq!(rows[2].p95_ms, 7.0);
    }

    #[tokio::test]
    async fn status_works_without_hyprland_or_overlay() {
        let (d, _rx) = Daemon::new(None);
        let r = d.handle(Request::Status).await;
        assert!(r.ok);
        let s: StatusReply = r.payload().unwrap();
        assert_eq!(s.state, State::Idle);
        assert!(!s.overlay);
    }

    #[tokio::test]
    async fn status_works_with_dead_overlay() {
        let (d, _rx) = Daemon::new(None);
        d.overlay_gone("test").await;
        assert!(matches!(&*d.overlay.lock().await, OverlaySlot::Dead));
        let r = d.handle(Request::Status).await;
        assert!(r.ok);
        assert!(!r.payload::<StatusReply>().unwrap().overlay);
    }
}
