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
            vision: None,
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

fn dump_frame(name: &str, f: &woints_core::vision::Frame) {
    let Some(dir) = std::env::var_os("WOINTS_DUMP") else {
        return;
    };
    let size = tiny_skia::IntSize::from_wh(f.width, f.height).unwrap();
    let pm = tiny_skia::Pixmap::from_vec(f.rgba.clone(), size).unwrap();
    let path = std::path::Path::new(&dir).join(format!("{name}.png"));
    pm.save_png(&path).unwrap();
    eprintln!("dumped {}", path.display());
}

fn assert_size(f: &woints_core::vision::Frame, scale: f32) {
    let ew = f.rect.w as f32 * scale;
    let eh = f.rect.h as f32 * scale;
    eprintln!(
        "frame {}x{} rect {:?} scale {} (output {scale})",
        f.width, f.height, f.rect, f.scale
    );
    assert!((f.width as f32 - ew).abs() <= ew * 0.02 + 2.0, "width");
    assert!((f.height as f32 - eh).abs() <= eh * 0.02 + 2.0, "height");
}

/// Captures the focused toplevel and a `quickshell:bar` layer; reads pixels only.
#[tokio::test]
async fn captures_toplevel_and_layer() {
    use woints_core::model::{Surface, Target};
    use woints_core::ports::FrameCapture;
    if std::env::var("WOINTS_LIVE_TESTS").as_deref() != Ok("1") {
        return;
    }
    let hypr = woints_hypr::Hypr::from_env().unwrap();
    let mons = hypr.monitors().await.unwrap();
    let outputs: Vec<OutputGeom> = mons.iter().map(|m| m.geom()).collect();
    let focused = mons
        .iter()
        .find(|m| m.focused)
        .map(|m| OutputId(m.name.clone()));
    let (overlay, _events) = woints_overlay::spawn().expect("overlay starts");
    let link = overlay.capture_link();

    if let Some(w) = hypr.active_window().await.unwrap() {
        let target = woints_hypr::focused_target(&w, &outputs, focused.as_ref());
        let scale = outputs
            .iter()
            .find(|o| o.id == target.output)
            .unwrap()
            .scale;
        let cap = woints_overlay::ToplevelExportCapture(link.clone());
        let f = tokio::time::timeout(Duration::from_secs(2), cap.capture(&target, &outputs))
            .await
            .expect("in time")
            .expect("toplevel captured");
        assert_size(&f, scale);
        dump_frame("toplevel", &f);
        let t = std::time::Instant::now();
        cap.capture(&target, &outputs)
            .await
            .expect("second capture");
        eprintln!(
            "second capture {:.1} ms",
            t.elapsed().as_secs_f64() * 1000.0
        );
    }

    let layers = hypr.layers().await.unwrap();
    if let Some(l) = layers
        .iter()
        .find(|l| l.layer.namespace.starts_with("quickshell"))
    {
        let mon = mons.iter().find(|m| m.name == l.monitor).unwrap();
        let target = Target {
            surface: Surface::Layer {
                address: l.layer.address.clone(),
                namespace: l.layer.namespace.clone(),
            },
            rect: l.layer.rect(),
            output: OutputId(mon.name.clone()),
            pid: None,
            class: String::new(),
        };
        let cap = woints_overlay::ScreencopyRegionCapture(link);
        let f = tokio::time::timeout(Duration::from_secs(2), cap.capture(&target, &outputs))
            .await
            .expect("in time")
            .expect("layer captured");
        assert_size(&f, mon.geom().scale);
        dump_frame("layer", &f);
    }
}
