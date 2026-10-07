//! Live Wayland tests. Opt-in: `WOINTS_LIVE_TESTS=1 cargo test -p woints-overlay --test live`.
//! `maps_on_focused_output_for_500ms` steals keyboard focus for ~500 ms on
//! the focused output; the virtual pointer test sends no pointer events.

use std::time::Duration;

use woints_core::act::VirtualPointer;
use woints_core::geom::{OutputGeom, OutputId, Point, Rect};
use woints_core::ports::{Badge, BadgeStyle, Overlay, Scene};
use woints_core::theme::Theme;

#[tokio::test]
async fn maps_on_focused_output_for_500ms() {
    if std::env::var("WOINTS_LIVE_TESTS").as_deref() != Ok("1") {
        return;
    }
    let mons = woints_hypr::Hypr::from_env()
        .unwrap()
        .monitors()
        .await
        .unwrap();
    let m = mons.iter().find(|m| m.focused).expect("focused monitor");
    let g: OutputGeom = m.geom();
    let origin: Point<_> = g.origin;
    let scene = Scene {
        outputs: vec![g.clone()],
        badges: vec![Badge {
            label: "as".into(),
            rect: Rect::new(origin.x + 40, origin.y + 40, 22, 17),
            output: OutputId(m.name.clone()),
            matched: 0,
            style: BadgeStyle::Normal,
        }],
        chip: None,
        pinned: None,
        scroll: None,
        theme: Theme::default(),
    };
    let (overlay, _events) = woints_overlay::spawn().expect("overlay starts");
    let t = tokio::time::timeout(Duration::from_secs(2), overlay.show(scene)).await;
    let timings = t.expect("committed in time").expect("show ok");
    tokio::time::sleep(Duration::from_millis(500)).await;
    overlay.hide().unwrap();
    assert!(timings.render_ms >= 0.0);
}

#[tokio::test]
async fn virtual_pointer_binds_and_roundtrips() {
    if std::env::var("WOINTS_LIVE_TESTS").as_deref() != Ok("1") {
        return;
    }
    let (overlay, _events) = woints_overlay::spawn().expect("overlay starts");
    assert!(
        overlay.caps().pointer,
        "zwlr_virtual_pointer_manager_v1 bound"
    );
    eprintln!("clipboard protocol: {:?}", overlay.caps().clipboard);
    let link = overlay.pointer_link();
    tokio::time::timeout(Duration::from_secs(2), link.roundtrip())
        .await
        .expect("roundtrip in time")
        .expect("roundtrip ok");
}
