//! Hyprland IPC over its Unix sockets (design D4). Never forks `hyprctl`.

use std::collections::BTreeMap;
use std::io;
use std::path::PathBuf;

use serde::Deserialize;
use serde::de::DeserializeOwned;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::net::unix::OwnedReadHalf;

use woints_core::geom::{Global, OutputGeom, OutputId, Point, Rect, Size, pick_output};
use woints_core::model::{Scope, Surface, Target};
use woints_core::ports::{SelectError, TargetSelector};

mod submap;

pub use submap::{MODULE_MISSING, SubmapInput, SubmapKeys};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("not running under Hyprland: {0} is not set")]
    NoEnv(&'static str),
    #[error("hyprland socket: {0}")]
    Io(#[from] io::Error),
    #[error("hyprland reply for {cmd}: {source}")]
    Parse {
        cmd: String,
        source: serde_json::Error,
    },
    #[error("hyprland dispatch failed: {0}")]
    Dispatch(String),
    #[error("not supported by this Hyprland build: {0}")]
    Unsupported(&'static str),
}

pub type Result<T> = std::result::Result<T, Error>;

/// `$XDG_RUNTIME_DIR/hypr/$HYPRLAND_INSTANCE_SIGNATURE`.
pub fn socket_dir() -> Result<PathBuf> {
    let rt = std::env::var_os("XDG_RUNTIME_DIR").ok_or(Error::NoEnv("XDG_RUNTIME_DIR"))?;
    let sig = std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE")
        .ok_or(Error::NoEnv("HYPRLAND_INSTANCE_SIGNATURE"))?;
    Ok(PathBuf::from(rt).join("hypr").join(sig))
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct WorkspaceRef {
    pub id: i64,
    pub name: String,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct Monitor {
    pub id: i64,
    pub name: String,
    pub description: String,
    pub x: i32,
    pub y: i32,
    /// Mode size in device pixels (before transform).
    pub width: i32,
    pub height: i32,
    pub physical_width: i32,
    pub physical_height: i32,
    pub scale: f32,
    pub transform: i32,
    pub focused: bool,
    pub disabled: bool,
    pub active_workspace: WorkspaceRef,
    /// `id` 0 when no special workspace is shown.
    pub special_workspace: WorkspaceRef,
}

impl Monitor {
    /// Logical size: device size ÷ scale, axes swapped for 90°/270° transforms.
    pub fn logical_size(&self) -> (i32, i32) {
        let s = if self.scale > 0.0 {
            f64::from(self.scale)
        } else {
            1.0
        };
        let w = (f64::from(self.width) / s).round() as i32;
        let h = (f64::from(self.height) / s).round() as i32;
        if self.transform % 2 == 1 {
            (h, w)
        } else {
            (w, h)
        }
    }

    pub fn logical_rect(&self) -> Rect<Global> {
        let (w, h) = self.logical_size();
        Rect::new(self.x, self.y, w, h)
    }

    pub fn geom(&self) -> OutputGeom {
        let (w, h) = self.logical_size();
        OutputGeom {
            id: OutputId(self.name.clone()),
            origin: Point::new(self.x, self.y),
            logical_size: Size { w, h },
            scale: self.scale,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct Client {
    pub address: String,
    pub mapped: bool,
    pub hidden: bool,
    pub at: [i32; 2],
    pub size: [i32; 2],
    pub workspace: WorkspaceRef,
    pub floating: bool,
    pub monitor: i64,
    pub class: String,
    pub title: String,
    pub pid: i64,
    pub xwayland: bool,
    pub fullscreen: i32,
}

impl Client {
    pub fn rect(&self) -> Rect<Global> {
        Rect::new(self.at[0], self.at[1], self.size[0], self.size[1])
    }
    pub fn pid(&self) -> Option<u32> {
        u32::try_from(self.pid).ok().filter(|&p| p > 0)
    }
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct Layer {
    pub address: String,
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    pub namespace: String,
    pub pid: i64,
}

impl Layer {
    pub fn rect(&self) -> Rect<Global> {
        Rect::new(self.x, self.y, self.w, self.h)
    }
}

/// A layer surface with the monitor and level it lives on.
#[derive(Debug, Clone, PartialEq)]
pub struct LayerEntry {
    pub monitor: String,
    /// 0 background, 1 bottom, 2 top, 3 overlay.
    pub level: u8,
    pub layer: Layer,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct LayersMonitor {
    levels: BTreeMap<String, Vec<Layer>>,
}

/// One keyboard from `devices`.
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct Keyboard {
    pub name: String,
    pub rules: String,
    pub model: String,
    /// Comma-separated layouts.
    pub layout: String,
    pub variant: String,
    pub options: String,
    pub active_layout_index: u32,
    pub active_keymap: String,
    pub main: bool,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Devices {
    keyboards: Vec<Keyboard>,
}

pub fn parse_keyboards(s: &str) -> std::result::Result<Vec<Keyboard>, serde_json::Error> {
    serde_json::from_str::<Devices>(s).map(|d| d.keyboards)
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct Version {
    pub version: String,
    pub tag: String,
    pub commit: String,
}

pub fn parse_cursor_pos(s: &str) -> std::result::Result<Point<Global>, serde_json::Error> {
    #[derive(Deserialize)]
    struct Pos {
        x: f64,
        y: f64,
    }
    let p: Pos = serde_json::from_str(s)?;
    Ok(Point::new(p.x.round() as i32, p.y.round() as i32))
}

pub fn parse_monitors(s: &str) -> std::result::Result<Vec<Monitor>, serde_json::Error> {
    serde_json::from_str(s)
}

pub fn parse_active_window(s: &str) -> std::result::Result<Option<Client>, serde_json::Error> {
    let c: Client = serde_json::from_str(s)?;
    Ok((!c.address.is_empty()).then_some(c))
}

pub fn parse_clients(s: &str) -> std::result::Result<Vec<Client>, serde_json::Error> {
    serde_json::from_str(s)
}

pub fn parse_layers(s: &str) -> std::result::Result<Vec<LayerEntry>, serde_json::Error> {
    let m: BTreeMap<String, LayersMonitor> = serde_json::from_str(s)?;
    let mut out = Vec::new();
    for (monitor, lm) in m {
        for (level, layers) in lm.levels {
            let level = level.parse().unwrap_or(0);
            out.extend(layers.into_iter().map(|layer| LayerEntry {
                monitor: monitor.clone(),
                level,
                layer,
            }));
        }
    }
    Ok(out)
}

/// How this Hyprland build parses `dispatch` arguments.
///
/// Builds with the Lua config API (0.56+ with `hyprland.lua`) treat
/// `dispatch <args>` as `hl.dispatch(<args>)`, so dispatchers must be Lua
/// expressions such as `hl.dsp.cursor.move({ x = 1, y = 2 })`. Older builds
/// take the hyprlang form `movecursor 1 2`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DispatchSyntax {
    Lua,
    Legacy,
}

/// Request client for `.socket.sock`. Cheap to clone; one connection per request.
#[derive(Debug, Clone, Default)]
pub struct Hypr {
    dir: PathBuf,
    syntax: std::sync::Arc<std::sync::OnceLock<DispatchSyntax>>,
}

impl Hypr {
    pub fn from_env() -> Result<Self> {
        Ok(Self {
            dir: socket_dir()?,
            ..Self::default()
        })
    }

    pub fn with_dir(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            ..Self::default()
        }
    }

    /// Force the dispatch syntax instead of probing (config `hypr.dispatch_syntax`).
    pub fn with_syntax(self, syntax: DispatchSyntax) -> Self {
        let _ = self.syntax.set(syntax);
        self
    }

    /// Detect the dispatch syntax once: `eval return 1` answers `ok` only on
    /// Lua-enabled builds. Verified live on Hyprland 0.56.2 (2026-10-07).
    pub async fn dispatch_syntax(&self) -> DispatchSyntax {
        if let Some(s) = self.syntax.get() {
            return *s;
        }
        let detected = match self.request("eval return 1").await {
            Ok(reply) if reply.trim() == "ok" => DispatchSyntax::Lua,
            _ => DispatchSyntax::Legacy,
        };
        let _ = self.syntax.set(detected);
        detected
    }

    /// Move the cursor to a global logical point with the right dispatcher
    /// for this build (`hl.dsp.cursor.move` or `movecursor`).
    pub async fn move_cursor(&self, p: Point<Global>) -> Result<()> {
        let cmd = match self.dispatch_syntax().await {
            DispatchSyntax::Lua => format!("hl.dsp.cursor.move({{ x = {}, y = {} }})", p.x, p.y),
            DispatchSyntax::Legacy => format!("movecursor {} {}", p.x, p.y),
        };
        self.dispatch(&cmd).await
    }

    /// Send a raw request and read the reply to EOF.
    pub async fn request(&self, cmd: &str) -> Result<String> {
        let mut s = UnixStream::connect(self.dir.join(".socket.sock")).await?;
        s.write_all(cmd.as_bytes()).await?;
        let mut buf = String::new();
        s.read_to_string(&mut buf).await?;
        Ok(buf)
    }

    async fn json<T: DeserializeOwned>(&self, cmd: &str) -> Result<T> {
        let s = self.request(&format!("j/{cmd}")).await?;
        serde_json::from_str(&s).map_err(|source| Error::Parse {
            cmd: cmd.into(),
            source,
        })
    }

    pub async fn active_window(&self) -> Result<Option<Client>> {
        let s = self.request("j/activewindow").await?;
        parse_active_window(&s).map_err(|source| Error::Parse {
            cmd: "activewindow".into(),
            source,
        })
    }

    pub async fn monitors(&self) -> Result<Vec<Monitor>> {
        self.json("monitors").await
    }

    pub async fn clients(&self) -> Result<Vec<Client>> {
        self.json("clients").await
    }

    pub async fn layers(&self) -> Result<Vec<LayerEntry>> {
        let s = self.request("j/layers").await?;
        parse_layers(&s).map_err(|source| Error::Parse {
            cmd: "layers".into(),
            source,
        })
    }

    pub async fn keyboards(&self) -> Result<Vec<Keyboard>> {
        let s = self.request("j/devices").await?;
        parse_keyboards(&s).map_err(|source| Error::Parse {
            cmd: "devices".into(),
            source,
        })
    }

    pub async fn version(&self) -> Result<Version> {
        self.json("version").await
    }

    /// `cursorpos`, in global logical coordinates.
    pub async fn cursor_pos(&self) -> Result<Point<Global>> {
        let s = self.request("j/cursorpos").await?;
        parse_cursor_pos(&s).map_err(|source| Error::Parse {
            cmd: "cursorpos".into(),
            source,
        })
    }

    /// `dispatch <cmd>`; any reply other than `ok` is an error.
    pub async fn dispatch(&self, cmd: &str) -> Result<()> {
        let reply = self.request(&format!("dispatch {cmd}")).await?;
        if reply.trim() == "ok" {
            Ok(())
        } else {
            Err(Error::Dispatch(reply.trim().to_owned()))
        }
    }

    pub async fn events(&self) -> Result<EventStream> {
        let s = UnixStream::connect(self.dir.join(".socket2.sock")).await?;
        let (r, _w) = s.into_split();
        Ok(EventStream {
            lines: BufReader::new(r).lines(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// `activelayout>>KEYBOARD,LAYOUT`
    ActiveLayout {
        keyboard: String,
        layout: String,
    },
    /// `focusedmon>>MON,WORKSPACE`
    FocusedMon {
        monitor: String,
        workspace: String,
    },
    /// `activewindowv2>>ADDRESS` (empty when nothing is focused)
    ActiveWindow {
        address: Option<String>,
    },
    /// `monitoradded>>NAME`
    MonitorAdded {
        name: String,
    },
    /// `monitorremoved>>NAME`
    MonitorRemoved {
        name: String,
    },
    /// `submap>>NAME` (empty for the default submap)
    Submap {
        name: String,
    },
    /// `configreloaded>>`
    ConfigReloaded,
    /// `custom>>PAYLOAD`, from the `event` dispatcher
    Custom {
        payload: String,
    },
    Other {
        name: String,
        data: String,
    },
}

pub fn parse_event(line: &str) -> Option<Event> {
    let (name, data) = line.split_once(">>")?;
    let pair = |d: &str| {
        let (a, b) = d.split_once(',').unwrap_or((d, ""));
        (a.to_owned(), b.to_owned())
    };
    Some(match name {
        "activelayout" => {
            let (keyboard, layout) = pair(data);
            Event::ActiveLayout { keyboard, layout }
        }
        "focusedmon" => {
            let (monitor, workspace) = pair(data);
            Event::FocusedMon { monitor, workspace }
        }
        "activewindowv2" => Event::ActiveWindow {
            address: (!data.is_empty() && data != ",")
                .then(|| format!("0x{}", data.trim_start_matches("0x"))),
        },
        "monitoradded" => Event::MonitorAdded {
            name: data.to_owned(),
        },
        "monitorremoved" => Event::MonitorRemoved {
            name: data.to_owned(),
        },
        "submap" => Event::Submap {
            name: data.to_owned(),
        },
        "configreloaded" => Event::ConfigReloaded,
        "custom" => Event::Custom {
            payload: data.to_owned(),
        },
        _ => Event::Other {
            name: name.to_owned(),
            data: data.to_owned(),
        },
    })
}

/// Events from `.socket2.sock`.
pub struct EventStream {
    lines: tokio::io::Lines<BufReader<OwnedReadHalf>>,
}

impl EventStream {
    /// Next parsed event; `Ok(None)` when Hyprland closes the socket.
    pub async fn next(&mut self) -> Result<Option<Event>> {
        loop {
            match self.lines.next_line().await? {
                None => return Ok(None),
                Some(l) => {
                    if let Some(e) = parse_event(&l) {
                        return Ok(Some(e));
                    }
                }
            }
        }
    }
}

/// Hyprland state one selection works on.
#[derive(Debug, Clone, Default)]
pub struct HyprSnapshot {
    pub monitors: Vec<Monitor>,
    pub active: Option<Client>,
    /// Queried only for [`Scope::All`].
    pub clients: Vec<Client>,
    /// Queried only for scopes with layers.
    pub layers: Vec<LayerEntry>,
}

impl HyprSnapshot {
    pub fn outputs(&self) -> Vec<OutputGeom> {
        self.monitors
            .iter()
            .filter(|m| !m.disabled)
            .map(Monitor::geom)
            .collect()
    }

    pub fn focused_output(&self) -> Option<OutputId> {
        self.monitors
            .iter()
            .find(|m| m.focused && !m.disabled)
            .map(|m| OutputId(m.name.clone()))
    }
}

/// Layer filtering from `[scope]`.
#[derive(Debug, Clone, PartialEq)]
pub struct LayerFilter {
    /// Hyprland level indices (0 background … 3 overlay).
    pub levels: Vec<u8>,
    pub ignore: Vec<String>,
}

impl Default for LayerFilter {
    fn default() -> Self {
        Self {
            levels: vec![1, 2, 3],
            ignore: Vec::new(),
        }
    }
}

impl LayerFilter {
    pub fn keeps(&self, e: &LayerEntry) -> bool {
        self.levels.contains(&e.level)
            && !self.ignore.contains(&e.layer.namespace)
            // 1×1 surfaces are protocol placeholders.
            && e.layer.w > 1
            && e.layer.h > 1
    }
}

/// Build the focused-window target, choosing its output by centre containment.
pub fn focused_target(win: &Client, outputs: &[OutputGeom], focused: Option<&OutputId>) -> Target {
    let rect = win.rect();
    Target {
        surface: Surface::Toplevel {
            address: win.address.clone(),
        },
        rect,
        output: pick_output(rect, outputs, focused).unwrap_or_else(|| OutputId(String::new())),
        pid: win.pid(),
        class: win.class.clone(),
    }
}

pub fn layer_target(e: &LayerEntry) -> Target {
    Target {
        surface: Surface::Layer {
            address: e.layer.address.clone(),
            namespace: e.layer.namespace.clone(),
        },
        rect: e.layer.rect(),
        output: OutputId(e.monitor.clone()),
        pid: u32::try_from(e.layer.pid).ok().filter(|&p| p > 0),
        class: e.layer.namespace.clone(),
    }
}

/// Clients on each monitor's active workspace or its shown special
/// workspace, mapped and not hidden.
pub fn visible_clients<'a>(clients: &'a [Client], monitors: &[Monitor]) -> Vec<&'a Client> {
    clients
        .iter()
        .filter(|c| c.mapped && !c.hidden)
        .filter(|c| {
            monitors.iter().any(|m| {
                !m.disabled
                    && m.id == c.monitor
                    && (c.workspace.id == m.active_workspace.id
                        || (m.special_workspace.id != 0
                            && c.workspace.id == m.special_workspace.id))
            })
        })
        .collect()
}

/// Resolve `scope` into targets: toplevels first (the focused one leading),
/// then layers.
pub fn select_targets(snap: &HyprSnapshot, scope: Scope, filter: &LayerFilter) -> Vec<Target> {
    let outputs = snap.outputs();
    let focused = snap.focused_output();
    let mut targets: Vec<Target> = match scope {
        Scope::Focused | Scope::FocusedAndLayers => snap
            .active
            .iter()
            .map(|w| focused_target(w, &outputs, focused.as_ref()))
            .collect(),
        Scope::Layers => Vec::new(),
        Scope::All => {
            let active = snap.active.as_ref().map(|a| a.address.as_str());
            let mut v = visible_clients(&snap.clients, &snap.monitors);
            v.sort_by_key(|c| Some(c.address.as_str()) != active);
            v.into_iter()
                .map(|c| focused_target(c, &outputs, focused.as_ref()))
                .collect()
        }
    };
    let layer_output = match scope {
        Scope::Focused => return targets,
        Scope::FocusedAndLayers => Some(
            targets
                .first()
                .map(|t| t.output.clone())
                .or(focused)
                .unwrap_or_else(|| OutputId(String::new())),
        ),
        Scope::Layers | Scope::All => None,
    };
    let live: Vec<&str> = outputs.iter().map(|o| o.id.0.as_str()).collect();
    targets.extend(
        snap.layers
            .iter()
            .filter(|e| filter.keeps(e) && live.contains(&e.monitor.as_str()))
            .filter(|e| layer_output.as_ref().is_none_or(|o| o.0 == e.monitor))
            .map(layer_target),
    );
    targets
}

/// [`TargetSelector`] backed by Hyprland.
#[derive(Debug, Clone)]
pub struct HyprSelector {
    pub hypr: Hypr,
    pub filter: LayerFilter,
}

impl Hypr {
    /// Query what `scope` needs for selection.
    pub async fn snapshot(&self, scope: Scope) -> Result<HyprSnapshot> {
        let monitors = self.monitors().await?;
        let active = self.active_window().await?;
        let clients = if scope == Scope::All {
            self.clients().await?
        } else {
            Vec::new()
        };
        let layers = if scope == Scope::Focused {
            Vec::new()
        } else {
            self.layers().await?
        };
        Ok(HyprSnapshot {
            monitors,
            active,
            clients,
            layers,
        })
    }
}

impl TargetSelector for HyprSelector {
    async fn select(&self, scope: Scope) -> std::result::Result<Vec<Target>, SelectError> {
        let snap = self
            .hypr
            .snapshot(scope)
            .await
            .map_err(|e| SelectError::Backend(e.to_string()))?;
        Ok(select_targets(&snap, scope, &self.filter))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MONITORS: &str = include_str!("../tests/fixtures/monitors.json");
    const ACTIVE: &str = include_str!("../tests/fixtures/activewindow.json");
    const LAYERS: &str = include_str!("../tests/fixtures/layers.json");
    const CLIENTS: &str = include_str!("../tests/fixtures/clients.json");
    const VERSION: &str = include_str!("../tests/fixtures/version.json");

    #[test]
    fn cursor_pos() {
        assert_eq!(
            parse_cursor_pos(r#"{"x": -1780, "y": 120.4}"#).unwrap(),
            Point::new(-1780, 120)
        );
    }

    #[test]
    fn monitors_fixture() {
        let m = parse_monitors(MONITORS).unwrap();
        assert_eq!(m.len(), 1);
        let e = &m[0];
        assert_eq!(e.name, "eDP-1");
        assert_eq!((e.width, e.height, e.scale), (2880, 1800, 2.0));
        assert_eq!(e.logical_size(), (1440, 900));
        assert_eq!(e.logical_rect(), Rect::new(0, 0, 1440, 900));
        assert!(e.focused);
        assert_eq!(e.physical_width, 310);
        assert_eq!(e.active_workspace.id, 8);
        assert_eq!(e.special_workspace.id, 0);
    }

    #[test]
    fn rotated_monitor_swaps_axes() {
        let m = Monitor {
            width: 1920,
            height: 1080,
            scale: 1.5,
            transform: 1,
            ..Default::default()
        };
        assert_eq!(m.logical_size(), (720, 1280));
    }

    #[test]
    fn active_window_fixture() {
        let w = parse_active_window(ACTIVE).unwrap().unwrap();
        assert_eq!(w.class, "footclient");
        assert_eq!(w.rect(), Rect::new(1, 41, 1438, 858));
        assert_eq!(w.pid(), Some(4242));
        assert_eq!(w.monitor, 0);
        assert!(!w.floating && !w.xwayland);
        assert!(w.address.starts_with("0x"));
    }

    #[test]
    fn no_active_window() {
        assert_eq!(parse_active_window("{}").unwrap(), None);
    }

    #[test]
    fn layers_fixture_has_quickshell_bar_with_pid() {
        let ls = parse_layers(LAYERS).unwrap();
        let bar = ls
            .iter()
            .find(|l| l.layer.namespace == "quickshell:bar")
            .unwrap();
        assert_eq!(bar.monitor, "eDP-1");
        assert_eq!(bar.level, 1);
        assert!(bar.layer.pid > 0);
        assert_eq!(bar.layer.rect().w, 1440);
    }

    #[test]
    fn clients_and_version_fixtures() {
        let c = parse_clients(CLIENTS).unwrap();
        assert!(!c.is_empty());
        assert!(c.iter().all(|c| c.address.starts_with("0x")));
        let v: Version = serde_json::from_str(VERSION).unwrap();
        assert_eq!(v.version, "0.56.2");
    }

    #[test]
    fn unknown_fields_ignored() {
        let m = parse_monitors(
            r#"[{"name":"X","futureField":{"a":1},"scale":1.25,"width":1600,"height":1000}]"#,
        )
        .unwrap();
        assert_eq!(m[0].logical_size(), (1280, 800));
    }

    #[test]
    fn events() {
        assert_eq!(
            parse_event("activelayout>>at-translated-set-2-keyboard,English (US)"),
            Some(Event::ActiveLayout {
                keyboard: "at-translated-set-2-keyboard".into(),
                layout: "English (US)".into()
            })
        );
        assert_eq!(
            parse_event("focusedmon>>DP-1,3"),
            Some(Event::FocusedMon {
                monitor: "DP-1".into(),
                workspace: "3".into()
            })
        );
        assert_eq!(
            parse_event("activewindowv2>>629212f9ee40"),
            Some(Event::ActiveWindow {
                address: Some("0x629212f9ee40".into())
            })
        );
        assert_eq!(
            parse_event("activewindowv2>>"),
            Some(Event::ActiveWindow { address: None })
        );
        assert_eq!(
            parse_event("monitoradded>>HDMI-A-1"),
            Some(Event::MonitorAdded {
                name: "HDMI-A-1".into()
            })
        );
        assert_eq!(
            parse_event("monitorremoved>>HDMI-A-1"),
            Some(Event::MonitorRemoved {
                name: "HDMI-A-1".into()
            })
        );
        assert_eq!(
            parse_event("submap>>woints-keys"),
            Some(Event::Submap {
                name: "woints-keys".into()
            })
        );
        assert_eq!(
            parse_event("submap>>"),
            Some(Event::Submap {
                name: String::new()
            })
        );
        assert_eq!(parse_event("configreloaded>>"), Some(Event::ConfigReloaded));
        assert_eq!(
            parse_event("custom>>woints key 38 0"),
            Some(Event::Custom {
                payload: "woints key 38 0".into()
            })
        );
        assert_eq!(
            parse_event("custom>>which_keys>>1"),
            Some(Event::Custom {
                payload: "which_keys>>1".into()
            })
        );
        assert!(matches!(
            parse_event("workspace>>2"),
            Some(Event::Other { .. })
        ));
        assert_eq!(parse_event("garbage"), None);
    }

    #[test]
    fn focused_target_on_second_monitor() {
        let outs = [
            Monitor {
                name: "A".into(),
                width: 1920,
                height: 1080,
                scale: 1.0,
                ..Default::default()
            }
            .geom(),
            Monitor {
                name: "B".into(),
                x: 1920,
                width: 1440,
                height: 900,
                scale: 1.0,
                ..Default::default()
            }
            .geom(),
        ];
        let w = Client {
            address: "0x1".into(),
            at: [2000, 100],
            size: [400, 300],
            pid: 7,
            class: "foot".into(),
            ..Default::default()
        };
        let t = focused_target(&w, &outs, None);
        assert_eq!(t.output, OutputId("B".into()));
        assert_eq!(t.pid, Some(7));
        assert_eq!(t.rect, Rect::new(2000, 100, 400, 300));
    }

    fn fixture_snapshot() -> HyprSnapshot {
        HyprSnapshot {
            monitors: parse_monitors(MONITORS).unwrap(),
            active: parse_active_window(ACTIVE).unwrap(),
            clients: parse_clients(CLIENTS).unwrap(),
            layers: parse_layers(LAYERS).unwrap(),
        }
    }

    fn filter() -> LayerFilter {
        LayerFilter {
            levels: vec![1, 2, 3],
            ignore: vec!["awww-daemon".into(), "wayland-pipewire-idle-inhibit".into()],
        }
    }

    fn namespaces(ts: &[Target]) -> Vec<&str> {
        ts.iter()
            .filter_map(|t| match &t.surface {
                Surface::Layer { namespace, .. } => Some(namespace.as_str()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn layer_targets_from_fixture() {
        let s = fixture_snapshot();
        let t = select_targets(&s, Scope::Layers, &filter());
        assert_eq!(
            namespaces(&t),
            ["quickshell:bar", "quickshell:notifications-popups"]
        );
        assert!(t.iter().all(|t| t.surface.is_layer()));
        let bar = &t[0];
        assert_eq!(bar.rect, Rect::new(0, 0, 1440, 80));
        assert_eq!(bar.output, OutputId("eDP-1".into()));
        assert_eq!(bar.pid, Some(574865));
        // `top`/`overlay` only: the bar lives on `bottom`.
        let top = LayerFilter {
            levels: vec![2, 3],
            ..filter()
        };
        assert_eq!(
            namespaces(&select_targets(&s, Scope::Layers, &top)),
            ["quickshell:notifications-popups"]
        );
    }

    #[test]
    fn placeholder_skipped_even_when_not_ignored() {
        let s = fixture_snapshot();
        let f = LayerFilter {
            levels: vec![0, 1, 2, 3],
            ignore: vec!["awww-daemon".into()],
        };
        let t = select_targets(&s, Scope::Layers, &f);
        assert!(!namespaces(&t).contains(&"wayland-pipewire-idle-inhibit"));
    }

    #[test]
    fn scopes() {
        let s = fixture_snapshot();
        let f = filter();
        let focused = select_targets(&s, Scope::Focused, &f);
        assert_eq!(focused.len(), 1);
        assert!(!focused[0].surface.is_layer());
        let fl = select_targets(&s, Scope::FocusedAndLayers, &f);
        assert_eq!(fl.len(), 3);
        assert_eq!(fl[0], focused[0]);
        // Fixture monitor shows workspace 8, special hidden: one footclient.
        let all = select_targets(&s, Scope::All, &f);
        let tops: Vec<_> = all.iter().filter(|t| !t.surface.is_layer()).collect();
        assert_eq!(tops.len(), 1);
        assert_eq!(tops[0].class, "footclient");
    }

    #[test]
    fn all_two_monitors_and_special() {
        let mon = |id: i64, name: &str, x: i32, ws: i64, special: i64| Monitor {
            id,
            name: name.into(),
            x,
            width: 1920,
            height: 1080,
            scale: 1.0,
            focused: id == 0,
            active_workspace: WorkspaceRef {
                id: ws,
                name: ws.to_string(),
            },
            special_workspace: WorkspaceRef {
                id: special,
                name: String::new(),
            },
            ..Default::default()
        };
        let client = |addr: &str, monitor: i64, ws: i64, x: i32, floating: bool| Client {
            address: addr.into(),
            mapped: true,
            at: [x, 0],
            size: [800, 600],
            workspace: WorkspaceRef {
                id: ws,
                name: String::new(),
            },
            floating,
            monitor,
            class: addr.into(),
            ..Default::default()
        };
        let mut s = HyprSnapshot {
            monitors: vec![mon(0, "A", 0, 1, 0), mon(1, "B", 1920, 3, 0)],
            active: Some(client("a2", 0, 1, 900, false)),
            clients: vec![
                client("a1", 0, 1, 0, false),
                client("a2", 0, 1, 900, false),
                client("b1", 1, 3, 2000, true),
                client("hidden-ws", 0, 2, 0, false),
                client("sp", 0, -98, 100, true),
            ],
            layers: vec![],
        };
        let all = select_targets(&s, Scope::All, &filter());
        let classes: Vec<_> = all.iter().map(|t| t.class.as_str()).collect();
        assert_eq!(classes, ["a2", "a1", "b1"]);
        assert_eq!(all[2].output, OutputId("B".into()));
        s.monitors[0].special_workspace.id = -98;
        assert_eq!(select_targets(&s, Scope::All, &filter()).len(), 4);
        s.clients[0].hidden = true;
        assert_eq!(select_targets(&s, Scope::All, &filter()).len(), 3);
    }

    #[test]
    fn keyboards() {
        let k = parse_keyboards(
            r#"{"mice":[],"keyboards":[{"name":"kb","layout":"us,ru","variant":"colemak_dh,","active_layout_index":0,"main":true}]}"#,
        )
        .unwrap();
        assert_eq!(k[0].layout, "us,ru");
        assert!(k[0].main);
    }

    async fn dispatch_reply(reply: &'static str) -> Result<()> {
        let dir = std::env::temp_dir().join(format!(
            "woints-hypr-test-{}-{}",
            std::process::id(),
            reply.len()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let l = tokio::net::UnixListener::bind(dir.join(".socket.sock")).unwrap();
        let srv = tokio::spawn(async move {
            let (mut s, _) = l.accept().await.unwrap();
            let mut buf = [0u8; 256];
            let n = s.read(&mut buf).await.unwrap();
            assert_eq!(&buf[..n], b"dispatch movecursor 100 200");
            s.write_all(reply.as_bytes()).await.unwrap();
        });
        let r = Hypr::with_dir(&dir).dispatch("movecursor 100 200").await;
        srv.await.unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        r
    }

    /// Serve `n` connections, recording each request; reply with the given
    /// strings in order.
    async fn serve_script(
        tag: &str,
        replies: Vec<&'static str>,
    ) -> (PathBuf, tokio::task::JoinHandle<Vec<String>>) {
        let dir =
            std::env::temp_dir().join(format!("woints-hypr-test-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let l = tokio::net::UnixListener::bind(dir.join(".socket.sock")).unwrap();
        let srv = tokio::spawn(async move {
            let mut seen = Vec::new();
            for reply in replies {
                let (mut s, _) = l.accept().await.unwrap();
                let mut buf = [0u8; 256];
                let n = s.read(&mut buf).await.unwrap();
                seen.push(String::from_utf8_lossy(&buf[..n]).into_owned());
                s.write_all(reply.as_bytes()).await.unwrap();
            }
            seen
        });
        (dir, srv)
    }

    #[tokio::test]
    async fn move_cursor_uses_lua_dispatcher_when_eval_works() {
        let (dir, srv) = serve_script("lua", vec!["ok", "ok"]).await;
        let h = Hypr::with_dir(&dir);
        h.move_cursor(Point::new(25, 750)).await.unwrap();
        assert_eq!(h.dispatch_syntax().await, DispatchSyntax::Lua);
        let seen = srv.await.unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(
            seen,
            vec![
                "eval return 1".to_string(),
                "dispatch hl.dsp.cursor.move({ x = 25, y = 750 })".to_string(),
            ]
        );
    }

    #[tokio::test]
    async fn move_cursor_falls_back_to_legacy_when_eval_unknown() {
        let (dir, srv) = serve_script("legacy", vec!["unknown request", "ok"]).await;
        let h = Hypr::with_dir(&dir);
        h.move_cursor(Point::new(-1780, 120)).await.unwrap();
        assert_eq!(h.dispatch_syntax().await, DispatchSyntax::Legacy);
        let seen = srv.await.unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(seen[1], "dispatch movecursor -1780 120");
    }

    #[tokio::test]
    async fn with_syntax_skips_probe() {
        let (dir, srv) = serve_script("forced", vec!["ok"]).await;
        let h = Hypr::with_dir(&dir).with_syntax(DispatchSyntax::Legacy);
        h.move_cursor(Point::new(1, 2)).await.unwrap();
        let seen = srv.await.unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(seen, vec!["dispatch movecursor 1 2".to_string()]);
    }

    #[tokio::test]
    async fn dispatch_ok_and_error() {
        assert!(dispatch_reply("ok").await.is_ok());
        match dispatch_reply("Invalid dispatcher").await {
            Err(Error::Dispatch(m)) => assert_eq!(m, "Invalid dispatcher"),
            other => panic!("{other:?}"),
        }
    }

    fn live() -> bool {
        std::env::var("WOINTS_LIVE_TESTS").as_deref() == Ok("1")
    }

    #[tokio::test]
    async fn live_queries() {
        if !live() {
            return;
        }
        let h = Hypr::from_env().unwrap();
        let mons = h.monitors().await.unwrap();
        assert!(!mons.is_empty());
        let _ = h.active_window().await.unwrap();
        assert!(!h.version().await.unwrap().version.is_empty());
    }
}
