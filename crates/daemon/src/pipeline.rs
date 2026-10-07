//! One pass: `hypr.query → select → collect → label → place → render → commit`.

use std::time::{Duration, Instant};

use tracing::Instrument;

use woints_core::geom::{
    OutputGeom, OutputId, Rect, WindowGeom, WindowLocal, global_to_output, output_to_global,
    window_to_global,
};
use woints_core::labels::{PriorityItem, assign, labels, priority};
use woints_core::model::{ActionKind, Element, ElementId, ElementSource, Scope, Target, Timings};
use woints_core::place::{PlaceInput, badge_size, place};
use woints_core::ports::{
    Badge, BadgeStyle, Budget, Chip, CollectReport, Collected, ElementProvider, ProviderError,
    Scene, SelectError,
};
use woints_core::theme::Theme;
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
                actions: vec![ActionKind::Pointer],
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
        chip: chip_output.map(|output| Chip {
            mode: "click".into(),
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
/// `render`/`commit` are the caller's (they need the overlay or a renderer).
pub async fn run<P: ElementProvider + Sync>(
    hypr: HyprState,
    scope: Scope,
    theme: &Theme,
    mut timings: Timings,
    provider: &P,
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
    let mut elements = Vec::new();
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
                elements.extend(c.elements);
                reports.push(c.report);
                sub.0.extend(c.timings.0);
            }
            Err(e) => {
                tracing::warn!(provider = provider.name(), "{e}");
                reports.push(CollectReport {
                    provider: provider.name().into(),
                    reasons: vec![e.to_string()],
                    ..Default::default()
                });
            }
        }
    }
    for (n, e) in elements.iter_mut().enumerate() {
        e.id = ElementId(n as u32 + 1);
    }
    if sub.0.is_empty() {
        stage(&mut timings, "collect", t, ());
    } else {
        timings.0.extend(sub.0);
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
            &Theme::default(),
            Timings::default(),
            &DemoProvider,
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
            &Theme::default(),
            Timings::default(),
            &DemoProvider,
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
                &Theme::default(),
                Timings::default(),
                &DemoProvider
            )
            .await
            .unwrap_err(),
            SelectError::Unsupported(Scope::All)
        );
    }
}
