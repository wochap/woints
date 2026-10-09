//! Read-only toolkit probe used for `docs/toolkits.md`:
//! `cargo run -p woints-a11y --example probe -- <hyprland-class> [--register]`.
//! Counts what Collection and a full walk see on the window's frame and on
//! any `document web` node; `--register` first registers an
//! `object:state-changed` listener with the Registry.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use woints_a11y::dbus::{self, Obj};
use woints_a11y::rule::{MatchRule, has_state, state};
use woints_a11y::walk::{self, NodeData, WalkLimits};
use woints_a11y::{A11yConnection, AtspiProvider};
use woints_core::geom::OutputId;
use woints_core::model::{Surface, Target};
use woints_core::ports::CollectReport;

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let class = args
        .first()
        .expect("usage: probe <class> [--register]")
        .clone();
    let register = args.iter().any(|a| a == "--register");
    let hypr = woints_hypr::Hypr::from_env().expect("hyprland");
    let c = hypr
        .clients()
        .await
        .unwrap()
        .into_iter()
        .find(|c| c.class == class)
        .expect("no such window");
    let target = Target {
        surface: Surface::Toplevel {
            address: c.address.clone(),
        },
        rect: c.rect(),
        output: OutputId(String::new()),
        pid: c.pid(),
        class: c.class.clone(),
    };
    let a11y = Arc::new(A11yConnection::new());
    let p = AtspiProvider::new(a11y.clone());
    let (conn, generation) = a11y.get().await.unwrap();
    let mut report = CollectReport::default();
    let (app, frame) = p
        .pair(&conn, generation, &target, &mut report)
        .await
        .unwrap()
        .expect("no a11y app");
    println!(
        "app {} {} {} paired {:?}",
        app.name, app.toolkit, app.version, report.paired
    );

    if register {
        let reg = Obj::new(dbus::REGISTRY, "/org/a11y/atspi/registry").unwrap();
        let r: zbus::Result<()> = dbus::call(
            &conn,
            &reg,
            dbus::REGISTRY,
            "RegisterEvent",
            &("object:state-changed", Vec::<String>::new(), ""),
        )
        .await;
        println!("RegisterEvent(object:state-changed): {r:?}");
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    let t = Instant::now();
    let m = dbus::get_matches(&conn, &frame, &MatchRule::interactive()).await;
    println!(
        "GetMatches(frame): {:?} in {:.1} ms",
        m.as_ref().map(Vec::len),
        t.elapsed().as_secs_f64() * 1000.0
    );

    let web = Mutex::new(Vec::new());
    let showing_interactive = Mutex::new(0usize);
    let limits = WalkLimits {
        max_nodes: 20_000,
        max_depth: 200,
        concurrency: walk::CONCURRENCY,
        deadline: Instant::now() + Duration::from_secs(20),
        skip_web_content: false,
    };
    let t = Instant::now();
    let (conn_r, web_r, si_r) = (&conn, &web, &showing_interactive);
    let r = walk::walk(frame.clone(), limits, |o: Obj| async move {
        let (role, st, kids) = futures::join!(
            dbus::role(conn_r, &o),
            dbus::state(conn_r, &o),
            dbus::children(conn_r, &o)
        );
        let role = role.ok()?;
        let st = st.ok()?;
        if woints_a11y::rule::is_web_content(role) {
            web_r.lock().unwrap().push(o.clone());
        }
        let showing = has_state(&st, state::SHOWING) && has_state(&st, state::VISIBLE);
        if showing && woints_a11y::rule::is_interactive(role) {
            *si_r.lock().unwrap() += 1;
        }
        // Descend everything so hidden web content is counted too.
        Some(NodeData {
            role,
            showing: true,
            children: kids.unwrap_or_default(),
        })
    })
    .await;
    println!(
        "walk: visited {} interactive {} showing+interactive {} truncated {} in {:.1} ms",
        r.visited,
        r.found.len(),
        showing_interactive.lock().unwrap(),
        r.truncated,
        t.elapsed().as_secs_f64() * 1000.0
    );
    let web = web.into_inner().unwrap();
    for w in &web {
        let m = dbus::get_matches(&conn, w, &MatchRule::interactive()).await;
        println!("GetMatches(web node {w}): {:?}", m.as_ref().map(Vec::len));
    }
}
