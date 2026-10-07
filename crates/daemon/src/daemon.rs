//! Shared daemon state and request handlers.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::{Mutex, Notify, mpsc};

use woints_a11y::{A11yConnection, AtspiActuator, AtspiProvider};
use woints_core::act::{ActuatorKind, ActuatorRouting, Axis, InputConfig, PointerKind};
use woints_core::config::{Config, VisionMode};
use woints_core::geom::{OutputId, pick_output};
use woints_core::model::{Action, Element, ElementId, Scope, Target, Timings};
use woints_core::ports::{
    ActError, Actuator, Budget, CollectReport, Collected, ElementProvider, KeyPress, Overlay,
    OverlayError, OverlayEvent, ProviderError, Scene, ScrollFrame,
};
use woints_core::proto::{
    ActReply, ActionOutcome, BenchReply, BenchRow, HintsReply, OutputInfo, ProviderStatus, Request,
    Response, SnapshotReply, State, StateReply, StatusReply,
};
use woints_core::theme::{Eliminated, Theme};
use woints_hypr::{Hypr, Monitor};
use woints_input::{AbsoluteBackend, Backend, MovecursorBackend, PointerActuator};
use woints_overlay::render::{OutputSurface, Renderer};
use woints_overlay::{OverlayHandle, PointerLink};

use crate::pipeline::{self, DemoProvider, HyprState};
use crate::session::{Effect, Input, Session};
use crate::store::SnapshotStore;

/// Hard cap on the selection flash (design §05).
const FLASH_CAP_MS: u32 = 120;
/// xkb keysyms.
const KEYSYM_BACKSPACE: u32 = 0xff08;
const KEYSYM_TAB: u32 = 0xff09;

/// Scroll direction of a key in scroll mode: `h j k l` and arrows.
pub fn scroll_key(keysym: u32) -> Option<(Axis, i32)> {
    match keysym {
        0x68 | 0x48 | 0xff51 => Some((Axis::Horizontal, -1)),
        0x6a | 0x4a | 0xff54 => Some((Axis::Vertical, 1)),
        0x6b | 0x4b | 0xff52 => Some((Axis::Vertical, -1)),
        0x6c | 0x4c | 0xff53 => Some((Axis::Horizontal, 1)),
        _ => None,
    }
}

pub const CLIPBOARD_UNAVAILABLE: &str =
    "clipboard unavailable: zwlr_data_control_v1 not advertised";
pub const POINTER_UNAVAILABLE: &str = "virtual pointer unavailable";

type Pointer = PointerActuator<Backend<Hypr, PointerLink>>;

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
#[derive(Clone)]
struct Shown {
    scene: Scene,
    labels: Vec<String>,
    targets: Vec<Target>,
}

/// How long `hints` waits for the overlay to commit.
const SHOW_TIMEOUT: Duration = Duration::from_secs(2);

pub struct Daemon {
    pub started: Instant,
    pub theme: Theme,
    #[cfg_attr(not(feature = "vision"), allow(dead_code))]
    pub config: Config,
    pub input: InputConfig,
    routing: ActuatorRouting,
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
    /// Vision over the running overlay's capture link; rebuilt with the overlay.
    #[cfg(feature = "vision")]
    vision: Mutex<Option<Arc<pipeline::Vision>>>,
    actuator: Option<AtspiActuator>,
    shown: Mutex<Option<Shown>>,
    /// Phase 1 of the current session, restored by Escape (drag) or Tab (scroll).
    first: Mutex<Option<Shown>>,
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
        let input = InputConfig::default();
        let d = Arc::new(Self {
            started: Instant::now(),
            theme: Theme::default(),
            config: config_from_env(),
            routing: ActuatorRouting {
                prefer: input.prefer,
            },
            input,
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
            #[cfg(feature = "vision")]
            vision: Mutex::new(None),
            actuator,
            shown: Mutex::new(None),
            first: Mutex::new(None),
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
        #[cfg(feature = "vision")]
        {
            *self.vision.lock().await = Some(Arc::new(pipeline::Vision::new(
                handle.capture_link(),
                &self.config,
            )));
        }
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
        let timeout = Duration::from_millis(u64::from(self.input.unmap_timeout_ms));
        if let OverlaySlot::Running(h) = &*self.overlay.lock().await {
            match tokio::time::timeout(timeout, h.hide_and_wait()).await {
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
        let acting = effects
            .iter()
            .any(|e| matches!(e, Effect::Act { .. } | Effect::Drag { .. }));
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
                    *self.first.lock().await = None;
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
                } => match self.element(snapshot_id, index).await {
                    Some(e) => {
                        let _ = self.act_on(&e, action).await;
                    }
                    None => tracing::warn!(snapshot_id, index, "selected element vanished"),
                },
                Effect::BeginSecond {
                    snapshot_id,
                    source,
                } => self.begin_second(snapshot_id, source).await,
                Effect::Drag {
                    snapshot_id,
                    from,
                    to,
                } => {
                    let (Some(a), Some(b)) = (
                        self.element(snapshot_id, from).await,
                        self.element(snapshot_id, to).await,
                    ) else {
                        tracing::warn!(snapshot_id, from, to, "drag element vanished");
                        continue;
                    };
                    let _ = self
                        .recorded(&a, Action::Drag, async {
                            self.pointer()
                                .await?
                                .drag(a.rect.center(), b.rect.center())
                                .await
                        })
                        .await;
                }
                Effect::BeginScroll { snapshot_id, index } => {
                    self.begin_scroll(snapshot_id, index).await
                }
                Effect::Scroll {
                    snapshot_id,
                    index,
                    axis,
                    sign,
                } => {
                    let r = match self.pointer().await {
                        Ok(p) => p.scroll_key(axis, sign).await,
                        Err(e) => Err(e),
                    };
                    if let Err(e) = r {
                        tracing::warn!(snapshot_id, index, "scroll: {e}");
                    }
                }
                Effect::ReShow => {
                    let first = self.first.lock().await.clone();
                    if let Some(f) = first {
                        self.update_overlay(f.scene.clone()).await;
                        *self.shown.lock().await = Some(f);
                    }
                }
            }
        }
    }

    async fn element(&self, snapshot_id: u64, index: usize) -> Option<Element> {
        self.store
            .lock()
            .await
            .get(snapshot_id)
            .and_then(|s| s.elements.get(index).cloned())
    }

    /// Drag phase 2: relabel without the source's glyphs and pin the source.
    async fn begin_second(&self, snapshot_id: u64, source: usize) {
        let Some(first) = self.first.lock().await.clone() else {
            return;
        };
        let Some(elements) = self
            .store
            .lock()
            .await
            .get(snapshot_id)
            .map(|s| s.elements.clone())
        else {
            return;
        };
        let label = first.labels.get(source).cloned().unwrap_or_default();
        let scope = first
            .scene
            .chip
            .as_ref()
            .map(|c| c.scope)
            .unwrap_or_default();
        let (scene, labels) = pipeline::second_scene(
            &first.scene,
            &elements,
            source,
            &label,
            &first.targets,
            None,
            scope,
            &self.theme,
        );
        self.update_overlay(scene.clone()).await;
        *self.shown.lock().await = Some(Shown {
            scene,
            labels: labels.clone(),
            targets: first.targets,
        });
        let mut s = self.session.lock().await;
        let (next, _) = s.transition(Input::SecondShown { labels }, Instant::now(), 0);
        *s = next;
    }

    /// Scroll mode: stroke (and rail) around the container, cursor on it.
    async fn begin_scroll(&self, snapshot_id: u64, index: usize) {
        let Some(e) = self.element(snapshot_id, index).await else {
            return;
        };
        let Some(first) = self.first.lock().await.clone() else {
            return;
        };
        let value = match &self.actuator {
            Some(a) if !e.caps.is_empty() => a.scroll_value(&e).await,
            _ => None,
        };
        let mut scene = first.scene.clone();
        scene.scroll = pick_output(e.rect, &scene.outputs, None).map(|output| ScrollFrame {
            rect: e.rect,
            output,
            value,
        });
        pipeline::set_chip(&mut scene, "scroll", Some("hjkl ←↓↑→"), false);
        self.update_overlay(scene).await;
        let r = match self.pointer().await {
            Ok(p) => p.scroll_enter(e.rect).await,
            Err(e) => Err(e),
        };
        if let Err(err) = r {
            tracing::warn!(element = e.id.0, "scroll mode: {err}");
        }
    }

    /// Map an overlay key press to a session input. Glyphs are learned per
    /// keycode, so the glyph shown and the key pressed agree under any layout
    /// and with modifiers held.
    pub async fn key_input(&self, k: &KeyPress) -> Option<Input> {
        match k.keysym {
            KEYSYM_BACKSPACE => return Some(Input::Backspace),
            KEYSYM_TAB => return Some(Input::Tab),
            _ => {}
        }
        if self.state().await == State::Scrolling {
            return scroll_key(k.keysym).map(|(a, s)| Input::ScrollKey(a, s));
        }
        let alphabet = &self.theme.hint.alphabet;
        let lower = |c: char| c.to_lowercase().next().unwrap_or(c);
        let mut map = self.keymap.lock().await;
        let typed = k
            .utf8
            .as_deref()
            .and_then(|s| s.chars().next())
            .map(lower)
            .filter(|c| alphabet.contains(*c))
            .or_else(|| {
                char::from_u32(k.keysym)
                    .filter(char::is_ascii_graphic)
                    .map(lower)
                    .filter(|c| alphabet.contains(*c))
            });
        let glyph = match typed {
            Some(c) => {
                map.insert(k.keycode, c);
                c
            }
            None => *map.get(&k.keycode)?,
        };
        Some(Input::Glyph(glyph, self.input.modifiers.mapped(k.mods)))
    }

    /// The pointer actuator over the overlay's virtual pointer.
    async fn pointer(&self) -> Result<Pointer, ActError> {
        let unavailable = || ActError::Failed(POINTER_UNAVAILABLE.into());
        if self.input.pointer == PointerKind::None {
            return Err(unavailable());
        }
        let hypr = self.hypr.clone().ok_or_else(unavailable)?;
        let link = match &*self.overlay.lock().await {
            OverlaySlot::Running(h) if h.is_alive() && h.caps().pointer => h.pointer_link(),
            _ => return Err(unavailable()),
        };
        let backend = match self.input.pointer {
            PointerKind::Absolute => Backend::Absolute(AbsoluteBackend {
                ipc: hypr,
                pointer: link,
                outputs: self
                    .monitors()
                    .await
                    .unwrap_or_default()
                    .iter()
                    .filter(|m| !m.disabled)
                    .map(Monitor::geom)
                    .collect(),
            }),
            _ => Backend::Movecursor(MovecursorBackend {
                ipc: hypr,
                pointer: link,
            }),
        };
        Ok(PointerActuator::new(backend, &self.input))
    }

    fn pointer_status(&self, caps: Option<woints_core::ports::OverlayCaps>) -> String {
        match (self.input.pointer, caps) {
            (PointerKind::None, _) => "unavailable".into(),
            (k, Some(c)) if c.pointer && self.hypr.is_some() => k.as_str().into(),
            _ => "unavailable".into(),
        }
    }

    /// `copy`: element text to the clipboard through data control.
    async fn copy(&self, e: &Element) -> Result<(), ActError> {
        let link = match &*self.overlay.lock().await {
            OverlaySlot::Running(h) if h.is_alive() && h.caps().clipboard.is_some() => {
                h.pointer_link()
            }
            _ => return Err(ActError::Failed(CLIPBOARD_UNAVAILABLE.into())),
        };
        let a = self
            .actuator
            .as_ref()
            .ok_or_else(|| ActError::Failed("copy needs the AT-SPI provider".into()))?;
        let text = a.text(e).await?;
        link.set_clipboard(text).await.map_err(ActError::Failed)
    }

    /// Route `action` on `e` to an actuator (design D16) and run it.
    async fn perform(&self, e: &Element, action: Action) -> Result<(), ActError> {
        match self.routing.route(e, action)? {
            ActuatorKind::DoAction | ActuatorKind::GrabFocus => {
                let r = match &self.actuator {
                    Some(a) => a.act(e, action).await,
                    None => Err(ActError::NeedsPointer(action)),
                };
                match r {
                    Err(ActError::NeedsPointer(_)) => {
                        tracing::info!(element = e.id.0, %action, "AT-SPI declined; pointer click");
                        self.pointer().await?.act(Action::Click, e.rect).await
                    }
                    r => r,
                }
            }
            ActuatorKind::ReadText => self.copy(e).await,
            ActuatorKind::Pointer => match action {
                Action::Drag | Action::Scroll => Err(ActError::Failed(format!(
                    "{action} needs {}",
                    if action == Action::Drag {
                        "a destination (\"to\")"
                    } else {
                        "scroll mode or \"axis\"/\"amount\""
                    }
                ))),
                Action::Focus => self.pointer().await?.act(Action::Click, e.rect).await,
                a => self.pointer().await?.act(a, e.rect).await,
            },
        }
    }

    /// Run `f` for `action` on `e`, recording `status.last_action` and the `act` stage.
    async fn recorded(
        &self,
        e: &Element,
        action: Action,
        f: impl std::future::Future<Output = Result<(), ActError>>,
    ) -> Result<(), ActError> {
        let t = Instant::now();
        let r = f.await;
        let elapsed = t.elapsed();
        match &r {
            Ok(()) => tracing::info!(element = e.id.0, %action, name = %e.name, "acted"),
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
        lt.0.retain(|s| s.stage != Timings::ACT);
        lt.record(Timings::ACT, elapsed);
        r
    }

    pub async fn act_on(&self, e: &Element, action: Action) -> Result<(), ActError> {
        self.recorded(e, action, self.perform(e, action)).await
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

    async fn pipeline(
        &self,
        scope: Scope,
        action: Option<Action>,
    ) -> Result<pipeline::Output, String> {
        let mut timings = Timings::default();
        let h = self.query(&mut timings).await?;
        #[cfg(feature = "vision")]
        let out = match self.vision_stage().await {
            Some(v) => {
                pipeline::run(h, scope, action, &self.theme, timings, &self.provider, &*v).await
            }
            None => {
                pipeline::run(
                    h,
                    scope,
                    action,
                    &self.theme,
                    timings,
                    &self.provider,
                    &pipeline::NoVision,
                )
                .await
            }
        };
        #[cfg(not(feature = "vision"))]
        let out = pipeline::run(
            h,
            scope,
            action,
            &self.theme,
            timings,
            &self.provider,
            &pipeline::NoVision,
        )
        .await;
        let out = out.map_err(|e| e.to_string())?;
        *self.last_collect.lock().await = out.reports.clone();
        Ok(out)
    }

    /// The vision stage, starting the overlay thread for its capture link
    /// (no surface is mapped) unless `vision.mode = off`.
    #[cfg(feature = "vision")]
    async fn vision_stage(&self) -> Option<Arc<pipeline::Vision>> {
        if self.config.vision.mode == VisionMode::Off {
            return None;
        }
        let none = matches!(&*self.overlay.lock().await, OverlaySlot::None);
        if none && let Err(e) = self.start_overlay().await {
            tracing::warn!("vision: overlay start failed: {e}");
        }
        self.vision.lock().await.clone()
    }

    pub async fn handle(&self, req: Request) -> Response {
        match req {
            Request::Hints { scope, action } => self.hints(scope, action).await,
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
                to,
                axis,
                amount,
            } => self.act(snapshot_id, id, action, to, axis, amount).await,
        }
    }

    async fn act(
        &self,
        snapshot_id: u64,
        id: ElementId,
        action: Action,
        to: Option<ElementId>,
        axis: Option<Axis>,
        amount: Option<i32>,
    ) -> Response {
        let (element, dest) = {
            let store = self.store.lock().await;
            let Some(s) = store.get(snapshot_id) else {
                return Response::err(format!("unknown snapshot {snapshot_id}"));
            };
            let find = |id: ElementId| s.elements.iter().find(|e| e.id == id).cloned();
            let Some(e) = find(id) else {
                return Response::err(format!(
                    "unknown element {} in snapshot {snapshot_id}",
                    id.0
                ));
            };
            let dest = match to {
                Some(t) => match find(t) {
                    Some(d) => Some(d),
                    None => {
                        return Response::err(format!(
                            "unknown element {} in snapshot {snapshot_id}",
                            t.0
                        ));
                    }
                },
                None => None,
            };
            (e, dest)
        };
        let r = match (action, dest) {
            (Action::Drag, Some(d)) => {
                self.recorded(&element, action, async {
                    self.routing.route(&element, action)?;
                    self.routing.route(&d, action)?;
                    self.pointer()
                        .await?
                        .drag(element.rect.center(), d.rect.center())
                        .await
                })
                .await
            }
            (Action::Scroll, _) if axis.is_some() || amount.is_some() => {
                self.recorded(&element, action, async {
                    self.routing.route(&element, action)?;
                    let p = self.pointer().await?;
                    p.scroll_enter(element.rect).await?;
                    let lines = amount.unwrap_or(self.input.scroll_step as i32);
                    p.scroll(axis.unwrap_or_default(), lines).await
                })
                .await
            }
            _ => self.act_on(&element, action).await,
        };
        match r {
            Ok(()) => Response::with(&ActReply { performed: action }),
            Err(e) => Response::err(e.to_string()),
        }
    }

    async fn hints(&self, scope: Scope, action: Option<Action>) -> Response {
        // Keep woints out of captured frames (design D14): unmap first.
        if self.shown.lock().await.is_some() {
            self.hide_overlay_and_wait().await;
        }
        self.apply(Input::HintsRequested).await;
        let explicit = action.is_some();
        let action = action.unwrap_or(self.input.default_action);
        let mut out = match self.pipeline(scope, Some(action)).await {
            Ok(o) => o,
            Err(e) => return Response::err(e),
        };
        let (mode, sub) = pipeline::chip_mode(action);
        pipeline::set_chip(&mut out.scene, mode, sub, false);
        // A single scroll container is scrolled without picking.
        let direct = action == Action::Scroll && out.elements.len() == 1;
        let first_scene = out.scene.clone();
        if direct {
            let e = &out.elements[0];
            out.scene.scroll =
                pick_output(e.rect, &out.scene.outputs, None).map(|output| ScrollFrame {
                    rect: e.rect,
                    output,
                    value: None,
                });
            pipeline::set_chip(&mut out.scene, "scroll", Some("hjkl ←↓↑→"), false);
        }
        if out.targets.is_empty() {
            return Response::with(&HintsReply {
                state: State::Idle,
                targets: None,
                elements: 0,
                vision: None,
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
        let vision = out.vision.clone();
        let id = self.store.lock().await.insert(out.elements);
        *self.last_timings.lock().await = out.timings;
        let shown = Shown {
            scene: first_scene,
            labels: out.labels.clone(),
            targets: out.targets,
        };
        *self.first.lock().await = Some(shown.clone());
        *self.shown.lock().await = Some(shown);
        if direct {
            self.apply(Input::ScrollDirect {
                snapshot_id: id,
                labels: out.labels,
                index: 0,
            })
            .await;
        } else {
            self.apply(Input::Shown {
                snapshot_id: id,
                labels: out.labels,
                action,
                explicit,
            })
            .await;
        }
        Response::with(&HintsReply {
            state: self.state().await,
            targets: Some(n_targets),
            elements: n_elements,
            vision: Some(vision),
        })
    }

    async fn snapshot(&self, scope: Scope) -> Response {
        match self.pipeline(scope, None).await {
            Ok(o) => {
                let id = self.store.lock().await.insert(o.elements.clone());
                Response::with(&SnapshotReply {
                    snapshot_id: id,
                    targets: o.targets,
                    elements: o.elements,
                    timings: o.timings,
                    vision: o.vision,
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
        let caps = match &*self.overlay.lock().await {
            OverlaySlot::Running(h) if h.is_alive() => Some(h.caps()),
            _ => None,
        };
        let overlay = caps.is_some();
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
            pointer: self.pointer_status(caps),
            clipboard: match caps.and_then(|c| c.clipboard) {
                Some(_) => "data-control".into(),
                None => "unavailable".into(),
            },
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
            let mut o = match self.pipeline(scope, None).await {
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

/// Defaults plus `WOINTS_VISION_MODE` (`fallback`, `always`, `off`) until
/// the config file arrives in M5.
fn config_from_env() -> Config {
    let mut c = Config::default();
    if let Ok(m) = std::env::var("WOINTS_VISION_MODE") {
        match serde_json::from_value::<VisionMode>(serde_json::Value::String(m.clone())) {
            Ok(mode) => c.vision.mode = mode,
            Err(_) => tracing::warn!("WOINTS_VISION_MODE={m}: expected fallback, always or off"),
        }
    }
    c
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
            actions: Action::supported(&[]),
            caps: vec![],
            confidence: 1.0,
            handle: None,
        }
    }

    fn act(snapshot_id: u64, id: u32, action: Action) -> Request {
        Request::Act {
            snapshot_id,
            id: ElementId(id),
            action,
            to: None,
            axis: None,
            amount: None,
        }
    }

    #[tokio::test]
    async fn act_unknown_snapshot_and_element() {
        let d = demo();
        let r = d.handle(act(17, 3, Action::Click)).await;
        assert_eq!(r.error.as_deref(), Some("unknown snapshot 17"));
        let id = d.store.lock().await.insert(vec![element(1)]);
        let r = d.handle(act(id, 3, Action::Click)).await;
        assert!(!r.ok);
    }

    #[tokio::test]
    async fn pointer_unavailable_returns_to_idle() {
        let d = demo();
        let id = d.store.lock().await.insert(vec![element(1), element(2)]);
        d.apply(Input::Shown {
            snapshot_id: id,
            labels: vec!["a".into(), "s".into()],
            action: Action::Right,
            explicit: true,
        })
        .await;
        assert_eq!(d.state().await, State::Showing);
        d.apply(Input::Glyph('s', None)).await;
        assert_eq!(d.state().await, State::Idle);
        let s: StatusReply = d.handle(Request::Status).await.payload().unwrap();
        let a = s.last_action.unwrap();
        assert_eq!(
            (a.element, a.action, a.ok),
            (ElementId(2), Action::Right, false)
        );
        assert_eq!(a.error.as_deref(), Some(POINTER_UNAVAILABLE));
        assert!(s.last_timings.get("act").is_some());
        assert_eq!(s.providers[0].name, "demo");
        assert_eq!(
            (s.pointer.as_str(), s.clipboard.as_str()),
            ("unavailable", "unavailable")
        );
    }

    #[tokio::test]
    async fn act_errors() {
        let d = demo();
        let id = d.store.lock().await.insert(vec![element(1), element(2)]);
        let r = d.handle(act(id, 1, Action::Click)).await;
        assert_eq!(r.error.as_deref(), Some(POINTER_UNAVAILABLE));
        let r = d.handle(act(id, 1, Action::Focus)).await;
        assert_eq!(
            r.error.as_deref(),
            Some(
                "element 1 does not support focus; supported: click,right,middle,double,hover,drag,scroll"
            )
        );
        let r = d
            .handle(Request::Act {
                snapshot_id: id,
                id: ElementId(1),
                action: Action::Drag,
                to: Some(ElementId(2)),
                axis: None,
                amount: None,
            })
            .await;
        assert_eq!(r.error.as_deref(), Some(POINTER_UNAVAILABLE));
        let r = d.handle(act(id, 1, Action::Drag)).await;
        assert_eq!(
            r.error.as_deref(),
            Some("drag needs a destination (\"to\")")
        );
    }

    #[tokio::test]
    async fn copy_without_data_control() {
        let d = demo();
        let mut e = element(1);
        e.caps = vec![woints_core::model::AtspiCap::Accessible];
        e.actions = Action::supported(&e.caps);
        let id = d.store.lock().await.insert(vec![e]);
        let r = d.handle(act(id, 1, Action::Copy)).await;
        assert_eq!(r.error.as_deref(), Some(CLIPBOARD_UNAVAILABLE));
    }

    #[tokio::test]
    async fn drag_session_returns_to_idle() {
        let d = demo();
        let id = d
            .store
            .lock()
            .await
            .insert(vec![element(1), element(2), element(3)]);
        d.apply(Input::Shown {
            snapshot_id: id,
            labels: vec!["a".into(), "s".into(), "d".into()],
            action: Action::Drag,
            explicit: true,
        })
        .await;
        d.apply(Input::Glyph('a', None)).await;
        assert_eq!(d.state().await, State::ShowingSecond);
        d.apply(Input::Cancel).await;
        assert_eq!(d.state().await, State::Idle);
    }

    #[tokio::test]
    async fn key_mapping() {
        let d = demo();
        let key = |keycode, utf8: Option<&str>, keysym, shift| KeyPress {
            keycode,
            keysym,
            utf8: utf8.map(String::from),
            mods: woints_core::ports::Modifiers {
                shift,
                ..Default::default()
            },
        };
        assert_eq!(
            d.key_input(&key(22, None, 0xff08, false)).await,
            Some(Input::Backspace)
        );
        assert_eq!(
            d.key_input(&key(23, None, 0xff09, false)).await,
            Some(Input::Tab)
        );
        assert_eq!(
            d.key_input(&key(38, Some("a"), 0x61, false)).await,
            Some(Input::Glyph('a', None))
        );
        // Same keycode with Shift held still maps to the glyph, with the
        // Shift mapping attached.
        assert_eq!(
            d.key_input(&key(38, Some("A"), 0x41, true)).await,
            Some(Input::Glyph('a', Some(Action::Right)))
        );
        assert_eq!(d.key_input(&key(24, Some("q"), 0x71, false)).await, None);
    }

    #[test]
    fn scroll_keys() {
        assert_eq!(scroll_key(0x6a), Some((Axis::Vertical, 1)));
        assert_eq!(scroll_key(0xff52), Some((Axis::Vertical, -1)));
        assert_eq!(scroll_key(0x68), Some((Axis::Horizontal, -1)));
        assert_eq!(scroll_key(0xff53), Some((Axis::Horizontal, 1)));
        assert_eq!(scroll_key(0x61), None);
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
