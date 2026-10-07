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

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct Version {
    pub version: String,
    pub tag: String,
    pub commit: String,
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

/// Request client for `.socket.sock`. Cheap to clone; one connection per request.
#[derive(Debug, Clone)]
pub struct Hypr {
    dir: PathBuf,
}

impl Hypr {
    pub fn from_env() -> Result<Self> {
        Ok(Self { dir: socket_dir()? })
    }

    pub fn with_dir(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
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

    pub async fn version(&self) -> Result<Version> {
        self.json("version").await
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

/// [`TargetSelector`] backed by Hyprland. Only [`Scope::Focused`] in M1.
#[derive(Debug, Clone)]
pub struct HyprSelector {
    pub hypr: Hypr,
    pub outputs: Vec<OutputGeom>,
    pub focused_output: Option<OutputId>,
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

impl TargetSelector for HyprSelector {
    async fn select(&self, scope: Scope) -> std::result::Result<Vec<Target>, SelectError> {
        if scope != Scope::Focused {
            return Err(SelectError::Unsupported(scope));
        }
        let win = self
            .hypr
            .active_window()
            .await
            .map_err(|e| SelectError::Backend(e.to_string()))?;
        Ok(win
            .into_iter()
            .map(|w| focused_target(&w, &self.outputs, self.focused_output.as_ref()))
            .collect())
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
