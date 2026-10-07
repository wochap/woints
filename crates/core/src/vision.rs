//! Vision domain types shared by capture, detection and merge (design D13–D16).

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::geom::{Global, Rect};

/// Pixels of one captured [`Frame`], origin at its top-left corner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct FramePx;

/// An RGBA8 image of one target. Frame pixel `(px, py)` is global logical
/// `(rect.x + px / scale, rect.y + py / scale)`.
#[derive(Clone, PartialEq)]
pub struct Frame {
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub rect: Rect<Global>,
    pub scale: f32,
}

impl fmt::Debug for Frame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Frame")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("rect", &self.rect)
            .field("scale", &self.scale)
            .finish_non_exhaustive()
    }
}

impl Frame {
    /// Frame pixel rect to global logical, rounding edges outward-neutral.
    pub fn to_global(&self, r: Rect<FramePx>) -> Rect<Global> {
        let s = f64::from(self.scale.max(f32::EPSILON));
        let x0 = (f64::from(r.x) / s).round() as i32;
        let y0 = (f64::from(r.y) / s).round() as i32;
        let x1 = (f64::from(r.right()) / s).round() as i32;
        let y1 = (f64::from(r.bottom()) / s).round() as i32;
        Rect::new(self.rect.x + x0, self.rect.y + y0, x1 - x0, y1 - y0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DetectionClass {
    Box,
    TextLine,
    Link,
    Image,
    Unknown,
}

impl DetectionClass {
    /// Element `role` for this class.
    pub fn role(&self) -> &'static str {
        match self {
            DetectionClass::Box => "box",
            DetectionClass::TextLine => "text line",
            DetectionClass::Link => "link",
            DetectionClass::Image => "image",
            DetectionClass::Unknown => "unknown",
        }
    }
}

/// One candidate region found by a detector.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Detection {
    pub rect_px: Rect<FramePx>,
    pub confidence: f32,
    pub class: DetectionClass,
}

/// Whether vision ran for a request and why (`snapshot`, logs, `doctor`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VisionReport {
    pub ran: bool,
    pub reason: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pixel_to_global_at_scale_2() {
        let f = Frame {
            rgba: vec![],
            width: 800,
            height: 600,
            rect: Rect::new(100, 50, 400, 300),
            scale: 2.0,
        };
        assert_eq!(
            f.to_global(Rect::new(200, 100, 0, 0)).origin(),
            crate::geom::Point::new(200, 100)
        );
        assert_eq!(f.to_global(Rect::new(0, 0, 800, 600)), f.rect);
    }

    #[test]
    fn report_json() {
        let r = VisionReport {
            ran: true,
            reason: "too_few_atspi".into(),
        };
        assert_eq!(
            serde_json::to_value(&r).unwrap(),
            serde_json::json!({"ran": true, "reason": "too_few_atspi"})
        );
        assert_eq!(
            serde_json::to_value(DetectionClass::TextLine).unwrap(),
            "text_line"
        );
    }
}
