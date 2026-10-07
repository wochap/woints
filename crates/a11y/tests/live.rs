//! Talks to the live a11y bus and Hyprland. Opt-in:
//! `WOINTS_LIVE_TESTS=1 cargo test -p woints-a11y --test live -- --nocapture`.
//! Each test skips (with a message) when its app is not running; start them
//! with `gtk3-widget-factory`, `qt6ct`, `gnome-calculator`. Read-only: no
//! window is opened, focused or acted on.

use std::sync::Arc;
use std::time::{Duration, Instant};

use woints_a11y::{A11yConnection, AtspiProvider};
use woints_core::geom::OutputId;
use woints_core::model::{Surface, Target};
use woints_core::ports::{Budget, Collected};

fn live() -> bool {
    std::env::var("WOINTS_LIVE_TESTS").as_deref() == Ok("1")
}

async fn target(class: &str) -> Option<Target> {
    let clients = woints_hypr::Hypr::from_env().ok()?.clients().await.ok()?;
    let c = clients.into_iter().find(|c| c.class == class && c.mapped)?;
    Some(Target {
        surface: Surface::Toplevel {
            address: c.address.clone(),
        },
        rect: c.rect(),
        output: OutputId(String::new()),
        pid: c.pid(),
        class: c.class.clone(),
    })
}

async fn collect(p: &AtspiProvider, class: &str, budget_ms: u64) -> Option<Collected> {
    let Some(t) = target(class).await else {
        eprintln!("skip: no {class} window");
        return None;
    };
    let budget = Budget {
        deadline: Instant::now() + Duration::from_millis(budget_ms),
    };
    let c = p.collect_target(&t, budget).await.expect("collect");
    eprintln!(
        "{class}: {} elements, report {:?}, timings {:?}",
        c.elements.len(),
        c.report,
        c.timings
    );
    for e in &c.elements {
        assert!(
            e.rect.intersects(&t.rect),
            "{:?} outside {:?}",
            e.rect,
            t.rect
        );
    }
    Some(c)
}

fn provider() -> AtspiProvider {
    AtspiProvider::new(Arc::new(A11yConnection::new()))
}

#[tokio::test]
async fn pairs_gtk3_widget_factory() {
    if !live() {
        return;
    }
    let Some(c) = collect(&provider(), "gtk3-widget-factory", 200).await else {
        return;
    };
    assert_eq!(c.report.paired.as_deref(), Some("pid"));
}

#[tokio::test]
async fn collection_gtk3_and_qt() {
    if !live() {
        return;
    }
    let p = provider();
    if let Some(c) = collect(&p, "gtk3-widget-factory", 200).await {
        assert_eq!(c.report.used.as_deref(), Some("collection"));
        assert!(c.elements.len() >= 30, "{}", c.elements.len());
        // Second run reuses the connection.
        let c2 = collect(&p, "gtk3-widget-factory", 200).await.unwrap();
        assert!(c2.timings.get("collect.connect").unwrap() < 1.0);
        assert!(c2.timings.get("collect.match").unwrap() < 10.0);
    }
    if let Some(c) = collect(&p, "qt6ct", 200).await {
        assert_eq!(c.report.used.as_deref(), Some("collection"));
        assert!(c.elements.len() >= 50, "{}", c.elements.len());
        // Collapsed combo popups: no list item at the window's top-left row.
        let t = target("qt6ct").await.unwrap();
        assert!(
            !c.elements
                .iter()
                .any(|e| e.role == "list item" && e.rect.x == t.rect.x && e.rect.y == t.rect.y),
            "phantom combo popup items kept"
        );
    }
}

#[tokio::test]
async fn walk_gnome_calculator() {
    if !live() {
        return;
    }
    let p = provider();
    let Some(_) = collect(&p, "org.gnome.Calculator", 200).await else {
        return;
    };
    let c = collect(&p, "org.gnome.Calculator", 200).await.unwrap();
    assert_eq!(c.report.used.as_deref(), Some("walk"));
    assert!(c.elements.len() >= 30, "{}", c.elements.len());
    assert!(c.timings.get("collect.match").unwrap() < 40.0);
}

#[tokio::test]
async fn tiny_budget_truncates() {
    if !live() {
        return;
    }
    let mut p = provider();
    p.max_nodes = 5;
    let Some(c) = collect(&p, "org.gnome.Calculator", 200).await else {
        return;
    };
    assert!(c.report.truncated);
}

/// `WOINTS_LIVE_CLASS=<class>`: collect any running window and print the report.
#[tokio::test]
async fn any_class() {
    if !live() {
        return;
    }
    let Ok(class) = std::env::var("WOINTS_LIVE_CLASS") else {
        return;
    };
    let p = provider();
    collect(&p, &class, 200).await;
    collect(&p, &class, 200).await;
}
