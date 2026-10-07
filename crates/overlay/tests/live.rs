//! Maps a real layer surface. Opt-in: `WOINTS_LIVE_TESTS=1 cargo test -p woints-overlay --test live`.
//! Steals keyboard focus for ~500 ms on the focused output.

use std::time::Duration;

use woints_core::geom::{OutputGeom, OutputId, Point, Rect};
use woints_core::ports::{Badge, Overlay, Scene};
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
        }],
        chip: None,
        theme: Theme::default(),
    };
    let (overlay, _events) = woints_overlay::spawn().expect("overlay starts");
    let t = tokio::time::timeout(Duration::from_secs(2), overlay.show(scene)).await;
    let timings = t.expect("committed in time").expect("show ok");
    tokio::time::sleep(Duration::from_millis(500)).await;
    overlay.hide().unwrap();
    assert!(timings.render_ms >= 0.0);
}
