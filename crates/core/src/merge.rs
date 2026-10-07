//! Combine AT-SPI and vision elements without duplicates (design D17).

use serde::{Deserialize, Serialize};

use crate::geom::Rect;
use crate::model::{Action, Element, ElementId, ElementSource};
use crate::vision::{Detection, Frame};

/// Largest area ratio between two rects for containment to count as a duplicate.
pub const MAX_CONTAINMENT_RATIO: f64 = 3.0;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MergeParams {
    pub iou_threshold: f32,
    pub containment_threshold: f32,
}

impl Default for MergeParams {
    fn default() -> Self {
        Self {
            iou_threshold: 0.3,
            containment_threshold: 0.9,
        }
    }
}

/// Vision elements for `detections` in global logical coordinates; ids are
/// left at 0 for the pipeline to number.
pub fn to_elements(detections: &[Detection], frame: &Frame) -> Vec<Element> {
    detections
        .iter()
        .map(|d| Element {
            id: ElementId(0),
            rect: frame.to_global(d.rect_px),
            role: d.class.role().into(),
            name: String::new(),
            source: ElementSource::Vision,
            actions: Action::POINTER.to_vec(),
            caps: vec![],
            confidence: d.confidence.clamp(0.0, 1.0),
            handle: None,
        })
        .collect()
}

/// `outer` holds at least `threshold` of `inner`'s area and is at most
/// [`MAX_CONTAINMENT_RATIO`] times larger.
fn contains<S>(outer: &Rect<S>, inner: &Rect<S>, threshold: f32) -> bool {
    let ia = inner.area();
    if ia <= 0 {
        return false;
    }
    let covered = outer.intersection_area(inner) as f64 / ia as f64;
    covered >= f64::from(threshold) && outer.area() as f64 <= ia as f64 * MAX_CONTAINMENT_RATIO
}

/// `v` duplicates `a` by IoU or containment.
pub fn duplicate<S>(v: &Rect<S>, a: &Rect<S>, p: &MergeParams) -> bool {
    v.iou(a) > p.iou_threshold
        || contains(v, a, p.containment_threshold)
        || contains(a, v, p.containment_threshold)
}

/// AT-SPI elements first, then vision elements not duplicating any of them,
/// by confidence descending.
pub fn merge(atspi: Vec<Element>, vision: Vec<Element>, params: &MergeParams) -> Vec<Element> {
    let mut kept: Vec<Element> = vision
        .into_iter()
        .filter(|v| !atspi.iter().any(|a| duplicate(&v.rect, &a.rect, params)))
        .collect();
    kept.sort_by(|a, b| b.confidence.total_cmp(&a.confidence));
    let mut out = atspi;
    out.extend(kept);
    out
}

/// Detector chain merge (design D5 of M6): every `primary` detection, then
/// `secondary` ones not duplicating any of them.
pub fn merge_detections(
    primary: Vec<Detection>,
    secondary: Vec<Detection>,
    params: &MergeParams,
) -> Vec<Detection> {
    let extra: Vec<Detection> = secondary
        .into_iter()
        .filter(|s| {
            !primary
                .iter()
                .any(|p| duplicate(&s.rect_px, &p.rect_px, params))
        })
        .collect();
    let mut out = primary;
    out.extend(extra);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::Global;
    use crate::vision::DetectionClass;

    fn el(r: (i32, i32, i32, i32), source: ElementSource, conf: f32) -> Element {
        Element {
            id: ElementId(0),
            rect: Rect::new(r.0, r.1, r.2, r.3),
            role: "push button".into(),
            name: String::new(),
            source,
            actions: vec![Action::Click],
            caps: vec![],
            confidence: conf,
            handle: None,
        }
    }

    fn frame(scale: f32) -> Frame {
        Frame {
            rgba: vec![],
            width: 0,
            height: 0,
            rect: Rect::new(100, 50, 1000, 800),
            scale,
        }
    }

    fn det(r: (i32, i32, i32, i32)) -> Detection {
        Detection {
            rect_px: Rect::new(r.0, r.1, r.2, r.3),
            confidence: 0.5,
            class: DetectionClass::Link,
            source: Default::default(),
        }
    }

    #[test]
    fn mapping_scale_1_5() {
        let e = to_elements(&[det((150, 75, 300, 60))], &frame(1.5));
        assert_eq!(e[0].rect, Rect::new(200, 100, 200, 40));
        assert_eq!(e[0].source, ElementSource::Vision);
        assert_eq!(e[0].role, "link");
        assert_eq!(e[0].actions, Action::POINTER.to_vec());
        assert!(e[0].name.is_empty());
    }

    #[test]
    fn mapping_scale_2() {
        let e = to_elements(&[det((200, 100, 400, 80))], &frame(2.0));
        assert_eq!(e[0].rect, Rect::new(200, 100, 200, 40));
    }

    fn run(v: (i32, i32, i32, i32), a: (i32, i32, i32, i32)) -> Vec<Element> {
        merge(
            vec![el(a, ElementSource::Atspi, 1.0)],
            vec![el(v, ElementSource::Vision, 0.5)],
            &MergeParams::default(),
        )
    }

    #[test]
    fn overlapping_button() {
        let m = run((10, 10, 100, 30), (12, 11, 98, 30));
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].source, ElementSource::Atspi);
    }

    #[test]
    fn distinct_boxes_kept() {
        // 50×10 overlap of two 100×50 rects: IoU = 500 / 9500 ≈ 0.05.
        let v = Rect::<Global>::new(0, 0, 100, 50);
        let a = Rect::<Global>::new(50, 40, 100, 50);
        assert!(v.iou(&a) < 0.11);
        assert_eq!(run((0, 0, 100, 50), (50, 40, 100, 50)).len(), 2);
    }

    #[test]
    fn vision_box_around_labelled_button() {
        let m = run((0, 0, 120, 40), (4, 4, 112, 32));
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].source, ElementSource::Atspi);
    }

    #[test]
    fn vision_box_inside_large_panel() {
        assert_eq!(run((10, 10, 50, 20), (0, 0, 1000, 800)).len(), 2);
    }

    #[test]
    fn ordering_and_provenance() {
        let m = merge(
            vec![el((0, 0, 10, 10), ElementSource::Atspi, 1.0)],
            vec![
                el((100, 0, 10, 10), ElementSource::Vision, 0.4),
                el((200, 0, 10, 10), ElementSource::Vision, 0.9),
            ],
            &MergeParams::default(),
        );
        let got: Vec<_> = m.iter().map(|e| (e.source, e.confidence)).collect();
        assert_eq!(
            got,
            [
                (ElementSource::Atspi, 1.0),
                (ElementSource::Vision, 0.9),
                (ElementSource::Vision, 0.4)
            ]
        );
        let v = serde_json::to_value(&m[1]).unwrap();
        assert_eq!(v["source"], "vision");
        assert!(v["confidence"].is_number());
    }
}
