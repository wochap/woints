//! One pass: `hypr.query → select → collect → label → place → render → commit`.

use std::future::poll_fn;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use futures_core::Stream;
use tracing::Instrument;

use woints_core::geom::{
    OutputGeom, OutputId, Rect, WindowGeom, WindowLocal, global_to_output, output_to_global,
    window_to_global,
};
use woints_core::labels::{PriorityItem, assign, labels, priority};
use woints_core::model::{ActionKind, Element, ElementId, ElementSource, Scope, Target, Timings};
use woints_core::place::{PlaceInput, badge_size, place};
use woints_core::ports::{Badge, Budget, Chip, ElementProvider, ElementStream, Scene, SelectError};
use woints_core::theme::Theme;
use woints_hypr::{Client, focused_target};

/// Everything a run produces.
#[derive(Debug, Clone)]
pub struct Output {
    pub targets: Vec<Target>,
    pub elements: Vec<Element>,
    pub scene: Scene,
    pub timings: Timings,
}

/// Inputs gathered by the `hypr.query` stage.
pub struct HyprState {
    pub outputs: Vec<OutputGeom>,
    pub focused_output: Option<OutputId>,
    pub active: Option<Client>,
}

struct IterStream<I>(I);

impl<I: Iterator + Unpin> Stream for IterStream<I> {
    type Item = I::Item;
    fn poll_next(mut self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Option<I::Item>> {
        Poll::Ready(self.0.next())
    }
}

async fn next<S: Stream + ?Sized>(s: &mut Pin<Box<S>>) -> Option<S::Item> {
    poll_fn(|cx| s.as_mut().poll_next(cx)).await
}

/// M1 demo provider: five boxes at the focused window's corners and centre.
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

    fn collect<'a>(&'a self, target: &'a Target, _budget: Budget) -> ElementStream<'a> {
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
                actions: vec![ActionKind::Click],
                confidence: 1.0,
            })
            .collect();
        Box::pin(IterStream(items.into_iter()))
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
) -> Scene {
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
            });
        }
    }
    stage(timings, "place", t, ());

    let chip_output = targets
        .first()
        .map(|t| t.output.clone())
        .or_else(|| focused_output.cloned());
    Scene {
        outputs: outputs.to_vec(),
        badges,
        chip: chip_output.map(|output| Chip {
            mode: "click".into(),
            typed: String::new(),
            scope,
            output,
        }),
        theme: theme.clone(),
    }
}

/// Run select → collect → label → place on already queried Hyprland state.
/// `render`/`commit` are the caller's (they need the overlay or a renderer).
pub async fn run(
    hypr: HyprState,
    scope: Scope,
    theme: &Theme,
    mut timings: Timings,
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

    // collect
    let t = Instant::now();
    let provider = DemoProvider;
    let mut elements = Vec::new();
    for target in &targets {
        let budget = Budget {
            deadline: Instant::now() + Duration::from_millis(80),
        };
        let mut s = provider.collect(target, budget);
        while let Some(e) = next(&mut s)
            .instrument(tracing::debug_span!("collect", provider = provider.name()))
            .await
        {
            if Instant::now() > budget.deadline {
                tracing::warn!(provider = provider.name(), "budget exceeded; truncating");
                break;
            }
            elements.push(e);
        }
    }
    stage(&mut timings, "collect", t, ());

    let scene = build_scene(
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
        scene,
        timings,
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
        run(h, Scope::Focused, &Theme::default(), Timings::default())
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
    async fn no_focused_window() {
        let h = HyprState {
            outputs: outputs(),
            focused_output: None,
            active: None,
        };
        let o = run(h, Scope::Focused, &Theme::default(), Timings::default())
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
            run(h, Scope::All, &Theme::default(), Timings::default())
                .await
                .unwrap_err(),
            SelectError::Unsupported(Scope::All)
        );
    }
}
