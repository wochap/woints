//! Shared daemon state and request handlers.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use tokio::sync::{Mutex, Notify, mpsc};

use woints_a11y::{A11yConnection, AtspiActuator, AtspiProvider};
use woints_core::act::{ActuatorKind, ActuatorRouting, Axis, InputConfig, KeysChoice, PointerKind};
use woints_core::config::{self as cfg, Config, ProviderMode, VisionMode};
use woints_core::geom::{OutputId, pick_output};
use woints_core::model::{Action, Element, ElementId, ElementSource, Scope, Target, Timings};
use woints_core::ports::{
    ActError, Actuator, Budget, CollectReport, Collected, ElementProvider, KeyEvent, KeyPress,
    Overlay, OverlayError, OverlayEvent, ProviderError, Scene, ScrollFrame,
};
use woints_core::proto::{
    ActReply, ActionOutcome, BenchReply, BenchRow, ConfigStatus, HintsReply, OutputInfo,
    ProviderStatus, ReloadReply, Request, Response, SnapshotReply, State, StateReply, StatusReply,
};
use woints_core::theme::{Eliminated, Scheme, SchemeSource, Theme};
use woints_hypr::{Event, Hypr, HyprSnapshot, Keyboard, Monitor, SubmapKeys};
use woints_input::{AbsoluteBackend, Backend, MovecursorBackend, PointerActuator};
use woints_overlay::font::{self, ResolvedFont, fontdb::Database};
use woints_overlay::render::{OutputSurface, Renderer};
use woints_overlay::{OverlayHandle, PointerLink};
use woints_qs::QuickshellIpcProvider;

use crate::keys::Keys;
use crate::pipeline::{self, DemoProvider, NoLayers};
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
    /// Per element of the snapshot: it belongs to a layer target.
    layer: Vec<bool>,
}

/// The active config and everything derived from it; swapped whole on reload.
pub struct Settings {
    pub config: Config,
    pub theme: Theme,
    pub input: InputConfig,
    pub routing: ActuatorRouting,
    /// Face resolved for `hint.font`.
    pub font: ResolvedFont,
}

impl Settings {
    /// `fonts` is the system font database; `None` (not scanned yet) uses
    /// the embedded font.
    pub fn new(
        config: Config,
        scheme: Scheme,
        layout: Option<&[Option<char>]>,
        fonts: Option<&Database>,
    ) -> Self {
        let alphabet = config.alphabet(layout);
        let mut theme = config.theme(&alphabet, scheme);
        let input = config.input_config();
        let hint = &mut theme.hint;
        let resolved = match fonts {
            Some(db) => font::resolve(db, &hint.font, hint.font_weight),
            None => ResolvedFont::embedded(&hint.font),
        };
        if fonts.is_some() && resolved.missing() {
            tracing::warn!(
                "hint font {:?} not found; using embedded {}",
                hint.font,
                font::EMBEDDED_FAMILY
            );
        }
        let glyphs = if hint.uppercase {
            hint.alphabet.to_uppercase()
        } else {
            hint.alphabet.clone()
        };
        let loaded = font::load(&resolved);
        hint.advance_em = font::advance_em(&loaded, &glyphs);
        hint.font_file = resolved.file();
        Self {
            font: resolved,
            routing: ActuatorRouting {
                prefer: input.prefer,
            },
            theme,
            input,
            config,
        }
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// Load the config at `path`, falling back to defaults on errors (reported
/// in the status).
pub fn initial_config(path: Option<&std::path::Path>) -> (Config, ConfigStatus) {
    let Some(path) = path else {
        return (
            Config::default(),
            ConfigStatus {
                source: "defaults".into(),
                loaded_at: now_ms(),
                ..Default::default()
            },
        );
    };
    match cfg::load(path) {
        Ok((l, src)) => (l.config, loaded_status(&src, l.warnings)),
        Err(errors) => {
            for e in &errors {
                tracing::error!("config: {e}; using defaults");
            }
            (
                Config::default(),
                ConfigStatus {
                    path: Some(path.display().to_string()),
                    source: "defaults".into(),
                    loaded_at: now_ms(),
                    errors,
                    warnings: Vec::new(),
                },
            )
        }
    }
}

fn loaded_status(src: &cfg::Source, warnings: Vec<String>) -> ConfigStatus {
    for w in &warnings {
        tracing::warn!("config: {w}");
    }
    let (path, source) = match src {
        cfg::Source::Defaults => (None, "defaults".to_owned()),
        cfg::Source::File(p) => (Some(p.display().to_string()), p.display().to_string()),
    };
    ConfigStatus {
        path,
        source,
        loaded_at: now_ms(),
        errors: Vec::new(),
        warnings,
    }
}

/// How long `hints` waits for the overlay to commit.
const SHOW_TIMEOUT: Duration = Duration::from_secs(2);

pub struct Daemon {
    pub started: Instant,
    settings: RwLock<Arc<Settings>>,
    config_path: Option<PathBuf>,
    config_status: Mutex<ConfigStatus>,
    /// Home-row glyphs of the active layout, for `keys.mode = "auto"`.
    layout: Mutex<Option<Vec<Option<char>>>>,
    /// Detected system colour scheme and where it came from.
    scheme: std::sync::Mutex<(Scheme, SchemeSource)>,
    pub(crate) hypr: Option<Hypr>,
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
    /// System font database, scanned at start-up and on reload (design D5).
    fonts: Mutex<Option<Arc<Database>>>,
    pub(crate) provider: Provider,
    pub(crate) quickshell: Option<QuickshellIpcProvider>,
    /// Vision over the running overlay's capture link; rebuilt with the overlay.
    #[cfg(feature = "vision")]
    vision: Mutex<Option<Arc<pipeline::Vision>>>,
    /// ONNX session, kept across reloads unless its model/EP/threads change.
    #[cfg(feature = "vision")]
    pub(crate) onnx: std::sync::Mutex<woints_vision::OnnxSlot>,
    actuator: Option<AtspiActuator>,
    shown: Mutex<Option<Shown>>,
    /// Phase 1 of the current session, restored by Escape (drag) or Tab (scroll).
    first: Mutex<Option<Shown>>,
    /// Keycode → label glyph, learned from the overlay's key reports.
    keymap: Mutex<HashMap<u32, char>>,
    /// The main keyboard, for the submap's bind table.
    keyboard: Mutex<Option<Keyboard>>,
    keys: std::sync::Mutex<Keys>,
    key_token: AtomicU64,
    /// Active window when the session started (submap keys only).
    session_window: Mutex<Option<String>>,
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
    pub fn new(
        hypr: Option<Hypr>,
        config_path: Option<PathBuf>,
    ) -> (Arc<Self>, mpsc::UnboundedReceiver<OverlayEvent>) {
        let (provider, actuator) = if std::env::var("WOINTS_DEMO").as_deref() == Ok("1") {
            tracing::info!("WOINTS_DEMO=1: using the demo provider");
            (Provider::Demo(DemoProvider), None)
        } else {
            let conn = Arc::new(A11yConnection::new());
            let p = AtspiProvider::new(conn.clone());
            let a = AtspiActuator::new(conn, p.apps_cache());
            (Provider::Atspi(p), Some(a))
        };
        let (config, status) = initial_config(config_path.as_deref());
        let qs = QuickshellIpcProvider::new(Duration::from_millis(
            config.providers.quickshell.budget_ms,
        ));
        Self::with_parts(
            hypr,
            provider,
            actuator,
            Some(qs),
            config,
            status,
            config_path,
        )
    }

    pub fn with_parts(
        hypr: Option<Hypr>,
        provider: Provider,
        actuator: Option<AtspiActuator>,
        quickshell: Option<QuickshellIpcProvider>,
        config: Config,
        status: ConfigStatus,
        config_path: Option<PathBuf>,
    ) -> (Arc<Self>, mpsc::UnboundedReceiver<OverlayEvent>) {
        let (tx, rx) = mpsc::unbounded_channel();
        if let Provider::Atspi(p) = &provider {
            p.set_wake(config.a11y.clone());
        }
        #[cfg(feature = "vision")]
        let onnx = {
            let mut s = woints_vision::OnnxSlot::default();
            s.sync(&config.vision_config());
            s
        };
        let d = Arc::new(Self {
            started: Instant::now(),
            settings: RwLock::new(Arc::new(Settings::new(config, Scheme::Dark, None, None))),
            config_path,
            config_status: Mutex::new(status),
            layout: Mutex::new(None),
            scheme: std::sync::Mutex::new((Scheme::Dark, SchemeSource::Fallback)),
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
            fonts: Mutex::new(None),
            provider,
            quickshell,
            #[cfg(feature = "vision")]
            vision: Mutex::new(None),
            #[cfg(feature = "vision")]
            onnx: std::sync::Mutex::new(onnx),
            actuator,
            shown: Mutex::new(None),
            first: Mutex::new(None),
            keymap: Mutex::new(HashMap::new()),
            keyboard: Mutex::new(None),
            keys: std::sync::Mutex::new(Keys::Overlay { reason: None }),
            key_token: AtomicU64::new(0),
            session_window: Mutex::new(None),
            shutdown: Notify::new(),
        });
        (d, rx)
    }

    #[cfg(feature = "vision")]
    pub(crate) fn onnx_slot(&self) -> std::sync::MutexGuard<'_, woints_vision::OnnxSlot> {
        self.onnx.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// `status` `detector` and `onnx` lines.
    fn detector_status(&self) -> (String, Option<String>) {
        let kind = self.settings().config.vision.detector;
        #[cfg(feature = "vision")]
        {
            let s = self.onnx_slot();
            (
                woints_vision::ChainDetector::describe(kind, s.detector().is_some()),
                s.summary(),
            )
        }
        #[cfg(not(feature = "vision"))]
        (kind.as_str().into(), None)
    }

    /// The settings in effect; cheap to clone, stable for one request.
    pub fn settings(&self) -> Arc<Settings> {
        self.settings
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Detected colour scheme and its source.
    pub fn scheme(&self) -> (Scheme, SchemeSource) {
        *self.scheme.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Record a newly detected scheme; re-renders a showing session when the
    /// palette `hint.theme` resolves to changes.
    pub async fn set_scheme(&self, scheme: Scheme, source: SchemeSource) {
        let old = {
            let mut cur = self.scheme.lock().unwrap_or_else(|e| e.into_inner());
            std::mem::replace(&mut *cur, (scheme, source))
        };
        let config = self.settings().config.clone();
        if config.palette(old.0) != config.palette(scheme) {
            self.install(config).await;
        }
    }

    fn set_settings(&self, s: Settings) {
        *self.settings.write().unwrap_or_else(|e| e.into_inner()) = Arc::new(s);
    }

    /// Swap in `config`; cancels a session whose alphabet changed and
    /// re-renders one whose look changed.
    async fn install(&self, config: Config) {
        let layout = self.layout.lock().await.clone();
        let old = self.settings();
        let fonts = self.fonts.lock().await.clone();
        let (scheme, _) = self.scheme();
        let new = Settings::new(config, scheme, layout.as_deref(), fonts.as_deref());
        let alphabet_changed = old.theme.hint.alphabet != new.theme.hint.alphabet;
        let look_changed = old.theme != new.theme;
        let theme = new.theme.clone();
        if let Provider::Atspi(p) = &self.provider {
            p.set_wake(new.config.a11y.clone());
        }
        self.set_settings(new);
        #[cfg(feature = "vision")]
        {
            let v = self.settings().config.vision_config();
            let onnx = {
                let mut slot = self.onnx_slot();
                slot.sync(&v);
                slot.detector()
            };
            if let OverlaySlot::Running(h) = &*self.overlay.lock().await {
                *self.vision.lock().await = Some(Arc::new(pipeline::Vision::new(
                    h.capture_link(),
                    &self.settings().config,
                    onnx,
                )));
            }
        }
        if self.state().await == State::Idle {
            return;
        }
        if alphabet_changed {
            tracing::info!("label alphabet changed; cancelling the session");
            self.apply(Input::Cancel).await;
        } else if look_changed {
            let scene = {
                let mut shown = self.shown.lock().await;
                let Some(sh) = shown.as_mut() else { return };
                sh.scene.theme = theme.clone();
                sh.scene.clone()
            };
            if let Some(f) = self.first.lock().await.as_mut() {
                f.scene.theme = theme;
            }
            self.update_overlay(scene).await;
        }
    }

    /// Scan system fonts on a blocking thread.
    async fn scan_fonts(&self) {
        match tokio::task::spawn_blocking(font::system_db).await {
            Ok(db) => *self.fonts.lock().await = Some(Arc::new(db)),
            Err(e) => tracing::warn!("font scan: {e}"),
        }
    }

    /// Start-up font scan; re-resolves the hint font of the active config.
    pub async fn load_fonts(&self) {
        self.scan_fonts().await;
        let config = self.settings().config.clone();
        self.install(config).await;
    }

    /// `reload` and SIGHUP: parse, validate, swap; keep the last good config
    /// on error.
    pub async fn reload(&self) -> Response {
        let Some(path) = self.config_path.clone() else {
            return Response::err("no config path: neither XDG_CONFIG_HOME nor HOME is set");
        };
        #[allow(unused_mut)]
        let mut loaded = cfg::load(&path);
        #[cfg(feature = "vision")]
        if let Ok((l, _)) = &loaded
            && let Err(e) = woints_vision::check_config(&l.config.vision)
        {
            loaded = Err(vec![e]);
        }
        match loaded {
            Ok((l, src)) => {
                let status = loaded_status(&src, l.warnings);
                *self.config_status.lock().await = status.clone();
                self.scan_fonts().await;
                self.install(l.config).await;
                self.setup_keys().await;
                tracing::info!(source = %status.source, "config reloaded");
                Response::with(&ReloadReply { config: status })
            }
            Err(errors) => {
                for e in &errors {
                    tracing::error!("config reload: {e}; keeping the previous config");
                }
                let msg = errors.join("; ");
                self.config_status.lock().await.errors = errors;
                Response::err(msg)
            }
        }
    }

    pub async fn config_status(&self) -> ConfigStatus {
        self.config_status.lock().await.clone()
    }

    /// Re-read the keyboard layout from Hyprland for `keys.mode = "auto"`.
    pub async fn refresh_layout(&self) {
        let Some(h) = &self.hypr else { return };
        let glyphs = match h.keyboards().await {
            Ok(kbs) => {
                let kb = crate::keys::main_keyboard(&kbs);
                *self.keyboard.lock().await = kb.cloned();
                kb.and_then(crate::keys::layout_glyphs)
            }
            Err(e) => {
                tracing::warn!("keyboard layout: {e}");
                None
            }
        };
        if glyphs.is_none() {
            tracing::info!("keyboard layout unknown; labels use keys.alphabet");
        }
        let changed = *self.layout.lock().await != glyphs;
        if changed {
            *self.layout.lock().await = glyphs;
            let config = self.settings().config.clone();
            self.install(config).await;
            tracing::info!(alphabet = %self.settings().theme.hint.alphabet, "label alphabet");
        }
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
            let onnx = self.onnx_slot().detector();
            *self.vision.lock().await = Some(Arc::new(pipeline::Vision::new(
                handle.capture_link(),
                &self.settings().config,
                onnx,
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
        self.end_keys().await;
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
        let timeout = Duration::from_millis(u64::from(self.settings().input.unmap_timeout_ms));
        if let OverlaySlot::Running(h) = &*self.overlay.lock().await {
            match tokio::time::timeout(timeout, h.hide_and_wait()).await {
                Ok(Ok(())) => {}
                Ok(Err(e)) => tracing::warn!("overlay unmap: {e}"),
                Err(_) => tracing::warn!("overlay unmap not confirmed in time"),
            }
        }
    }

    fn flash_ms(&self) -> u32 {
        self.settings().theme.timing.flash_ms.min(FLASH_CAP_MS)
    }

    /// Feed the session FSM and perform its effects in order.
    pub async fn apply(&self, input: Input) {
        if input == Input::Quit {
            self.end_keys().await;
        }
        let mut s = self.session.lock().await;
        let (next, effects) = s.transition(input, Instant::now(), self.flash_ms());
        *s = next;
        drop(s);
        let acting = effects
            .iter()
            .any(|e| matches!(e, Effect::Act { .. } | Effect::Drag { .. }));
        if acting {
            // Before the action: the target window must see its own keys again.
            self.end_keys().await;
        }
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
                        let fade = self.settings().theme.hint.eliminated == Eliminated::Fade;
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
                    self.end_keys().await;
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
                        self.begin_keys().await;
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
            &first.layer,
            source,
            &label,
            &first.targets,
            None,
            scope,
            &first.scene.theme,
        );
        self.update_overlay(scene.clone()).await;
        *self.shown.lock().await = Some(Shown {
            scene,
            labels: labels.clone(),
            targets: first.targets,
            layer: first.layer,
        });
        let mut s = self.session.lock().await;
        let (next, _) = s.transition(Input::SecondShown { labels }, Instant::now(), 0);
        *s = next;
        drop(s);
        self.begin_keys().await;
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
        self.begin_keys().await;
    }

    pub(crate) fn keys(&self) -> Keys {
        self.keys.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    #[cfg(test)]
    pub(crate) fn set_keys(&self, keys: Keys) {
        *self.keys.lock().unwrap_or_else(|e| e.into_inner()) = keys;
    }

    /// Start capturing keys for the state just shown (re-arms the timer).
    async fn begin_keys(&self) {
        let token = self.key_token.fetch_add(1, Ordering::Relaxed) + 1;
        self.keys().begin(token).await;
    }

    async fn end_keys(&self) {
        self.keys().end().await;
    }

    /// Address of the focused window, when keys come from a submap.
    async fn active_address(&self) -> Option<String> {
        if self.keys().overlay() {
            return None;
        }
        let h = self.hypr.as_ref()?;
        h.active_window().await.ok().flatten().map(|c| c.address)
    }

    /// Pick the key source per `input.keys` and install its binds. A session
    /// in progress is cancelled first, since its source is about to change.
    pub async fn setup_keys(&self) {
        if self.state().await != State::Idle {
            self.apply(Input::Cancel).await;
        }
        let settings = self.settings();
        let keys = if settings.input.keys == KeysChoice::Overlay {
            Keys::Overlay { reason: None }
        } else {
            match self.install_submap(&settings).await {
                Ok(k) => Keys::Submap(Arc::new(k)),
                Err(reason) if settings.input.keys == KeysChoice::Submap => {
                    tracing::error!("key capture through a submap unavailable: {reason}");
                    Keys::Unavailable(format!("submap install failed: {reason}"))
                }
                Err(reason) => {
                    tracing::warn!("key capture through a submap unavailable: {reason}");
                    Keys::Overlay {
                        reason: Some(format!("submap install failed: {reason}")),
                    }
                }
            }
        };
        tracing::info!(keys = %keys.describe(), "key source");
        *self.keys.lock().unwrap_or_else(|e| e.into_inner()) = keys;
    }

    async fn install_submap(&self, settings: &Settings) -> Result<SubmapKeys, String> {
        let hypr = self.hypr.clone().ok_or("not running under Hyprland")?;
        let kb = self
            .keyboard
            .lock()
            .await
            .clone()
            .ok_or("keyboard layout unknown")?;
        let (table, translate) =
            crate::keys::bind_table(&settings.input.submap, &kb, &settings.theme.hint.alphabet)
                .ok_or("keymap unavailable")?;
        let timer_ms = settings.theme.timing.max_showing_ms.saturating_add(2000);
        SubmapKeys::install(hypr, table, translate, timer_ms)
            .await
            .map_err(|e| e.to_string())
    }

    /// Route one event from a key source into the session.
    pub async fn key_event(&self, ev: KeyEvent) {
        match ev {
            KeyEvent::Key(k) => {
                tracing::debug!(keycode = k.keycode, keysym = k.keysym, utf8 = ?k.utf8, "key");
                if let Some(input) = self.key_input(&k).await {
                    self.apply(input).await;
                }
            }
            KeyEvent::Escape => {
                let before = self.state().await;
                self.apply(Input::Escape).await;
                // The overlay unmaps itself on Escape; a submap source leaves
                // that to the daemon.
                if before != State::Idle
                    && self.state().await == State::Idle
                    && !self.keys().overlay()
                {
                    self.end_keys().await;
                    *self.shown.lock().await = None;
                    *self.first.lock().await = None;
                    self.hide_overlay().await;
                }
            }
            KeyEvent::Ended(reason) => {
                if self.state().await != State::Idle {
                    tracing::info!(%reason, "key capture ended; cancelling the session");
                }
                self.apply(Input::Cancel).await;
            }
        }
    }

    /// Feed a socket2 event to the key source and the session guards.
    pub async fn hypr_event(&self, ev: &Event) {
        let keys = self.keys();
        match ev {
            Event::Custom { .. } | Event::Submap { .. } => {
                if let Keys::Submap(k) = &keys
                    && let Some(e) = k.on_event(ev)
                {
                    self.key_event(e).await;
                }
            }
            Event::ActiveWindow { address } if !keys.overlay() => {
                let started = self.session_window.lock().await.clone();
                if let Some(w) = started
                    && address.as_ref() != Some(&w)
                    && self.state().await != State::Idle
                {
                    self.key_event(KeyEvent::Ended("active window changed".into()))
                        .await;
                }
            }
            Event::ConfigReloaded => {
                tracing::info!("hyprland config reloaded; reinstalling key binds");
                self.setup_keys().await;
            }
            _ => {}
        }
    }

    /// The socket2 stream dropped: submap keys can no longer arrive.
    pub async fn events_lost(&self) {
        if !self.keys().overlay() && self.state().await != State::Idle {
            self.key_event(KeyEvent::Ended("event stream lost".into()))
                .await;
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
        let settings = self.settings();
        let alphabet = &settings.theme.hint.alphabet;
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
        Some(Input::Glyph(glyph, settings.input.modifiers.mapped(k.mods)))
    }

    /// The pointer actuator over the overlay's virtual pointer.
    async fn pointer(&self) -> Result<Pointer, ActError> {
        let unavailable = || ActError::Failed(POINTER_UNAVAILABLE.into());
        let input = self.settings().input.clone();
        if input.pointer == PointerKind::None {
            return Err(unavailable());
        }
        let hypr = self.hypr.clone().ok_or_else(unavailable)?;
        let link = match &*self.overlay.lock().await {
            OverlaySlot::Running(h) if h.is_alive() && h.caps().pointer => h.pointer_link(),
            _ => return Err(unavailable()),
        };
        let backend = match input.pointer {
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
        Ok(PointerActuator::new(backend, &input))
    }

    fn pointer_status(&self, caps: Option<woints_core::ports::OverlayCaps>) -> String {
        match (self.settings().input.pointer, caps) {
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
        let route = self.settings().routing.route(e, action)?;
        if e.source == ElementSource::Quickshell
            && let Some(qs) = &self.quickshell
        {
            match qs.invoke(e, action).await {
                Ok(()) => return Ok(()),
                Err(err) => {
                    tracing::info!(element = e.id.0, %action, "quickshell {err}; pointer click");
                    let a = if Action::POINTER.contains(&action)
                        && action != Action::Drag
                        && action != Action::Scroll
                    {
                        action
                    } else {
                        Action::Click
                    };
                    return self.pointer().await?.act(a, e.rect).await;
                }
            }
        }
        match route {
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

    /// `hypr.query` stage: what selection for `scope` needs.
    async fn query(&self, scope: Scope, timings: &mut Timings) -> Result<HyprSnapshot, String> {
        let t = Instant::now();
        let hypr = self.hypr.as_ref().ok_or("not running under Hyprland")?;
        let monitors = self.monitors().await?;
        let active = hypr.active_window().await.map_err(|e| e.to_string())?;
        let (clients, layers) = tokio::join!(
            async {
                if scope == Scope::All {
                    hypr.clients().await.map_err(|e| e.to_string())
                } else {
                    Ok(Vec::new())
                }
            },
            async {
                if scope == Scope::Focused {
                    Ok(Vec::new())
                } else {
                    hypr.layers().await.map_err(|e| e.to_string())
                }
            }
        );
        let snap = HyprSnapshot {
            monitors,
            active,
            clients: clients?,
            layers: layers?,
        };
        timings.record("hypr.query", t.elapsed());
        Ok(snap)
    }

    async fn pipeline(
        &self,
        scope: Scope,
        action: Option<Action>,
    ) -> Result<pipeline::Output, String> {
        let settings = self.settings();
        let mut timings = Timings::default();
        let h = self.query(scope, &mut timings).await?;
        #[cfg(feature = "vision")]
        let vision = self.vision_stage().await;
        macro_rules! go {
            ($layers:expr, $vision:expr) => {
                pipeline::run(
                    h,
                    scope,
                    action,
                    &settings.config,
                    &settings.theme,
                    timings,
                    &self.provider,
                    $layers,
                    $vision,
                )
                .await
            };
        }
        let qs = self
            .quickshell
            .as_ref()
            .filter(|_| settings.config.providers.quickshell.mode != ProviderMode::Off);
        #[cfg(feature = "vision")]
        let out = match (qs, vision.as_deref()) {
            (Some(q), Some(v)) => go!(q, v),
            (Some(q), None) => go!(q, &pipeline::NoVision),
            (None, Some(v)) => go!(&NoLayers, v),
            (None, None) => go!(&NoLayers, &pipeline::NoVision),
        };
        #[cfg(not(feature = "vision"))]
        let out = match qs {
            Some(q) => go!(q, &pipeline::NoVision),
            None => go!(&NoLayers, &pipeline::NoVision),
        };
        let out = out.map_err(|e| e.to_string())?;
        *self.last_collect.lock().await = out.reports.clone();
        Ok(out)
    }

    /// The vision stage, starting the overlay thread for its capture link
    /// (no surface is mapped) unless `vision.mode = off`.
    #[cfg(feature = "vision")]
    async fn vision_stage(&self) -> Option<Arc<pipeline::Vision>> {
        if self.settings().config.vision.mode == VisionMode::Off {
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
            Request::Hints { scope, action } => {
                let scope = scope.unwrap_or(self.settings().config.scope.default);
                self.hints(scope, action).await
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
            Request::Snapshot { scope } => {
                let scope = scope.unwrap_or(self.settings().config.scope.default);
                self.snapshot(scope).await
            }
            Request::Bench { iterations, scope } => {
                let scope = scope.unwrap_or(self.settings().config.scope.default);
                self.bench(iterations, scope).await
            }
            Request::Reload => self.reload().await,
            Request::Doctor => Response::with(&woints_core::proto::DoctorReply {
                report: self.doctor().await,
            }),
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
        let routing = self.settings().routing;
        let scroll_step = self.settings().input.scroll_step;
        let r = match (action, dest) {
            (Action::Drag, Some(d)) => {
                self.recorded(&element, action, async {
                    routing.route(&element, action)?;
                    routing.route(&d, action)?;
                    self.pointer()
                        .await?
                        .drag(element.rect.center(), d.rect.center())
                        .await
                })
                .await
            }
            (Action::Scroll, _) if axis.is_some() || amount.is_some() => {
                self.recorded(&element, action, async {
                    routing.route(&element, action)?;
                    let p = self.pointer().await?;
                    p.scroll_enter(element.rect).await?;
                    let lines = amount.unwrap_or(scroll_step as i32);
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
        if let Keys::Unavailable(reason) = self.keys() {
            return Response::err(format!("input.keys = \"submap\": {reason}"));
        }
        // Keep woints out of captured frames (design D14): unmap first.
        if self.shown.lock().await.is_some() {
            self.hide_overlay_and_wait().await;
        }
        self.apply(Input::HintsRequested).await;
        let explicit = action.is_some();
        let action = action.unwrap_or(self.settings().input.default_action);
        let mut out = match self.pipeline(scope, Some(action)).await {
            Ok(o) => o,
            Err(e) => return Response::err(e),
        };
        let (mode, sub) = pipeline::chip_mode(action);
        pipeline::set_chip(&mut out.scene, mode, sub, false);
        // A single scroll container is scrolled without picking.
        let direct = action == Action::Scroll && out.elements.len() == 1;
        out.scene.keyboard = self.keys().overlay();
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

        *self.session_window.lock().await = self.active_address().await;
        let n_targets = out.targets.len();
        let n_elements = out.elements.len();
        let vision = out.vision.clone();
        let id = self.store.lock().await.insert(out.elements);
        *self.last_timings.lock().await = out.timings;
        let shown = Shown {
            scene: first_scene,
            labels: out.labels.clone(),
            targets: out.targets,
            layer: out.layer,
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
        self.begin_keys().await;
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
        let (detector, onnx) = self.detector_status();
        Response::with(&StatusReply {
            state: self.state().await,
            version: env!("CARGO_PKG_VERSION").into(),
            uptime_ms: self.started.elapsed().as_millis() as u64,
            outputs,
            hyprland_version: self.hyprland_version().await,
            overlay,
            last_timings: self.last_timings.lock().await.clone(),
            providers: self.provider_statuses().await,
            last_collect: self.last_collect.lock().await.clone(),
            last_action: self.last_action.lock().await.clone(),
            pointer: self.pointer_status(caps),
            clipboard: match caps.and_then(|c| c.clipboard) {
                Some(_) => "data-control".into(),
                None => "unavailable".into(),
            },
            config: self.config_status().await,
            scheme: self.scheme().0,
            source: self.scheme().1,
            detector,
            onnx,
            keys: self.keys().describe(),
        })
    }

    async fn provider_statuses(&self) -> Vec<ProviderStatus> {
        let mut v = vec![match &self.provider {
            Provider::Atspi(p) => p.status(),
            Provider::Demo(_) => ProviderStatus {
                name: "demo".into(),
                available: true,
                last_error: None,
            },
        }];
        if let Some(q) = &self.quickshell {
            let mut s = q.status().await;
            if self.settings().config.providers.quickshell.mode == ProviderMode::Off {
                s.available = false;
                s.last_error = Some("disabled (providers.quickshell.mode = \"off\")".into());
            }
            v.push(s);
        }
        v
    }

    pub(crate) async fn overlay_globals(&self) -> Result<Vec<String>, String> {
        if matches!(&*self.overlay.lock().await, OverlaySlot::None) {
            self.start_overlay().await.map_err(|e| e.to_string())?;
        }
        match &*self.overlay.lock().await {
            OverlaySlot::Running(h) if h.is_alive() => Ok(h.globals().to_vec()),
            _ => Err("overlay not running".into()),
        }
    }

    pub(crate) async fn active_target(&self) -> Option<Target> {
        let hypr = self.hypr.as_ref()?;
        let w = hypr.active_window().await.ok()??;
        let mons = self.monitors().await.ok()?;
        let outputs: Vec<_> = mons
            .iter()
            .filter(|m| !m.disabled)
            .map(Monitor::geom)
            .collect();
        let focused = mons
            .iter()
            .find(|m| m.focused)
            .map(|m| OutputId(m.name.clone()));
        Some(woints_hypr::focused_target(&w, &outputs, focused.as_ref()))
    }

    /// Vision decision for doctor's focused-window lines.
    pub(crate) async fn vision_decision(
        &self,
        usable: usize,
        target: &Target,
        wake_failed: bool,
    ) -> (bool, String) {
        #[cfg(feature = "vision")]
        {
            let d = woints_vision::decide_vision(
                &self.settings().config.vision_config(),
                usable,
                &target.class,
                target.surface.is_layer(),
                usable > 0,
                wake_failed,
            );
            (d.run, d.reason.into())
        }
        #[cfg(not(feature = "vision"))]
        {
            let _ = (usable, target, wake_failed);
            (false, "vision_unavailable".into())
        }
    }

    pub(crate) async fn hyprland_version_pub(&self) -> Option<String> {
        self.hyprland_version().await
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
    use woints_core::theme::Tokens;

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

    #[test]
    fn missing_font_uses_embedded_advance() {
        let mut config = Config::default();
        config.hint.font = "NoSuchFont".into();
        let s = Settings::new(config, Scheme::Dark, None, Some(&Database::new()));
        assert!(s.font.missing());
        assert_eq!(s.theme.hint.font_file, None);
        let embedded = font::advance_em(&font::load_file(None), "ASDFGHJKL");
        assert_eq!(s.theme.hint.advance_em, embedded);
    }

    fn demo() -> Arc<Daemon> {
        demo_with(None)
    }

    fn demo_with(path: Option<PathBuf>) -> Arc<Daemon> {
        let (config, status) = initial_config(path.as_deref());
        Daemon::with_parts(
            None,
            Provider::Demo(DemoProvider),
            None,
            None,
            config,
            status,
            path,
        )
        .0
    }

    fn temp_config(name: &str, text: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("woints-cfg-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("config.toml");
        std::fs::write(&p, text).unwrap();
        p
    }

    #[tokio::test]
    async fn status_config_defaults() {
        let d = demo();
        let s: StatusReply = d.handle(Request::Status).await.payload().unwrap();
        assert_eq!(s.config.path, None);
        assert_eq!(s.config.source, "defaults");
    }

    #[tokio::test]
    async fn reload_keeps_last_good() {
        let p = temp_config("reload", "[hint]\ntheme = \"light\"\n");
        let d = demo_with(Some(p.clone()));
        assert_eq!(
            d.settings().theme.tokens,
            woints_core::theme::Tokens::light()
        );
        let loaded_at = d.config_status().await.loaded_at;
        std::fs::write(&p, "[theme.dark]\nbg = \"red\"\n").unwrap();
        let r = d.handle(Request::Reload).await;
        assert_eq!(
            r.error.as_deref(),
            Some("theme.dark.bg: expected #rrggbb or #rrggbbaa")
        );
        assert_eq!(
            d.settings().theme.tokens,
            woints_core::theme::Tokens::light()
        );
        let s: StatusReply = d.handle(Request::Status).await.payload().unwrap();
        assert!(!s.config.errors.is_empty());
        assert_eq!(s.config.loaded_at, loaded_at);
        std::fs::write(&p, "[keys]\nmode = \"explicit\"\nalphabet = \"qwer\"\n").unwrap();
        let r = d.handle(Request::Reload).await;
        assert!(r.ok, "{r:?}");
        let rr: ReloadReply = r.payload().unwrap();
        assert!(rr.config.errors.is_empty());
        assert_eq!(d.settings().theme.hint.alphabet, "qwer");
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }

    #[tokio::test]
    async fn reload_cancels_on_alphabet_change() {
        let p = temp_config("cancel", "");
        let d = demo_with(Some(p.clone()));
        let id = d.store.lock().await.insert(vec![element(1), element(2)]);
        d.apply(Input::Shown {
            snapshot_id: id,
            labels: vec!["a".into(), "s".into()],
            action: Action::Click,
            explicit: false,
        })
        .await;
        assert_eq!(d.state().await, State::Showing);
        std::fs::write(&p, "[keys]\nmode = \"explicit\"\nalphabet = \"qwer\"\n").unwrap();
        assert!(d.handle(Request::Reload).await.ok);
        assert_eq!(d.state().await, State::Idle);
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }

    #[tokio::test]
    async fn auto_theme_follows_scheme() {
        let d = demo();
        assert_eq!(d.scheme(), (Scheme::Dark, SchemeSource::Fallback));
        assert_eq!(d.settings().theme.tokens, Tokens::dark());
        d.set_scheme(Scheme::Light, SchemeSource::Portal).await;
        assert_eq!(d.settings().theme.tokens, Tokens::light());
        let s: StatusReply = d.handle(Request::Status).await.payload().unwrap();
        assert_eq!((s.scheme, s.source), (Scheme::Light, SchemeSource::Portal));
    }

    #[tokio::test]
    async fn forced_palette_ignores_scheme() {
        let p = temp_config("forced", "[hint]\ntheme = \"dark\"\n");
        let d = demo_with(Some(p.clone()));
        d.set_scheme(Scheme::Light, SchemeSource::Portal).await;
        assert_eq!(d.settings().theme.tokens, Tokens::dark());
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }

    #[tokio::test]
    async fn scheme_flip_keeps_showing_session() {
        let d = demo();
        let id = d.store.lock().await.insert(vec![element(1), element(2)]);
        d.apply(Input::Shown {
            snapshot_id: id,
            labels: vec!["aa".into(), "as".into()],
            action: Action::Click,
            explicit: false,
        })
        .await;
        d.apply(Input::Glyph('a', None)).await;
        d.set_scheme(Scheme::Light, SchemeSource::Portal).await;
        assert_eq!(d.state().await, State::Showing);
        assert_eq!(d.settings().theme.tokens, Tokens::light());
        let s: StatusReply = d.handle(Request::Status).await.payload().unwrap();
        assert_eq!(s.state, State::Showing);
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

    use crate::keys::tests::Counting;
    use std::sync::atomic::Ordering::SeqCst;

    async fn showing(d: &Daemon, action: Action) -> Arc<Counting> {
        let c = Arc::new(Counting::default());
        d.set_keys(Keys::Test(c.clone()));
        let id = d.store.lock().await.insert(vec![element(1), element(2)]);
        d.apply(Input::Shown {
            snapshot_id: id,
            labels: vec!["a".into(), "s".into()],
            action,
            explicit: true,
        })
        .await;
        assert_eq!(d.state().await, State::Showing);
        c
    }

    #[tokio::test]
    async fn every_path_to_idle_ends_the_keys() {
        let d = demo();
        let c = showing(&d, Action::Click).await;
        d.apply(Input::Cancel).await;
        assert_eq!(c.ends.load(SeqCst), 1);
        let c = showing(&d, Action::Click).await;
        d.key_event(KeyEvent::Escape).await;
        assert_eq!(d.state().await, State::Idle);
        assert_eq!(c.ends.load(SeqCst), 1);
        let c = showing(&d, Action::Click).await;
        d.apply(Input::Tick {
            max_showing: Duration::ZERO,
        })
        .await;
        assert_eq!(d.state().await, State::Idle);
        assert!(c.ends.load(SeqCst) >= 1);
        let c = showing(&d, Action::Click).await;
        d.apply(Input::MonitorsChanged).await;
        assert!(c.ends.load(SeqCst) >= 1);
        let c = showing(&d, Action::Click).await;
        d.apply(Input::Quit).await;
        assert!(c.ends.load(SeqCst) >= 1);
        let c = showing(&d, Action::Click).await;
        d.overlay_gone("test").await;
        assert!(c.ends.load(SeqCst) >= 1);
        assert_eq!(d.state().await, State::Idle);
    }

    #[tokio::test]
    async fn the_action_path_ends_the_keys_first() {
        let d = demo();
        let c = showing(&d, Action::Right).await;
        d.key_event(KeyEvent::Key(KeyPress {
            keycode: 39,
            keysym: 0x73,
            utf8: Some("s".into()),
            mods: Default::default(),
        }))
        .await;
        assert_eq!(d.state().await, State::Idle);
        assert!(c.ends.load(SeqCst) >= 1);
    }

    #[tokio::test]
    async fn key_events_drive_the_session() {
        let d = demo();
        let _ = showing(&d, Action::Click).await;
        d.key_event(KeyEvent::Ended("test".into())).await;
        assert_eq!(d.state().await, State::Idle);
        let _ = showing(&d, Action::Click).await;
        d.key_event(KeyEvent::Escape).await;
        assert_eq!(d.state().await, State::Idle);
    }

    #[tokio::test]
    async fn active_window_change_ends_the_session() {
        let d = demo();
        let _ = showing(&d, Action::Click).await;
        *d.session_window.lock().await = Some("0x1".into());
        d.hypr_event(&Event::ActiveWindow {
            address: Some("0x1".into()),
        })
        .await;
        assert_eq!(d.state().await, State::Showing);
        d.hypr_event(&Event::ActiveWindow {
            address: Some("0x2".into()),
        })
        .await;
        assert_eq!(d.state().await, State::Idle);
    }

    #[tokio::test]
    async fn lost_event_stream_ends_the_session() {
        let d = demo();
        let _ = showing(&d, Action::Click).await;
        d.events_lost().await;
        assert_eq!(d.state().await, State::Idle);
    }

    #[tokio::test]
    async fn config_reload_event_cancels_and_reinstalls() {
        let d = demo();
        let _ = showing(&d, Action::Click).await;
        d.hypr_event(&Event::ConfigReloaded).await;
        assert_eq!(d.state().await, State::Idle);
        // Without Hyprland the reinstall falls back to the overlay.
        let s: StatusReply = d.handle(Request::Status).await.payload().unwrap();
        assert_eq!(
            s.keys,
            "overlay (submap install failed: not running under Hyprland)"
        );
    }

    #[tokio::test]
    async fn keys_overlay_choice_is_reported_plainly() {
        let p = temp_config("keys", "[input]\nkeys = \"overlay\"\n");
        let d = demo_with(Some(p.clone()));
        d.setup_keys().await;
        let s: StatusReply = d.handle(Request::Status).await.payload().unwrap();
        assert_eq!(s.keys, "overlay");
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
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
