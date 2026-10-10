//! One pass: `hypr.query → select → collect → label → place → render → commit`.

use std::future::Future;
use std::time::{Duration, Instant};

use tracing::Instrument;

use woints_core::config::Config;
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
use woints_hypr::{HyprSnapshot, LayerFilter, select_targets};
use woints_qs::{LayerCollect, QuickshellIpcProvider};

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
    /// Per element: it belongs to a layer target.
    pub layer: Vec<bool>,
}

/// Collects elements for layer targets, all at once (one `qs` call per
/// instance, not per layer).
pub trait LayerSource: Sync {
    fn collect_layers(
        &self,
        targets: &[&Target],
        budget: Budget,
    ) -> impl Future<Output = LayerCollect> + Send;
}

/// Quickshell provider disabled: layers get vision only.
pub struct NoLayers;

impl LayerSource for NoLayers {
    async fn collect_layers(&self, targets: &[&Target], _: Budget) -> LayerCollect {
        LayerCollect {
            per_target: targets
                .iter()
                .map(|_| Collected {
                    report: CollectReport {
                        provider: "quickshell".into(),
                        reasons: vec!["disabled".into()],
                        ..Default::default()
                    },
                    ..Default::default()
                })
                .collect(),
            timings: Timings::default(),
        }
    }
}

impl LayerSource for QuickshellIpcProvider {
    async fn collect_layers(&self, targets: &[&Target], budget: Budget) -> LayerCollect {
        QuickshellIpcProvider::collect_layers(self, targets, budget).await
    }
}

/// `all` scope stacking assumption (design D14): floating windows sit above
/// tiled ones. Drops elements of tiled targets fully covered by a floating
/// target on the same output; returns how many were dropped.
pub fn cull_under_floating(
    per_target: &mut [Vec<Element>],
    targets: &[Target],
    floating: &[bool],
) -> usize {
    let mut culled = 0;
    for i in 0..targets.len() {
        if targets[i].surface.is_layer() || floating[i] {
            continue;
        }
        let covers: Vec<Rect<woints_core::geom::Global>> = (0..targets.len())
            .filter(|&j| {
                floating[j]
                    && !targets[j].surface.is_layer()
                    && targets[j].output == targets[i].output
            })
            .map(|j| targets[j].rect)
            .collect();
        if covers.is_empty() {
            continue;
        }
        let before = per_target[i].len();
        per_target[i].retain(|e| {
            !covers
                .iter()
                .any(|c| c.intersection_area(&e.rect) == e.rect.area())
        });
        culled += before - per_target[i].len();
    }
    culled
}

/// The vision fallback as the pipeline sees it (design D18): a policy
/// decision per target, then capture → detect.
pub trait VisionStage: Sync {
    /// Whether vision runs for `target` given `atspi` usable elements, and why.
    /// `wake_failed`: the target's Chromium tree stayed dormant after a wake.
    fn decide(
        &self,
        atspi: usize,
        target: &Target,
        served: bool,
        wake_failed: bool,
    ) -> (bool, &'static str);
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
    fn decide(&self, _: usize, _: &Target, _: bool, _: bool) -> (bool, &'static str) {
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
    pub fn new(
        link: woints_overlay::CaptureLink,
        config: &Config,
        onnx: Option<woints_vision::chain::SharedDetector>,
    ) -> Self {
        let vision = config.vision_config();
        let capture = woints_vision::CompositeCapture {
            toplevel: woints_overlay::ToplevelExportCapture(link.clone()),
            region: woints_overlay::ScreencopyRegionCapture(link),
            timeout: Duration::from_millis(vision.budget_ms),
        };
        Self {
            provider: woints_vision::VisionProvider::new(
                capture,
                Box::new(woints_vision::ChainDetector::new(
                    vision.detector,
                    onnx,
                    config.merge,
                )),
                vision,
            ),
            merge: config.merge,
        }
    }
}

#[cfg(feature = "vision")]
impl VisionStage for Vision {
    fn decide(
        &self,
        atspi: usize,
        target: &Target,
        served: bool,
        wake_failed: bool,
    ) -> (bool, &'static str) {
        let d = woints_vision::decide_vision(
            self.provider.config(),
            atspi,
            &target.class,
            target.surface.is_layer(),
            served,
            wake_failed,
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
    let container = woints_a11y::rule::is_scroll_container_name(&e.role);
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
    layer: &[bool],
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
    let rest_layer: Vec<bool> = layer
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != source)
        .map(|(_, l)| *l)
        .collect();
    let mut timings = Timings::default();
    let (mut scene, rest_labels) = build_scene(
        &rest,
        &rest_layer,
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
    layer: &[bool],
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
        .enumerate()
        .map(|(i, e)| PriorityItem {
            rect: e.rect,
            is_layer: layer.get(i).copied().unwrap_or(false),
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
/// Toplevels go to `provider`, layers to `layers`, concurrently. With
/// `action`, only elements [`hintable`] for it are kept. `render`/`commit`
/// are the caller's (they need the overlay or a renderer).
#[allow(clippy::too_many_arguments)]
pub async fn run<P: ElementProvider + Sync, L: LayerSource, V: VisionStage>(
    snap: HyprSnapshot,
    scope: Scope,
    action: Option<Action>,
    config: &Config,
    theme: &Theme,
    mut timings: Timings,
    provider: &P,
    layers: &L,
    vision: &V,
) -> Result<Output, SelectError> {
    // select
    let t = Instant::now();
    let filter = LayerFilter {
        levels: config.scope.levels.iter().map(|l| l.index()).collect(),
        ignore: config.scope.ignore_layers.clone(),
    };
    let mut targets = select_targets(&snap, scope, &filter);
    targets.retain(|t| t.surface.is_layer() || !config.class_disabled(&t.class));
    let outputs = snap.outputs();
    let focused_output = snap.focused_output();
    stage(&mut timings, "select", t, ());

    // collect: the providers' own stages, or one `collect` stage.
    let t = Instant::now();
    let top_idx: Vec<usize> = (0..targets.len())
        .filter(|&i| !targets[i].surface.is_layer())
        .collect();
    let layer_refs: Vec<&Target> = targets.iter().filter(|t| t.surface.is_layer()).collect();
    let tops = async {
        let mut out = Vec::with_capacity(top_idx.len());
        let mut sub = Timings::default();
        for &i in &top_idx {
            let target = &targets[i];
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
                    sub.0.extend(c.timings.0);
                    out.push((c.elements, c.report));
                }
                Err(e) => {
                    tracing::warn!(provider = provider.name(), "{e}");
                    out.push((
                        Vec::new(),
                        CollectReport {
                            provider: provider.name().into(),
                            reasons: vec![e.to_string()],
                            ..Default::default()
                        },
                    ));
                }
            }
        }
        (out, sub)
    };
    let lays = async {
        if layer_refs.is_empty() {
            LayerCollect::default()
        } else {
            // Layers go through forked `qs ipc` calls (50–100 ms each on
            // quickshell 0.3.1), so give them the configured quickshell budget
            // instead of the in-process AT-SPI one.
            let qs_budget = Duration::from_millis(config.providers.quickshell.budget_ms);
            let budget = Budget {
                deadline: Instant::now() + PROVIDER_BUDGET.max(qs_budget),
            };
            layers.collect_layers(&layer_refs, budget).await
        }
    };
    let ((top_out, sub), lc) = tokio::join!(tops, lays);
    let mut per_target: Vec<Vec<Element>> = vec![Vec::new(); targets.len()];
    // A layer is "served" when its quickshell instance answered `list`, even
    // with zero items (an empty notification popup, a bar with nothing
    // mapped); only layers with no provider at all fall back to vision.
    let mut served: Vec<bool> = vec![false; targets.len()];
    let mut wake_failed: Vec<bool> = vec![false; targets.len()];
    let mut reports = Vec::new();
    for (&i, (els, rep)) in top_idx.iter().zip(top_out) {
        served[i] = !els.is_empty();
        wake_failed[i] = rep.wake_failed();
        per_target[i] = els;
        reports.push(rep);
    }
    let layer_idx: Vec<usize> = (0..targets.len())
        .filter(|&i| targets[i].surface.is_layer())
        .collect();
    for (&i, c) in layer_idx.iter().zip(lc.per_target) {
        served[i] = !c.elements.is_empty() || c.report.paired.is_some();
        per_target[i] = c.elements;
        reports.push(c.report);
    }
    if sub.0.is_empty() && lc.timings.0.is_empty() {
        stage(&mut timings, "collect", t, ());
    } else {
        timings.0.extend(sub.0);
        timings.0.extend(lc.timings.0);
    }

    // vision: policy per target, then capture → detect → merge (design D18).
    let mut report = VisionReport::default();
    let mut vision_timings = Timings::default();
    let mut merge_ms = Duration::ZERO;
    let mut finals: Vec<Vec<Element>> = Vec::with_capacity(targets.len());
    for (((target, atspi), served), wake_failed) in
        targets.iter().zip(per_target).zip(served).zip(wake_failed)
    {
        let usable = match action {
            Some(a) => atspi.iter().filter(|e| hintable(e, a)).count(),
            None => atspi.len(),
        };
        let (run, reason) = vision.decide(usable, target, served, wake_failed);
        tracing::debug!(class = %target.class, usable, run, reason, "vision decision");
        if report.reason.is_empty() || run && !report.ran {
            report = VisionReport {
                ran: run,
                reason: reason.into(),
            };
        }
        if !run {
            finals.push(atspi);
            continue;
        }
        let found = match vision
            .collect(target, &outputs)
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
        finals.push(merge(atspi, found, &vision.merge_params()));
        merge_ms += t.elapsed();
    }
    if report.ran {
        for name in [
            "capture",
            "detect",
            "detect.onnx",
            "detect.onnx.pre",
            "detect.onnx.run",
            "detect.onnx.post",
        ] {
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
    if scope == Scope::All {
        let floating: Vec<bool> = targets
            .iter()
            .map(|t| match &t.surface {
                woints_core::model::Surface::Toplevel { address } => snap
                    .clients
                    .iter()
                    .chain(snap.active.iter())
                    .find(|c| c.address == *address)
                    .is_some_and(|c| c.floating),
                _ => false,
            })
            .collect();
        let culled = cull_under_floating(&mut finals, &targets, &floating);
        tracing::debug!(culled, "elements under floating windows dropped");
    }
    let mut elements = Vec::new();
    let mut layer = Vec::new();
    for (target, els) in targets.iter().zip(finals) {
        for e in els {
            if action.is_none_or(|a| hintable(&e, a)) {
                layer.push(target.surface.is_layer());
                elements.push(e);
            }
        }
    }
    for (n, e) in elements.iter_mut().enumerate() {
        e.id = ElementId(n as u32 + 1);
    }

    let (scene, labels) = build_scene(
        &elements,
        &layer,
        &targets,
        &outputs,
        focused_output.as_ref(),
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
        layer,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use woints_core::ports::Badge;
    use woints_hypr::{Client, Layer, LayerEntry, Monitor};

    fn monitors() -> Vec<Monitor> {
        vec![
            Monitor {
                id: 0,
                name: "A".into(),
                x: -1920,
                width: 1920,
                height: 1080,
                scale: 1.0,
                ..Default::default()
            },
            Monitor {
                id: 1,
                name: "B".into(),
                width: 2880,
                height: 1800,
                scale: 2.0,
                focused: true,
                ..Default::default()
            },
        ]
    }

    fn snap(active: Option<Client>) -> HyprSnapshot {
        HyprSnapshot {
            monitors: monitors(),
            active,
            ..Default::default()
        }
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
        let h = snap(Some(win));
        run(
            h,
            Scope::Focused,
            None,
            &Config::default(),
            &Theme::default(),
            Timings::default(),
            &DemoProvider,
            &NoLayers,
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
        fn decide(
            &self,
            atspi: usize,
            _: &Target,
            _: bool,
            wake_failed: bool,
        ) -> (bool, &'static str) {
            if wake_failed {
                (false, "chromium_dormant")
            } else if atspi < self.min {
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
        let h = snap(Some(window(-1500, 100)));
        run(
            h,
            Scope::Focused,
            Some(Action::Click),
            &Config::default(),
            &Theme::default(),
            Timings::default(),
            &DemoProvider,
            &NoLayers,
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

    /// No elements, report carrying `reason`.
    struct ReasonProvider(&'static str);

    impl ElementProvider for ReasonProvider {
        fn name(&self) -> &'static str {
            "atspi"
        }
        async fn collect(&self, _: &Target, _: Budget) -> Result<Collected, ProviderError> {
            Ok(Collected {
                elements: Vec::new(),
                report: CollectReport {
                    provider: "atspi".into(),
                    reasons: vec![self.0.into()],
                    ..Default::default()
                },
                timings: Timings::default(),
            })
        }
    }

    async fn run_reason(reason: &'static str) -> Output {
        run(
            snap(Some(window(-1500, 100))),
            Scope::Focused,
            Some(Action::Click),
            &Config::default(),
            &Theme::default(),
            Timings::default(),
            &ReasonProvider(reason),
            &NoLayers,
            &FakeVision { min: 10 },
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn wake_timeout_skips_vision() {
        let o = run_reason(woints_core::ports::CHROMIUM_WAKE_TIMEOUT).await;
        assert_eq!(
            o.vision,
            VisionReport {
                ran: false,
                reason: "chromium_dormant".into()
            }
        );
        assert!(o.elements.is_empty());
        let o = run_reason(woints_core::ports::CHROMIUM_DORMANT).await;
        assert!(o.vision.ran);
        assert_eq!(o.vision.reason, "too_few_atspi");
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
        let h = snap(None);
        let o = run(
            h,
            Scope::Focused,
            None,
            &Config::default(),
            &Theme::default(),
            Timings::default(),
            &DemoProvider,
            &NoLayers,
            &NoVision,
        )
        .await
        .unwrap();
        assert!(o.targets.is_empty() && o.elements.is_empty() && o.scene.badges.is_empty());
    }

    fn bar() -> LayerEntry {
        LayerEntry {
            monitor: "B".into(),
            level: 1,
            layer: Layer {
                address: "0xb".into(),
                x: 0,
                y: 0,
                w: 1440,
                h: 40,
                namespace: "quickshell:bar".into(),
                pid: 77,
            },
        }
    }

    /// Ten 200×30 bar items, all larger than the window's demo boxes.
    struct FakeLayers;

    impl LayerSource for FakeLayers {
        async fn collect_layers(&self, targets: &[&Target], _: Budget) -> LayerCollect {
            let mut timings = Timings::default();
            timings.record("qs.list", Duration::from_millis(1));
            LayerCollect {
                per_target: targets
                    .iter()
                    .map(|t| Collected {
                        elements: (0..10)
                            .map(|n| Element {
                                id: ElementId(0),
                                rect: Rect::new(t.rect.x + n * 140, t.rect.y, 130, 30),
                                role: "quickshell".into(),
                                name: format!("item {n}"),
                                source: ElementSource::Quickshell,
                                actions: vec![Action::Click],
                                caps: vec![],
                                confidence: 1.0,
                                handle: None,
                            })
                            .collect(),
                        ..Default::default()
                    })
                    .collect(),
                timings,
            }
        }
    }

    #[tokio::test]
    async fn window_plus_bar_one_label_space() {
        let mut s = snap(Some(window(100, 100)));
        s.layers = vec![bar()];
        let theme = Theme::default();
        let o = run(
            s,
            Scope::FocusedAndLayers,
            None,
            &Config::default(),
            &theme,
            Timings::default(),
            &DemoProvider,
            &FakeLayers,
            &NoVision,
        )
        .await
        .unwrap();
        assert_eq!(o.targets.len(), 2);
        assert!(o.targets[1].surface.is_layer());
        assert_eq!(o.elements.len(), 15);
        assert!(o.timings.get("qs.list").is_some());
        // Window elements get the single glyphs; every pair goes to the bar,
        // although each bar item is larger than every window box.
        for (i, l) in o.labels.iter().enumerate() {
            if !o.layer[i] {
                assert_eq!(l.chars().count(), 1, "window {l}");
            } else if l.chars().count() == 1 {
                assert!(
                    o.labels
                        .iter()
                        .zip(&o.layer)
                        .filter(|(_, b)| !**b)
                        .all(|(w, _)| w.chars().count() == 1)
                );
            }
        }
        assert_eq!(
            o.labels
                .iter()
                .zip(&o.layer)
                .filter(|(l, b)| l.chars().count() == 2 && !**b)
                .count(),
            0
        );
    }

    #[tokio::test]
    async fn layers_scope_has_no_toplevel() {
        let mut s = snap(Some(window(100, 100)));
        s.layers = vec![bar()];
        let o = run(
            s,
            Scope::Layers,
            None,
            &Config::default(),
            &Theme::default(),
            Timings::default(),
            &DemoProvider,
            &FakeLayers,
            &NoVision,
        )
        .await
        .unwrap();
        assert!(o.targets.iter().all(|t| t.surface.is_layer()));
        assert_eq!(o.elements.len(), 10);
    }

    #[tokio::test]
    async fn disabled_class_dropped() {
        let mut c = Config::default();
        c.apps.disabled_classes = vec!["fo?t".into()];
        let o = run(
            snap(Some(window(100, 100))),
            Scope::Focused,
            None,
            &c,
            &Theme::default(),
            Timings::default(),
            &DemoProvider,
            &NoLayers,
            &NoVision,
        )
        .await
        .unwrap();
        assert!(o.targets.is_empty());
    }

    #[tokio::test]
    async fn quickshell_off_spawns_nothing() {
        let before = woints_qs::spawn_count();
        let mut s = snap(Some(window(100, 100)));
        s.layers = vec![bar()];
        for scope in [Scope::Focused, Scope::FocusedAndLayers, Scope::Layers] {
            run(
                s.clone(),
                scope,
                Some(Action::Click),
                &Config::default(),
                &Theme::default(),
                Timings::default(),
                &DemoProvider,
                &NoLayers,
                &NoVision,
            )
            .await
            .unwrap();
        }
        assert_eq!(woints_qs::spawn_count(), before);
    }

    #[test]
    fn floating_over_tiled() {
        let t = |addr: &str, r: Rect<woints_core::geom::Global>| Target {
            surface: woints_core::model::Surface::Toplevel {
                address: addr.into(),
            },
            rect: r,
            output: OutputId("B".into()),
            pid: None,
            class: addr.into(),
        };
        let el = |x, y, w, h| Element {
            id: ElementId(0),
            rect: Rect::new(x, y, w, h),
            role: "push button".into(),
            name: String::new(),
            source: ElementSource::Demo,
            actions: vec![Action::Click],
            caps: vec![],
            confidence: 1.0,
            handle: None,
        };
        let targets = [
            t("tiled", Rect::new(0, 0, 1000, 800)),
            t("float", Rect::new(50, 50, 300, 300)),
        ];
        let mut per = vec![
            vec![el(100, 100, 40, 20), el(600, 600, 40, 20)],
            vec![el(60, 60, 10, 10)],
        ];
        assert_eq!(cull_under_floating(&mut per, &targets, &[false, true]), 1);
        assert_eq!(per[0].len(), 1);
        assert_eq!(per[0][0].rect.x, 600);
        assert_eq!(per[1].len(), 1);
    }

    #[tokio::test]
    async fn drag_phase_two_excludes_source() {
        let o = run_on(window(-1500, 100)).await;
        let theme = Theme::default();
        let source = o.labels.iter().position(|l| l == "a").unwrap();
        let (scene, labels) = second_scene(
            &o.scene,
            &o.elements,
            &o.layer,
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
