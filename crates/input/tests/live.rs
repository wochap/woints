//! Live pointer tests (tasks 6.1, 6.2). Opt-in:
//! `WOINTS_LIVE_TESTS=1 cargo test -p woints-input --test live -- --test-threads=1`.
//!
//! They launch real apps and click, drag and scroll in the live Hyprland
//! session: do not run them while working in that session. See
//! `docs/verify-m3.md`.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::process::{Child, Command};
use std::sync::Arc;
use std::time::{Duration, Instant};

use woints_a11y::{A11yConnection, AtspiActuator, AtspiProvider};
use woints_core::act::{Axis, InputConfig};
use woints_core::geom::OutputGeom;
use woints_core::model::{Action, Element};
use woints_core::ports::Budget;
use woints_core::proto::{State, StatusReply, socket_path};
use woints_hypr::{Client, Hypr, focused_target};
use woints_input::{Backend, MovecursorBackend, PointerActuator};
use woints_overlay::{OverlayHandle, PointerLink};

fn live() -> bool {
    std::env::var("WOINTS_LIVE_TESTS").as_deref() == Ok("1")
}

struct App {
    child: Child,
    client: Client,
}

impl Drop for App {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Start `cmd` and wait for its toplevel (matched by `class`).
// The child is killed and reaped by `App::drop`.
#[allow(clippy::zombie_processes)]
async fn spawn(hypr: &Hypr, cmd: &[&str], class: &str) -> App {
    let child = Command::new(cmd[0])
        .args(&cmd[1..])
        .spawn()
        .unwrap_or_else(|e| panic!("{}: {e}", cmd[0]));
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let clients = hypr.clients().await.expect("clients");
        if let Some(c) = clients
            .into_iter()
            .find(|c| c.class.eq_ignore_ascii_case(class) && c.pid() == Some(child.id()))
        {
            return App { child, client: c };
        }
        assert!(Instant::now() < deadline, "{class} did not map");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

async fn refresh(hypr: &Hypr, app: &mut App) {
    let clients = hypr.clients().await.expect("clients");
    if let Some(c) = clients
        .into_iter()
        .find(|c| c.address == app.client.address)
    {
        app.client = c;
    }
}

async fn outputs(hypr: &Hypr) -> Vec<OutputGeom> {
    hypr.monitors()
        .await
        .expect("monitors")
        .iter()
        .filter(|m| !m.disabled)
        .map(|m| m.geom())
        .collect()
}

struct Rig {
    hypr: Hypr,
    _overlay: OverlayHandle,
    pointer: PointerActuator<Backend<Hypr, PointerLink>>,
    provider: AtspiProvider,
    atspi: AtspiActuator,
}

fn rig() -> Rig {
    let hypr = Hypr::from_env().expect("Hyprland");
    let (overlay, _events) = woints_overlay::spawn().expect("overlay");
    assert!(overlay.caps().pointer, "virtual pointer bound");
    let pointer = PointerActuator::new(
        Backend::Movecursor(MovecursorBackend {
            ipc: hypr.clone(),
            pointer: overlay.pointer_link(),
        }),
        &InputConfig::default(),
    );
    let conn = Arc::new(A11yConnection::new());
    let provider = AtspiProvider::new(conn.clone());
    let atspi = AtspiActuator::new(conn, provider.apps_cache());
    Rig {
        hypr,
        _overlay: overlay,
        pointer,
        provider,
        atspi,
    }
}

impl Rig {
    async fn elements(&self, app: &App) -> Vec<Element> {
        let outs = outputs(&self.hypr).await;
        let target = focused_target(&app.client, &outs, None);
        let budget = Budget {
            deadline: Instant::now() + Duration::from_millis(500),
        };
        self.provider
            .collect_target(&target, budget)
            .await
            .expect("collect")
            .elements
    }

    async fn find(&self, app: &App, role: &str) -> Element {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(e) = self
                .elements(app)
                .await
                .into_iter()
                .find(|e| e.role == role)
            {
                return e;
            }
            assert!(Instant::now() < deadline, "no {role} element");
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
    }
}

// 6.1 ---------------------------------------------------------------------

// TODO(verify): never executed; check the click lands on every output.
#[tokio::test]
async fn left_click_on_widget_factory_button_on_each_output() {
    if !live() {
        return;
    }
    let r = rig();
    let mut app = spawn(&r.hypr, &["gtk3-widget-factory"], "gtk3-widget-factory").await;
    for m in r
        .hypr
        .monitors()
        .await
        .unwrap()
        .iter()
        .filter(|m| !m.disabled)
    {
        r.hypr
            .dispatch(&format!(
                "movewindow mon:{},address:{}",
                m.name, app.client.address
            ))
            .await
            .expect("movewindow");
        tokio::time::sleep(Duration::from_millis(300)).await;
        refresh(&r.hypr, &mut app).await;
        let b = r.find(&app, "push button").await;
        r.pointer
            .act(Action::Click, b.rect)
            .await
            .unwrap_or_else(|e| panic!("click on {}: {e}", m.name));
        let active = r
            .hypr
            .active_window()
            .await
            .unwrap()
            .expect("active window");
        assert_eq!(
            active.address, app.client.address,
            "click focused the app on {}",
            m.name
        );
    }
}

// TODO(verify): never executed; needs an output left of or above the origin.
#[tokio::test]
async fn click_on_window_at_negative_offset() {
    if !live() {
        return;
    }
    let r = rig();
    let Some(m) = r
        .hypr
        .monitors()
        .await
        .unwrap()
        .into_iter()
        .find(|m| !m.disabled && (m.x < 0 || m.y < 0))
    else {
        eprintln!("skipped: no output at a negative offset");
        return;
    };
    let mut app = spawn(&r.hypr, &["gtk3-widget-factory"], "gtk3-widget-factory").await;
    r.hypr
        .dispatch(&format!(
            "movewindow mon:{},address:{}",
            m.name, app.client.address
        ))
        .await
        .expect("movewindow");
    tokio::time::sleep(Duration::from_millis(300)).await;
    refresh(&r.hypr, &mut app).await;
    let b = r.find(&app, "push button").await;
    assert!(b.rect.x < 0 || b.rect.y < 0, "element at {:?}", b.rect);
    r.pointer.act(Action::Click, b.rect).await.expect("click");
}

// TODO(verify): never executed; double click must open the file in Thunar.
#[tokio::test]
async fn double_click_opens_file_in_thunar() {
    if !live() {
        return;
    }
    let r = rig();
    let dir = std::env::temp_dir().join("woints-live-thunar");
    std::fs::create_dir_all(dir.join("inner")).unwrap();
    let dir_s = dir.to_string_lossy().into_owned();
    let mut app = spawn(&r.hypr, &["thunar", &dir_s], "thunar").await;
    let item = r.find(&app, "table cell").await;
    r.pointer
        .act(Action::Double, item.rect)
        .await
        .expect("double");
    tokio::time::sleep(Duration::from_millis(500)).await;
    refresh(&r.hypr, &mut app).await;
    assert!(
        app.client.title.contains("inner"),
        "title: {}",
        app.client.title
    );
}

// TODO(verify): never executed; hover must show a tooltip.
#[tokio::test]
async fn hover_shows_tooltip() {
    if !live() {
        return;
    }
    let r = rig();
    let app = spawn(&r.hypr, &["gtk3-widget-factory"], "gtk3-widget-factory").await;
    let b = r.find(&app, "push button").await;
    r.pointer.act(Action::Hover, b.rect).await.expect("hover");
    tokio::time::sleep(Duration::from_millis(1500)).await;
    // The tooltip itself is checked by eye; the pointer must still be on the button.
    let pos = r.hypr.cursor_pos().await.unwrap();
    assert_eq!(pos, b.rect.center());
    drop(app);
}

// 6.2 ---------------------------------------------------------------------

// TODO(verify): never executed; check the list order changes.
#[tokio::test]
async fn drag_reorders_list_item() {
    if !live() {
        return;
    }
    let r = rig();
    let app = spawn(&r.hypr, &["gtk3-widget-factory"], "gtk3-widget-factory").await;
    let items: Vec<Element> = r
        .elements(&app)
        .await
        .into_iter()
        .filter(|e| e.role == "list item")
        .collect();
    assert!(items.len() >= 2, "need two list items");
    let (a, b) = (&items[0], &items[items.len() - 1]);
    r.pointer
        .drag(a.rect.center(), b.rect.center())
        .await
        .expect("drag");
    tokio::time::sleep(Duration::from_millis(300)).await;
    let after: Vec<String> = r
        .elements(&app)
        .await
        .into_iter()
        .filter(|e| e.role == "list item")
        .map(|e| e.name)
        .collect();
    let before: Vec<String> = items.iter().map(|e| e.name.clone()).collect();
    assert_ne!(after, before, "order unchanged");
}

// TODO(verify): never executed; the overlay is mapped with keyboard focus
// during the scroll, as in scroll mode.
#[tokio::test]
async fn scroll_moves_scroll_pane_under_overlay() {
    if !live() {
        return;
    }
    use woints_core::ports::{Overlay, Scene, ScrollFrame};
    let r = rig();
    let app = spawn(&r.hypr, &["gtk3-widget-factory"], "gtk3-widget-factory").await;
    let pane = r.find(&app, "scroll pane").await;
    let before = r.atspi.scroll_value(&pane).await.expect("scrollbar value");
    let outs = outputs(&r.hypr).await;
    let out = woints_core::geom::pick_output(pane.rect, &outs, None).unwrap();
    let scene = Scene {
        outputs: outs,
        badges: vec![],
        chip: None,
        pinned: None,
        scroll: Some(ScrollFrame {
            rect: pane.rect,
            output: out,
            value: Some(before),
        }),
        theme: Default::default(),
        keyboard: false,
    };
    r._overlay.show(scene).await.expect("overlay shown");
    r.pointer.scroll_enter(pane.rect).await.expect("enter");
    r.pointer
        .scroll_key(Axis::Vertical, 1)
        .await
        .expect("scroll");
    tokio::time::sleep(Duration::from_millis(300)).await;
    r._overlay.hide().unwrap();
    let after = r.atspi.scroll_value(&pane).await.expect("scrollbar value");
    assert!(after.current > before.current, "{before:?} -> {after:?}");
}

/// One request to the running `wointsd`.
fn daemon(req: serde_json::Value) -> serde_json::Value {
    let mut s = UnixStream::connect(socket_path()).expect("wointsd running");
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    writeln!(s, "{req}").unwrap();
    let mut line = String::new();
    BufReader::new(&s).read_line(&mut line).unwrap();
    serde_json::from_str(&line).unwrap()
}

fn daemon_state() -> State {
    let v = daemon(serde_json::json!({"cmd":"status"}));
    serde_json::from_value::<StatusReply>(v).unwrap().state
}

// TODO(verify): never executed; needs a running `wointsd`, and assumes
// `sendshortcut` reaches the overlay's exclusive-keyboard layer surface.
#[tokio::test]
async fn escape_in_drag_phase_two_restores_phase_one() {
    if !live() {
        return;
    }
    let hypr = Hypr::from_env().expect("Hyprland");
    let _app = spawn(&hypr, &["gtk3-widget-factory"], "gtk3-widget-factory").await;
    let r = daemon(serde_json::json!({"cmd":"hints","action":"drag"}));
    assert_eq!(r["state"], "showing", "{r}");
    // `a` is always the most central element's label.
    hypr.dispatch("sendshortcut , a, ").await.expect("key a");
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(daemon_state(), State::ShowingSecond);
    hypr.dispatch("sendshortcut , Escape, ")
        .await
        .expect("Escape");
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(daemon_state(), State::Showing);
    daemon(serde_json::json!({"cmd":"cancel"}));
}
