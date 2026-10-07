//! One pass: `hypr.query → select → collect → label → place → render → commit`.

use std::future::Future;
use std::time::{Duration, Instant};

use tracing::Instrument;

use woints_core::geom::{
    OutputGeom, OutputId, Rect, WindowGeom, WindowLocal, global_to_output, output_to_global,
    window_to_global,
};
use woints_core::labels::{PriorityItem, assign, labels, priority};
use woints_core::merge::{MergeParams, merge};
use woints_core::model::{Action, Element, ElementId, ElementSource, Scope, Target, Timings};
use woints_core::place::{PlaceInput, badge_size, place};
use woints_core::ports::{
    Badge, BadgeStyle, Budget, Chip, CollectReport, Collected, ElementProvider, Outline,
    ProviderError, Scene, SelectError,
};
use woints_core::theme::Theme;
use woints_core::vision::VisionReport;
use woints_hypr::{Client, focused_target};

/// Time one provider may spend on one target (design D17).
pub const PROVIDER_BUDGET: Duration = Duration::from_millis(60);

/// Everything a run produces.
#[derive(Debug, Clone)]
pub struct Output {
    pub targets: Vec<Target>,
    pub elements: Vec<Element>,
    /// Label per element index (empty when the element got none).
    pub labels: Vec<String>,
    pub scene: Scene,
    pub timings: Timings,
    pub reports: Vec<CollectReport>,
    pub vision: VisionReport,
}

/// The vision fallback as the pipeline sees it (design D18): a policy
/// decision per target, then capture → detect.
pub trait VisionStage: Sync {
    /// Whether vision runs for `target` given `atspi` usable elements, and why.
    fn decide(&self, atspi: usize, target: &Target, served: bool) -> (bool, &'static str);
    fn collect(
        &self,
        target: &Target,
        outputs: &[OutputGeom],
    ) -> impl Future<Output = Result<Collected, ProviderError>> + Send;
    fn merge_params(&self) -> MergeParams;
}

/// No vision: built without the `vision` feature or no capture backend.
pub struct NoVision;

impl VisionStage for NoVision {
    fn decide(&self, _: usize, _: &Target, _: bool) -> (bool, &'static str) {
        (false, "vision_unavailable")
    }
    async fn collect(&self, _: &Target, _: &[OutputGeom]) -> Result<Collected, ProviderError> {
        Err(ProviderError::Unavailable("vision".into()))
    }
    fn merge_params(&self) -> MergeParams {
        MergeParams::default()
    }
}

/// Vision provider over the overlay's capture link plus merge parameters.
#[cfg(feature = "vision")]
pub struct Vision {
    pub provider: woints_vision::VisionProvider<
        woints_vision::CompositeCapture<
            woints_overlay::ToplevelExportCapture,
            woints_overlay::ScreencopyRegionCapture,
        >,
    >,
    pub merge: MergeParams,
}

#[cfg(feature = "vision")]
impl Vision {
    pub fn new(link: woints_overlay::CaptureLink, config: &woints_core::config::Config) -> Self {
        let capture = woints_vision::CompositeCapture {
            toplevel: woints_overlay::ToplevelExportCapture(link.clone()),
            region: woints_overlay::ScreencopyRegionCapture(link),
            timeout: Duration::from_millis(config.vision.budget_ms),
        };
        Self {
            provider: woints_vision::VisionProvider::new(
                capture,
                Box::new(woints_vision::ClassicalDetector::new()),
                config.vision.clone(),
            ),
            merge: config.merge,
        }
    }
}

#[cfg(feature = "vision")]
impl VisionStage for Vision {
    fn decide(&self, atspi: usize, target: &Target, served: bool) -> (bool, &'static str) {
        let d = woints_vision::decide_vision(
            self.provider.config(),
            atspi,
            &target.class,
            target.surface.is_layer(),
            served,
        );
        (d.run, d.reason)
    }
    async fn collect(
        &self,
        target: &Target,
        outputs: &[OutputGeom],
    ) -> Result<Collected, ProviderError> {
        self.provider
            .collect_on(target, outputs, self.provider.budget())
            .await
    }
    fn merge_params(&self) -> MergeParams {
        self.merge
    }
}

/// Inputs gathered by the `hypr.query` stage.
pub struct HyprState {
    pub outputs: Vec<OutputGeom>,
    pub focused_output: Option<OutputId>,
    pub active: Option<Client>,
}

/// Demo provider: five boxes at the focused window's corners and centre.
pub struct DemoProvider;

pub const DEMO_BOX: (i32, i32) = (64, 28);
pub const DEMO_INSET: i32 = 12;

impl DemoProvider {
    pub fn boxes(target: &Target) -> Vec<Rect<WindowLocal>> {
        let (bw, bh) = DEMO_BOX;
        let (w, h) = (target.rect.w, target.rect.h);
        let i = DEMO_INSET;
        vec![
            Rect::new(i, i, bw, bh),
            Rect::new(w - i - bw, i, bw, bh),
            Rect::new((w - bw) / 2, (h - bh) / 2, bw, bh),
            Rect::new(i, h - i - bh, bw, bh),
            Rect::new(w - i - bw, h - i - bh, bw, bh),
        ]
    }
}

impl ElementProvider for DemoProvider {
    fn name(&self) -> &'static str {
        "demo"
    }

    async fn collect(&self, target: &Target, _budget: Budget) -> Result<Collected, ProviderError> {
        let win = WindowGeom {
            origin: target.rect.origin(),
        };
        let names = [
            "top-left",
            "top-right",
            "centre",
            "bottom-left",
            "bottom-right",
        ];
        let items: Vec<Element> = Self::boxes(target)
            .into_iter()
            .zip(names)
            .enumerate()
            .map(|(n, (r, name))| Element {
                id: ElementId(n as u32 + 1),
                rect: window_to_global(r, &win),
                role: "demo".into(),
                name: name.into(),
                source: ElementSource::Demo,
                actions: Action::supported(&[]),
                caps: vec![],
                confidence: 1.0,
                handle: None,
            })
            .collect();
        Ok(Collected {
            elements: items,
            report: CollectReport {
                provider: "demo".into(),
                ..Default::default()
            },
            timings: Timings::default(),
        })
    }
}

/// Whether `e` gets a hint for `action`: scroll hints only containers (all
/// elements for pointer-only targets), other actions skip containers and
/// elements that do not support them.
pub fn hintable(e: &Element, action: Action) -> bool {
    let container = woints_a11y::rule::is_scroll_container(&e.role);
    match action {
        Action::Scroll => container || e.caps.is_empty(),
        a => !container && e.actions.contains(&a),
    }
}

/// Chip text for `action`'s first phase: mode, sub-text, drag swatch.
pub fn chip_mode(action: Action) -> (&'static str, Option<&'static str>) {
    match action {
        Action::Click => ("click", None),
        Action::Right => ("right-click", None),
        Action::Middle => ("middle", None),
        Action::Double => ("double", None),
        Action::Hover => ("hover", None),
        Action::Focus => ("focus", None),
        Action::Drag => ("drag 1/2", Some("pick source")),
        Action::Scroll => ("scroll", Some("pick container")),
        Action::Copy => ("copy-text", None),
    }
}

pub fn set_chip(scene: &mut Scene, mode: &str, sub: Option<&str>, swatch: bool) {
    if let Some(c) = scene.chip.as_mut() {
        c.mode = mode.to_owned();
        c.sub = sub.map(str::to_owned);
        c.swatch = swatch;
        c.typed.clear();
    }
}

/// Drag phase 2 (design §07): fresh labels for every element but `source`
/// from an alphabet without the source label's glyphs; the source badge
/// keeps its label, pinned in the drag style with an outline.
#[allow(clippy::too_many_arguments)]
pub fn second_scene(
    first: &Scene,
    elements: &[Element],
    source: usize,
    source_label: &str,
    targets: &[Target],
    focused_output: Option<&OutputId>,
    scope: Scope,
    theme: &Theme,
) -> (Scene, Vec<String>) {
    let mut t = theme.clone();
    t.hint.alphabet = theme
        .hint
        .alphabet
        .chars()
        .filter(|c| !source_label.contains(*c))
        .collect();
    let rest: Vec<Element> = elements
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != source)
        .map(|(_, e)| e.clone())
        .collect();
    let mut timings = Timings::default();
    let (mut scene, rest_labels) = build_scene(
        &rest,
        targets,
        &first.outputs,
        focused_output,
        scope,
        &t,
        &mut timings,
    );
    scene.theme = theme.clone();
    let mut labels = rest_labels;
    labels.insert(source, String::new());
    if let Some(b) = first.badges.iter().find(|b| b.label == source_label) {
        scene.badges.push(Badge {
            style: BadgeStyle::Drag,
            matched: 0,
            ..b.clone()
        });
    }
    let e = &elements[source];
    scene.pinned =
        woints_core::geom::pick_output(e.rect, &first.outputs, focused_output).map(|output| {
            Outline {
                rect: e.rect,
                output,
            }
        });
    scene.chip = first.chip.clone();
    set_chip(&mut scene, "drag 2/2", Some("pick destination"), true);
    (scene, labels)
}

fn stage<T>(timings: &mut Timings, name: &str, started: Instant, v: T) -> T {
    timings.record(name, started.elapsed());
    v
}

/// Label and place `elements`, producing the scene to draw.
pub fn build_scene(
    elements: &[Element],
    targets: &[Target],
    outputs: &[OutputGeom],
    focused_output: Option<&OutputId>,
    scope: Scope,
    theme: &Theme,
    timings: &mut Timings,
) -> (Scene, Vec<String>) {
    // label
    let t = Instant::now();
    let alphabet: Vec<char> = theme.hint.alphabet.chars().collect();
    let focus = targets
        .first()
        .map(|t| t.rect.center())
        .unwrap_or(woints_core::Point::new(0, 0));
    let items: Vec<PriorityItem> = elements
        .iter()
        .map(|e| PriorityItem {
            rect: e.rect,
            is_layer: false,
        })
        .collect();
    let order = priority(&items, focus);
    let ls = labels(elements.len(), alphabet.len());
    let assigned = assign(&ls, &order);
    let label_strs: Vec<String> = assigned
        .iter()
        .map(|l| l.as_ref().map(|l| l.render(&alphabet)).unwrap_or_default())
        .collect();
    stage(timings, "label", t, ());

    // place, per output
    let t = Instant::now();
    let mut badges = Vec::with_capacity(elements.len());
    for out in outputs {
        let idx: Vec<usize> = (0..elements.len())
            .filter(|&i| {
                woints_core::geom::pick_output(elements[i].rect, outputs, focused_output).as_ref()
                    == Some(&out.id)
            })
            .collect();
        if idx.is_empty() {
            continue;
        }
        let inputs: Vec<PlaceInput> = idx
            .iter()
            .map(|&i| PlaceInput {
                target: global_to_output(elements[i].rect, out),
                size: badge_size(label_strs[i].chars().count(), &theme.hint),
            })
            .collect();
        let size = out.logical_size;
        for (k, p) in place(&inputs, size, &theme.hint).into_iter().enumerate() {
            badges.push(Badge {
                label: label_strs[idx[k]].clone(),
                rect: output_to_global(p.rect, out),
                output: out.id.clone(),
                matched: 0,
                style: BadgeStyle::Normal,
                vision: (elements[idx[k]].source == ElementSource::Vision)
                    .then_some(elements[idx[k]].rect),
            });
        }
    }
    stage(timings, "place", t, ());

    let chip_output = targets
        .first()
        .map(|t| t.output.clone())
        .or_else(|| focused_output.cloned());
    let scene = Scene {
        outputs: outputs.to_vec(),
        badges,
        pinned: None,
        scroll: None,
        chip: chip_output.map(|output| Chip {
            mode: "click".into(),
            sub: None,
            swatch: false,
            typed: String::new(),
            scope,
            output,
        }),
        theme: theme.clone(),
    };
    (scene, label_strs)
}

/// Redraw `base` for the typed buffer: matching badges keep their matched
/// prefix, `eliminated` ones fade (when `fade`) or disappear, and `flash`
/// marks the selected label.
pub fn filter_scene(
    base: &Scene,
    typed: &str,
    eliminated: &[&str],
    fade: bool,
    flash: Option<&str>,
) -> Scene {
    let mut scene = base.clone();
    let n = typed.chars().count();
    scene.badges = base
        .badges
        .iter()
        .filter_map(|b| {
            let mut b = b.clone();
            if b.style == BadgeStyle::Drag {
                return Some(b);
            }
            if flash == Some(b.label.as_str()) {
                b.style = BadgeStyle::Flash;
                b.matched = b.label.chars().count();
            } else if flash.is_some() {
                return None;
            } else if b.label.starts_with(typed) {
                b.matched = n;
            } else if fade && eliminated.contains(&b.label.as_str()) {
                b.style = BadgeStyle::Faded;
            } else {
                return None;
            }
            Some(b)
        })
        .collect();
    if let Some(c) = scene.chip.as_mut() {
        c.typed = typed.to_owned();
    }
    scene
}

/// Run select → collect → label → place on already queried Hyprland state.
/// With `action`, only elements [`hintable`] for it are kept.
/// `render`/`commit` are the caller's (they need the overlay or a renderer).
pub async fn run<P: ElementProvider + Sync, V: VisionStage>(
    hypr: HyprState,
    scope: Scope,
    action: Option<Action>,
    theme: &Theme,
    mut timings: Timings,
    provider: &P,
    vision: &V,
) -> Result<Output, SelectError> {
    // select
    let t = Instant::now();
    if scope != Scope::Focused {
        return Err(SelectError::Unsupported(scope));
    }
    let targets: Vec<Target> = hypr
        .active
        .iter()
        .map(|w| focused_target(w, &hypr.outputs, hypr.focused_output.as_ref()))
        .collect();
    stage(&mut timings, "select", t, ());

    // collect: the provider's own `collect.*` stages, or one `collect` stage.
    let t = Instant::now();
    let mut per_target: Vec<Vec<Element>> = Vec::with_capacity(targets.len());
    let mut reports = Vec::new();
    let mut sub = Timings::default();
    for target in &targets {
        let budget = Budget {
            deadline: Instant::now() + PROVIDER_BUDGET,
        };
        match provider
            .collect(target, budget)
            .instrument(tracing::debug_span!("collect", provider = provider.name()))
            .await
        {
            Ok(c) => {
                if c.report.truncated {
                    tracing::warn!(provider = provider.name(), "budget exceeded; truncated");
                }
                per_target.push(c.elements);
                reports.push(c.report);
                sub.0.extend(c.timings.0);
            }
            Err(e) => {
                tracing::warn!(provider = provider.name(), "{e}");
                per_target.push(Vec::new());
                reports.push(CollectReport {
                    provider: provider.name().into(),
                    reasons: vec![e.to_string()],
                    ..Default::default()
                });
            }
        }
    }
    if sub.0.is_empty() {
        stage(&mut timings, "collect", t, ());
    } else {
        timings.0.extend(sub.0);
    }

    // vision: policy per target, then capture → detect → merge (design D18).
    let mut report = VisionReport::default();
    let mut vision_timings = Timings::default();
    let mut merge_ms = Duration::ZERO;
    let mut elements = Vec::new();
    for (target, atspi) in targets.iter().zip(per_target) {
        let usable = match action {
            Some(a) => atspi.iter().filter(|e| hintable(e, a)).count(),
            None => atspi.len(),
        };
        let (run, reason) = vision.decide(usable, target, !atspi.is_empty());
        tracing::debug!(class = %target.class, usable, run, reason, "vision decision");
        if report.reason.is_empty() || run && !report.ran {
            report = VisionReport {
                ran: run,
                reason: reason.into(),
            };
        }
        if !run {
            elements.extend(atspi);
            continue;
        }
        let found = match vision
            .collect(target, &hypr.outputs)
            .instrument(tracing::debug_span!("vision"))
            .await
        {
            Ok(c) => {
                vision_timings.0.extend(c.timings.0);
                reports.push(c.report);
                c.elements
            }
            Err(e) => {
                tracing::warn!("vision: {e}");
                reports.push(CollectReport {
                    provider: "vision".into(),
                    reasons: vec![e.to_string()],
                    ..Default::default()
                });
                Vec::new()
            }
        };
        let t = Instant::now();
        elements.extend(merge(atspi, found, &vision.merge_params()));
        merge_ms += t.elapsed();
    }
    if report.ran {
        for name in ["capture", "detect"] {
            let ms: f64 = vision_timings
                .0
                .iter()
                .filter(|s| s.stage == name)
                .map(|s| s.ms)
                .sum();
            if vision_timings.get(name).is_some() {
                timings.record(name, Duration::from_secs_f64(ms / 1000.0));
            }
        }
        timings.record("merge", merge_ms);
    }
    if let Some(a) = action {
        elements.retain(|e| hintable(e, a));
    }
    for (n, e) in elements.iter_mut().enumerate() {
        e.id = ElementId(n as u32 + 1);
    }

    let (scene, labels) = build_scene(
        &elements,
        &targets,
        &hypr.outputs,
        hypr.focused_output.as_ref(),
        scope,
        theme,
        &mut timings,
    );
    Ok(Output {
        targets,
        elements,
        labels,
        scene,
        timings,
        reports,
        vision: report,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use woints_core::geom::{Point, Size};
    use woints_core::ports::Badge;

    fn outputs() -> Vec<OutputGeom> {
        vec![
            OutputGeom {
                id: OutputId("A".into()),
                origin: Point::new(-1920, 0),
                logical_size: Size { w: 1920, h: 1080 },
                scale: 1.0,
            },
            OutputGeom {
                id: OutputId("B".into()),
                origin: Point::new(0, 0),
                logical_size: Size { w: 1440, h: 900 },
                scale: 2.0,
            },
        ]
    }

    fn window(x: i32, y: i32) -> Client {
        Client {
            address: "0x1".into(),
            at: [x, y],
            size: [800, 600],
            class: "foot".into(),
            pid: 9,
            ..Default::default()
        }
    }

    async fn run_on(win: Client) -> Output {
        let h = HyprState {
            outputs: outputs(),
            focused_output: Some(OutputId("B".into())),
            active: Some(win),
        };
        run(
            h,
            Scope::Focused,
            None,
            &Theme::default(),
            Timings::default(),
            &DemoProvider,
            &NoVision,
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn demo_boxes_inside_window_on_its_output() {
        let o = run_on(window(-1500, 100)).await;
        assert_eq!(o.targets.len(), 1);
        assert_eq!(o.targets[0].output, OutputId("A".into()));
        assert_eq!(o.elements.len(), 5);
        let win = o.targets[0].rect;
        for e in &o.elements {
            assert_eq!(
                e.rect.intersection_area(&win),
                e.rect.area(),
                "{:?} outside window",
                e.rect
            );
        }
        assert_eq!(o.scene.badges.len(), 5);
        assert!(
            o.scene
                .badges
                .iter()
                .all(|b: &Badge| b.output == OutputId("A".into()))
        );
        let mut labels: Vec<_> = o.scene.badges.iter().map(|b| b.label.clone()).collect();
        labels.sort();
        assert_eq!(labels, ["a", "d", "f", "g", "s"]);
        // The centre box is the most central: it gets the first label.
        let centre = o.elements.iter().position(|e| e.name == "centre").unwrap();
        let centre_badge = o
            .scene
            .badges
            .iter()
            .find(|b| b.rect.intersects(&o.elements[centre].rect))
            .unwrap();
        assert_eq!(centre_badge.label, "a");
        let stages: Vec<_> = o.timings.0.iter().map(|s| s.stage.as_str()).collect();
        assert_eq!(stages, ["select", "collect", "label", "place"]);
    }

    /// Vision stage running when fewer than `min` elements, finding one box
    /// overlapping the first demo box and one new box.
    struct FakeVision {
        min: usize,
    }

    impl VisionStage for FakeVision {
        fn decide(&self, atspi: usize, _: &Target, _: bool) -> (bool, &'static str) {
            if atspi < self.min {
                (true, "too_few_atspi")
            } else {
                (false, "enough_atspi")
            }
        }
        async fn collect(&self, t: &Target, _: &[OutputGeom]) -> Result<Collected, ProviderError> {
            let o = t.rect.origin();
            let el = |x: i32, y: i32| Element {
                id: ElementId(0),
                rect: Rect::new(o.x + x, o.y + y, 64, 28),
                role: "box".into(),
                name: String::new(),
                source: ElementSource::Vision,
                actions: Action::POINTER.to_vec(),
                caps: vec![],
                confidence: 0.5,
                handle: None,
            };
            let mut timings = Timings::default();
            timings.record("capture", Duration::from_millis(1));
            timings.record("detect", Duration::from_millis(2));
            Ok(Collected {
                elements: vec![el(DEMO_INSET + 1, DEMO_INSET), el(300, 200)],
                report: CollectReport {
                    provider: "vision".into(),
                    ..Default::default()
                },
                timings,
            })
        }
        fn merge_params(&self) -> MergeParams {
            MergeParams::default()
        }
    }

    async fn run_vision(min: usize) -> Output {
        let h = HyprState {
            outputs: outputs(),
            focused_output: Some(OutputId("B".into())),
            active: Some(window(-1500, 100)),
        };
        run(
            h,
            Scope::Focused,
            Some(Action::Click),
            &Theme::default(),
            Timings::default(),
            &DemoProvider,
            &FakeVision { min },
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn vision_runs_and_merges() {
        let o = run_vision(10).await;
        assert_eq!(
            o.vision,
            VisionReport {
                ran: true,
                reason: "too_few_atspi".into()
            }
        );
        // Five demo boxes plus one new vision box; the overlapping one is dropped.
        assert_eq!(o.elements.len(), 6);
        assert_eq!(o.elements[5].source, ElementSource::Vision);
        let stages: Vec<_> = o.timings.0.iter().map(|s| s.stage.as_str()).collect();
        assert_eq!(
            stages,
            [
                "select", "collect", "capture", "detect", "merge", "label", "place"
            ]
        );
        let vb = o.scene.badges.iter().filter(|b| b.vision.is_some()).count();
        assert_eq!(vb, 1);
    }

    #[tokio::test]
    async fn vision_skipped_omits_stages() {
        let o = run_vision(3).await;
        assert!(!o.vision.ran);
        assert_eq!(o.vision.reason, "enough_atspi");
        assert_eq!(o.elements.len(), 5);
        assert!(o.timings.get("capture").is_none() && o.timings.get("merge").is_none());
    }

    #[tokio::test]
    async fn filter_scene_styles() {
        let o = run_on(window(-1500, 100)).await;
        let f = filter_scene(&o.scene, "d", &["a", "s"], false, None);
        assert_eq!(f.badges.len(), 1);
        assert_eq!((f.badges[0].label.as_str(), f.badges[0].matched), ("d", 1));
        assert_eq!(f.chip.unwrap().typed, "d");
        let faded = filter_scene(&o.scene, "d", &["a", "s"], true, None);
        assert_eq!(faded.badges.len(), 3);
        assert_eq!(
            faded
                .badges
                .iter()
                .filter(|b| b.style == BadgeStyle::Faded)
                .count(),
            2
        );
        let flash = filter_scene(&o.scene, "", &[], false, Some("g"));
        assert_eq!(flash.badges.len(), 1);
        assert_eq!(flash.badges[0].style, BadgeStyle::Flash);
    }

    #[tokio::test]
    async fn no_focused_window() {
        let h = HyprState {
            outputs: outputs(),
            focused_output: None,
            active: None,
        };
        let o = run(
            h,
            Scope::Focused,
            None,
            &Theme::default(),
            Timings::default(),
            &DemoProvider,
            &NoVision,
        )
        .await
        .unwrap();
        assert!(o.targets.is_empty() && o.elements.is_empty() && o.scene.badges.is_empty());
    }

    #[tokio::test]
    async fn other_scopes_unsupported() {
        let h = HyprState {
            outputs: outputs(),
            focused_output: None,
            active: None,
        };
        assert_eq!(
            run(
                h,
                Scope::All,
                None,
                &Theme::default(),
                Timings::default(),
                &DemoProvider,
                &NoVision
            )
            .await
            .unwrap_err(),
            SelectError::Unsupported(Scope::All)
        );
    }

    #[tokio::test]
    async fn drag_phase_two_excludes_source() {
        let o = run_on(window(-1500, 100)).await;
        let theme = Theme::default();
        let source = o.labels.iter().position(|l| l == "a").unwrap();
        let (scene, labels) = second_scene(
            &o.scene,
            &o.elements,
            source,
            "a",
            &o.targets,
            None,
            Scope::Focused,
            &theme,
        );
        assert_eq!(labels.len(), o.elements.len());
        assert_eq!(labels[source], "");
        assert!(labels.iter().all(|l| !l.contains('a')));
        let drag: Vec<_> = scene
            .badges
            .iter()
            .filter(|b| b.style == BadgeStyle::Drag)
            .collect();
        assert_eq!(drag.len(), 1);
        assert_eq!(drag[0].label, "a");
        assert_eq!(scene.pinned.as_ref().unwrap().rect, o.elements[source].rect);
        let chip = scene.chip.as_ref().unwrap();
        assert_eq!(
            (chip.mode.as_str(), chip.sub.as_deref(), chip.swatch),
            ("drag 2/2", Some("pick destination"), true)
        );
        // Typing keeps the pinned source drawn.
        let f = filter_scene(&scene, "s", &[], false, None);
        assert!(f.badges.iter().any(|b| b.style == BadgeStyle::Drag));
    }

    #[test]
    fn hintable_by_action() {
        use woints_core::model::AtspiCap;
        let el = |role: &str, caps: Vec<AtspiCap>| Element {
            id: ElementId(1),
            rect: Rect::new(0, 0, 1, 1),
            role: role.into(),
            name: String::new(),
            source: ElementSource::Atspi,
            actions: Action::supported(&caps),
            caps,
            confidence: 1.0,
            handle: None,
        };
        let pane = el("scroll pane", vec![AtspiCap::Accessible]);
        let button = el("push button", vec![AtspiCap::DoAction]);
        let vision = el("vision", vec![]);
        assert!(hintable(&pane, Action::Scroll) && !hintable(&pane, Action::Click));
        assert!(!hintable(&button, Action::Scroll) && hintable(&button, Action::Click));
        assert!(hintable(&vision, Action::Scroll) && !hintable(&vision, Action::Focus));
    }
}
