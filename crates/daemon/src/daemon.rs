//! Shared daemon state and request handlers.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::{Mutex, Notify, mpsc};

use woints_a11y::{A11yConnection, AtspiActuator, AtspiProvider};
use woints_core::geom::OutputId;
use woints_core::model::{ActionKind, Element, ElementId, Scope, Target, Timings};
use woints_core::ports::{
    ActError, Actuator, Budget, CollectReport, Collected, ElementProvider, KeyPress, Overlay,
    OverlayError, OverlayEvent, ProviderError, Scene,
};
use woints_core::proto::{
    ActReply, ActionOutcome, BenchReply, BenchRow, HintsReply, OutputInfo, ProviderStatus, Request,
    Response, SnapshotReply, State, StateReply, StatusReply,
};
use woints_core::theme::{Eliminated, Theme};
use woints_hypr::{Hypr, Monitor};
use woints_overlay::OverlayHandle;
use woints_overlay::render::{OutputSurface, Renderer};

use crate::pipeline::{self, DemoProvider, HyprState};
use crate::session::{Effect, Input, Session};
use crate::store::SnapshotStore;

/// Hard cap on the selection flash (design §05).
const FLASH_CAP_MS: u32 = 120;
/// How long an action waits for the overlay unmap to commit.
const UNMAP_TIMEOUT: Duration = Duration::from_millis(500);
/// xkb `BackSpace`.
const KEYSYM_BACKSPACE: u32 = 0xff08;

/// The element provider in use: AT-SPI, or the demo boxes with `WOINTS_DEMO=1`.
pub enum Provider {
    Demo(DemoProvider),
    Atspi(AtspiProvider),
}

impl ElementProvider for Provider {
    fn name(&self) -> &'static str {
        match self {
            Provider::Demo(p) => p.name(),
            Provider::Atspi(p) => p.name(),
        }
    }

    async fn collect(&self, target: &Target, budget: Budget) -> Result<Collected, ProviderError> {
        match self {
            Provider::Demo(p) => p.collect(target, budget).await,
            Provider::Atspi(p) => p.collect(target, budget).await,
        }
    }
}

/// The scene currently on screen, kept so key presses can redraw it.
struct Shown {
    scene: Scene,
    labels: Vec<String>,
}

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
    last_collect: Mutex<Vec<CollectReport>>,
    last_action: Mutex<Option<ActionOutcome>>,
    renderer: Mutex<Renderer>,
    provider: Provider,
    actuator: Option<AtspiActuator>,
    shown: Mutex<Option<Shown>>,
    /// Keycode → label glyph, learned from the overlay's key reports.
    keymap: Mutex<HashMap<u32, char>>,
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
        let (provider, actuator) = if std::env::var("WOINTS_DEMO").as_deref() == Ok("1") {
            tracing::info!("WOINTS_DEMO=1: using the demo provider");
            (Provider::Demo(DemoProvider), None)
        } else {
            let conn = Arc::new(A11yConnection::new());
            let p = AtspiProvider::new(conn.clone());
            let a = AtspiActuator::new(conn, p.apps_cache());
            (Provider::Atspi(p), Some(a))
        };
        Self::with_provider(hypr, provider, actuator)
    }

    pub fn with_provider(
        hypr: Option<Hypr>,
        provider: Provider,
        actuator: Option<AtspiActuator>,
    ) -> (Arc<Self>, mpsc::UnboundedReceiver<OverlayEvent>) {
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
            last_collect: Mutex::new(Vec::new()),
            last_action: Mutex::new(None),
            renderer: Mutex::new(Renderer::new()),
            provider,
            actuator,
            shown: Mutex::new(None),
            keymap: Mutex::new(HashMap::new()),
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

    async fn update_overlay(&self, scene: Scene) {
        if let OverlaySlot::Running(h) = &*self.overlay.lock().await {
            if let Err(e) = h.update(scene).await {
                tracing::warn!("overlay update: {e}");
            }
        }
    }

    async fn hide_overlay_and_wait(&self) {
        if let OverlaySlot::Running(h) = &*self.overlay.lock().await {
            match tokio::time::timeout(UNMAP_TIMEOUT, h.hide_and_wait()).await {
                Ok(Ok(())) => {}
                Ok(Err(e)) => tracing::warn!("overlay unmap: {e}"),
                Err(_) => tracing::warn!("overlay unmap not confirmed in time"),
            }
        }
    }

    fn flash_ms(&self) -> u32 {
        self.theme.timing.flash_ms.min(FLASH_CAP_MS)
    }

    /// Feed the session FSM and perform its effects in order.
    pub async fn apply(&self, input: Input) {
        let mut s = self.session.lock().await;
        let (next, effects) = s.transition(input, Instant::now(), self.flash_ms());
        *s = next;
        drop(s);
        let acting = effects.iter().any(|e| matches!(e, Effect::Act { .. }));
        for e in effects {
            match e {
                Effect::Redraw(f) => {
                    let scene = {
                        let shown = self.shown.lock().await;
                        let Some(sh) = shown.as_ref() else { continue };
                        let gone: Vec<&str> = f
                            .eliminated
                            .iter()
                            .filter_map(|&i| sh.labels.get(i).map(String::as_str))
                            .collect();
                        let fade = self.theme.hint.eliminated == Eliminated::Fade;
                        pipeline::filter_scene(&sh.scene, &f.typed, &gone, fade, None)
                    };
                    self.update_overlay(scene).await;
                }
                Effect::Flash(i) => {
                    let scene = {
                        let shown = self.shown.lock().await;
                        let Some(sh) = shown.as_ref() else { continue };
                        let label = sh.labels.get(i).cloned().unwrap_or_default();
                        pipeline::filter_scene(&sh.scene, &label, &[], false, Some(&label))
                    };
                    self.update_overlay(scene).await;
                    tokio::time::sleep(Duration::from_millis(u64::from(self.flash_ms()))).await;
                }
                Effect::Hide => {
                    *self.shown.lock().await = None;
                    if acting {
                        self.hide_overlay_and_wait().await;
                    } else {
                        self.hide_overlay().await;
                    }
                }
                Effect::Act {
                    snapshot_id,
                    index,
                    action,
                } => {
                    let element = self
                        .store
                        .lock()
                        .await
                        .get(snapshot_id)
                        .and_then(|s| s.elements.get(index).cloned());
                    match element {
                        Some(e) => {
                            let _ = self.act_on(&e, action).await;
                        }
                        None => tracing::warn!(snapshot_id, index, "selected element vanished"),
                    }
                }
            }
        }
    }

    /// Map an overlay key press to a session input. Glyphs are learned per
    /// keycode, so the glyph shown and the key pressed agree under any layout.
    pub async fn key_input(&self, k: &KeyPress) -> Option<Input> {
        if k.keysym == KEYSYM_BACKSPACE {
            return Some(Input::Backspace);
        }
        if k.mods.ctrl || k.mods.alt || k.mods.logo {
            return None;
        }
        let mut map = self.keymap.lock().await;
        let typed = k
            .utf8
            .as_deref()
            .and_then(|s| s.chars().next())
            .map(|c| c.to_lowercase().next().unwrap_or(c))
            .filter(|c| self.theme.hint.alphabet.contains(*c));
        if let Some(c) = typed {
            map.insert(k.keycode, c);
            return Some(Input::Glyph(c));
        }
        map.get(&k.keycode).copied().map(Input::Glyph)
    }

    /// Run the actuator chain on `e`, recording `status.last_action` and the `act` stage.
    pub async fn act_on(&self, e: &Element, action: ActionKind) -> Result<(), ActError> {
        let t = Instant::now();
        let r = match &self.actuator {
            Some(a) => a.act(e, action).await,
            None => Err(ActError::NeedsPointer(action)),
        };
        let elapsed = t.elapsed();
        match &r {
            Ok(()) => tracing::info!(element = e.id.0, %action, name = %e.name, "acted"),
            Err(ActError::NeedsPointer(a)) => tracing::info!("no actuator for {a}"),
            Err(err) => tracing::warn!(element = e.id.0, %action, "{err}"),
        }
        *self.last_action.lock().await = Some(ActionOutcome {
            element: e.id,
            action,
            ok: r.is_ok(),
            error: r.as_ref().err().map(ToString::to_string),
            ms: elapsed.as_secs_f64() * 1000.0,
        });
        let mut lt = self.last_timings.lock().await;
        lt.0.retain(|s| s.stage != "act");
        lt.record("act", elapsed);
        r
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
        let out = pipeline::run(h, scope, &self.theme, timings, &self.provider)
            .await
            .map_err(|e| e.to_string())?;
        *self.last_collect.lock().await = out.reports.clone();
        Ok(out)
    }

    pub async fn handle(&self, req: Request) -> Response {
        match req {
            Request::Hints { scope, action } => {
                self.hints(scope, action.unwrap_or(ActionKind::Activate))
                    .await
            }
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
            Request::Act {
                snapshot_id,
                id,
                action,
            } => self.act(snapshot_id, id, action).await,
        }
    }

    async fn act(&self, snapshot_id: u64, id: ElementId, action: ActionKind) -> Response {
        let element = {
            let store = self.store.lock().await;
            let Some(s) = store.get(snapshot_id) else {
                return Response::err(format!("unknown snapshot {snapshot_id}"));
            };
            match s.elements.iter().find(|e| e.id == id) {
                Some(e) => e.clone(),
                None => {
                    return Response::err(format!(
                        "unknown element {} in snapshot {snapshot_id}",
                        id.0
                    ));
                }
            }
        };
        match self.act_on(&element, action).await {
            Ok(()) => Response::with(&ActReply { performed: action }),
            Err(e) => Response::err(e.to_string()),
        }
    }

    async fn hints(&self, scope: Scope, action: ActionKind) -> Response {
        self.apply(Input::HintsRequested).await;
        let mut out = match self.pipeline(scope).await {
            Ok(o) => o,
            Err(e) => return Response::err(e),
        };
        if let Some(c) = out.scene.chip.as_mut() {
            c.mode = action.to_string();
        }
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
        let id = self.store.lock().await.insert(out.elements);
        *self.last_timings.lock().await = out.timings;
        *self.shown.lock().await = Some(Shown {
            scene: out.scene,
            labels: out.labels.clone(),
        });
        self.apply(Input::Shown {
            snapshot_id: id,
            labels: out.labels,
            action,
        })
        .await;
        Response::with(&HintsReply {
            state: State::Showing,
            targets: Some(n_targets),
            elements: n_elements,
        })
    }

    async fn snapshot(&self, scope: Scope) -> Response {
        match self.pipeline(scope).await {
            Ok(o) => {
                let id = self.store.lock().await.insert(o.elements.clone());
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
            providers: vec![self.provider_status()],
            last_collect: self.last_collect.lock().await.clone(),
            last_action: self.last_action.lock().await.clone(),
        })
    }

    fn provider_status(&self) -> ProviderStatus {
        match &self.provider {
            Provider::Atspi(p) => p.status(),
            Provider::Demo(_) => ProviderStatus {
                name: "demo".into(),
                available: true,
                last_error: None,
            },
        }
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
        let d = demo();
        let r = d.handle(Request::Status).await;
        assert!(r.ok);
        let s: StatusReply = r.payload().unwrap();
        assert_eq!(s.state, State::Idle);
        assert!(!s.overlay);
    }

    fn demo() -> Arc<Daemon> {
        Daemon::with_provider(None, Provider::Demo(DemoProvider), None).0
    }

    fn element(id: u32) -> Element {
        Element {
            id: ElementId(id),
            rect: woints_core::geom::Rect::new(0, 0, 10, 10),
            role: "push button".into(),
            name: "OK".into(),
            source: woints_core::model::ElementSource::Demo,
            actions: vec![ActionKind::Pointer],
            confidence: 1.0,
            handle: None,
        }
    }

    #[tokio::test]
    async fn act_unknown_snapshot_and_element() {
        let d = demo();
        let r = d
            .handle(Request::Act {
                snapshot_id: 17,
                id: ElementId(3),
                action: ActionKind::Activate,
            })
            .await;
        assert_eq!(r.error.as_deref(), Some("unknown snapshot 17"));
        let id = d.store.lock().await.insert(vec![element(1)]);
        let r = d
            .handle(Request::Act {
                snapshot_id: id,
                id: ElementId(3),
                action: ActionKind::Activate,
            })
            .await;
        assert!(!r.ok);
    }

    #[tokio::test]
    async fn needs_pointer_returns_to_idle() {
        let d = demo();
        let id = d.store.lock().await.insert(vec![element(1), element(2)]);
        d.apply(Input::Shown {
            snapshot_id: id,
            labels: vec!["a".into(), "s".into()],
            action: ActionKind::Right,
        })
        .await;
        assert_eq!(d.state().await, State::Showing);
        d.apply(Input::Glyph('s')).await;
        assert_eq!(d.state().await, State::Idle);
        let s: StatusReply = d.handle(Request::Status).await.payload().unwrap();
        let a = s.last_action.unwrap();
        assert_eq!(
            (a.element, a.action, a.ok),
            (ElementId(2), ActionKind::Right, false)
        );
        assert_eq!(a.error.as_deref(), Some("no actuator for right"));
        assert!(s.last_timings.get("act").is_some());
        assert_eq!(s.providers[0].name, "demo");
    }

    #[tokio::test]
    async fn act_over_socket_reports_needs_pointer() {
        let d = demo();
        let id = d.store.lock().await.insert(vec![element(1)]);
        let r = d
            .handle(Request::Act {
                snapshot_id: id,
                id: ElementId(1),
                action: ActionKind::Activate,
            })
            .await;
        assert_eq!(r.error.as_deref(), Some("no actuator for activate"));
    }

    #[tokio::test]
    async fn key_mapping() {
        let d = demo();
        let key = |keycode, utf8: Option<&str>, keysym| KeyPress {
            keycode,
            keysym,
            utf8: utf8.map(String::from),
            mods: Default::default(),
        };
        assert_eq!(
            d.key_input(&key(22, None, 0xff08)).await,
            Some(Input::Backspace)
        );
        assert_eq!(
            d.key_input(&key(38, Some("a"), 0x61)).await,
            Some(Input::Glyph('a'))
        );
        // Same keycode with Shift held still maps to the learned glyph.
        assert_eq!(
            d.key_input(&key(38, Some("A"), 0x41)).await,
            Some(Input::Glyph('a'))
        );
        assert_eq!(d.key_input(&key(24, Some("q"), 0x71)).await, None);
    }

    #[tokio::test]
    async fn status_works_with_dead_overlay() {
        let d = demo();
        d.overlay_gone("test").await;
        assert!(matches!(&*d.overlay.lock().await, OverlaySlot::Dead));
        let r = d.handle(Request::Status).await;
        assert!(r.ok);
        assert!(!r.payload::<StatusReply>().unwrap().overlay);
    }
}
