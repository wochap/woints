//! Typed coordinate spaces and the transforms between them (design D2).
//!
//! Every [`Point`] and [`Rect`] carries a phantom marker naming the space its
//! numbers live in. Arithmetic is only defined within one space; moving
//! between spaces requires an explicit transform with its context.
//!
//! ```compile_fail
//! use woints_core::geom::{Point, Global, WindowLocal};
//! let a: Point<WindowLocal> = Point::new(1, 2);
//! let b: Point<Global> = Point::new(3, 4);
//! let _ = a + b; // mixed spaces: does not compile
//! ```
//!
//! ```compile_fail
//! use woints_core::geom::{Point, Global, WindowLocal};
//! let a: Point<WindowLocal> = Point::new(1, 2);
//! let b: Point<Global> = Point::new(1, 2);
//! assert!(a == b); // mixed spaces: does not compile
//! ```

use std::fmt;
use std::marker::PhantomData;
use std::ops::{Add, Sub};

use serde::{Deserialize, Serialize};

pub mod space {
    use serde::{Deserialize, Serialize};

    /// Logical pixels relative to a toplevel's top-left corner.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
    pub struct WindowLocal;
    /// Logical pixels relative to an output's top-left corner.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
    pub struct OutputLocal;
    /// Logical pixels in Hyprland's global layout space (may be negative).
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
    pub struct Global;
    /// Physical pixels of one output's buffer.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
    pub struct Device;
}

pub use space::{Device, Global, OutputLocal, WindowLocal};

#[derive(Serialize, Deserialize)]
#[serde(bound = "")]
pub struct Point<S> {
    pub x: i32,
    pub y: i32,
    #[serde(skip)]
    _s: PhantomData<S>,
}

impl<S> Point<S> {
    pub const fn new(x: i32, y: i32) -> Self {
        Self {
            x,
            y,
            _s: PhantomData,
        }
    }
}

impl<S> Clone for Point<S> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<S> Copy for Point<S> {}
impl<S> PartialEq for Point<S> {
    fn eq(&self, o: &Self) -> bool {
        self.x == o.x && self.y == o.y
    }
}
impl<S> Eq for Point<S> {}
impl<S> fmt::Debug for Point<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "({}, {})", self.x, self.y)
    }
}
impl<S> Add for Point<S> {
    type Output = Self;
    fn add(self, o: Self) -> Self {
        Self::new(self.x + o.x, self.y + o.y)
    }
}
impl<S> Sub for Point<S> {
    type Output = Self;
    fn sub(self, o: Self) -> Self {
        Self::new(self.x - o.x, self.y - o.y)
    }
}

/// Axis-aligned rectangle; `x, y` is the top-left corner, `w, h` are non-negative.
#[derive(Serialize, Deserialize)]
#[serde(bound = "")]
pub struct Rect<S> {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    #[serde(skip)]
    _s: PhantomData<S>,
}

impl<S> Clone for Rect<S> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<S> Copy for Rect<S> {}
impl<S> PartialEq for Rect<S> {
    fn eq(&self, o: &Self) -> bool {
        (self.x, self.y, self.w, self.h) == (o.x, o.y, o.w, o.h)
    }
}
impl<S> Eq for Rect<S> {}
impl<S> fmt::Debug for Rect<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "({}, {}, {}, {})", self.x, self.y, self.w, self.h)
    }
}

impl<S> Rect<S> {
    pub const fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        Self {
            x,
            y,
            w,
            h,
            _s: PhantomData,
        }
    }

    pub fn origin(&self) -> Point<S> {
        Point::new(self.x, self.y)
    }
    pub fn right(&self) -> i32 {
        self.x + self.w
    }
    pub fn bottom(&self) -> i32 {
        self.y + self.h
    }
    pub fn area(&self) -> i64 {
        i64::from(self.w.max(0)) * i64::from(self.h.max(0))
    }
    /// Centre, rounded toward negative infinity.
    pub fn center(&self) -> Point<S> {
        Point::new(self.x + self.w.div_euclid(2), self.y + self.h.div_euclid(2))
    }
    /// Half-open containment: `[x, x + w) × [y, y + h)`.
    pub fn contains(&self, p: Point<S>) -> bool {
        p.x >= self.x && p.x < self.right() && p.y >= self.y && p.y < self.bottom()
    }
    /// True when the rects share a positive area.
    pub fn intersects(&self, o: &Self) -> bool {
        self.intersection_area(o) > 0
    }
    pub fn intersection_area(&self, o: &Self) -> i64 {
        let w = self.right().min(o.right()) - self.x.max(o.x);
        let h = self.bottom().min(o.bottom()) - self.y.max(o.y);
        if w <= 0 || h <= 0 {
            0
        } else {
            i64::from(w) * i64::from(h)
        }
    }
    /// Intersection over union, `0.0` for disjoint or empty rects.
    pub fn iou(&self, o: &Self) -> f32 {
        let i = self.intersection_area(o);
        let u = self.area() + o.area() - i;
        if u <= 0 { 0.0 } else { i as f32 / u as f32 }
    }
    pub fn translate(&self, dx: i32, dy: i32) -> Self {
        Self::new(self.x + dx, self.y + dy, self.w, self.h)
    }
    pub fn inflate(&self, d: i32) -> Self {
        Self::new(self.x - d, self.y - d, self.w + 2 * d, self.h + 2 * d)
    }
}

/// Logical width and height.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Size {
    pub w: i32,
    pub h: i32,
}

/// Stable output identifier: the connector name (`eDP-1`, `DP-1`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct OutputId(pub String);

impl fmt::Display for OutputId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Where a toplevel sits in global logical space.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowGeom {
    pub origin: Point<Global>,
}

/// An output in global logical space plus its fractional scale.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OutputGeom {
    pub id: OutputId,
    pub origin: Point<Global>,
    pub logical_size: Size,
    pub scale: f32,
}

impl OutputGeom {
    pub fn global_rect(&self) -> Rect<Global> {
        Rect::new(
            self.origin.x,
            self.origin.y,
            self.logical_size.w,
            self.logical_size.h,
        )
    }
    pub fn local_rect(&self) -> Rect<OutputLocal> {
        Rect::new(0, 0, self.logical_size.w, self.logical_size.h)
    }
    /// Buffer size in device pixels: logical size × scale, rounded.
    pub fn device_size(&self) -> (u32, u32) {
        (
            scale_round(self.logical_size.w, self.scale).max(1) as u32,
            scale_round(self.logical_size.h, self.scale).max(1) as u32,
        )
    }
}

fn scale_round(v: i32, scale: f32) -> i32 {
    (f64::from(v) * f64::from(scale)).round() as i32
}

pub fn window_to_global(r: Rect<WindowLocal>, win: &WindowGeom) -> Rect<Global> {
    Rect::new(r.x + win.origin.x, r.y + win.origin.y, r.w, r.h)
}

pub fn global_to_output(r: Rect<Global>, out: &OutputGeom) -> Rect<OutputLocal> {
    Rect::new(r.x - out.origin.x, r.y - out.origin.y, r.w, r.h)
}

pub fn output_to_global(r: Rect<OutputLocal>, out: &OutputGeom) -> Rect<Global> {
    Rect::new(r.x + out.origin.x, r.y + out.origin.y, r.w, r.h)
}

/// Logical output-local rect to device pixels.
///
/// Origin and far edge are rounded independently and the size is their
/// difference, so rects that touch in logical space still touch on device.
pub fn output_to_device(r: Rect<OutputLocal>, scale: f32) -> Rect<Device> {
    let x0 = scale_round(r.x, scale);
    let y0 = scale_round(r.y, scale);
    let x1 = scale_round(r.right(), scale);
    let y1 = scale_round(r.bottom(), scale);
    Rect::new(x0, y0, x1 - x0, y1 - y0)
}

pub fn global_to_device(r: Rect<Global>, out: &OutputGeom) -> Rect<Device> {
    output_to_device(global_to_output(r, out), out.scale)
}

/// Pick the output that holds `rect`: the one containing its centre, else the
/// one with the largest overlap, else `focused`, else the first output.
pub fn pick_output(
    rect: Rect<Global>,
    outputs: &[OutputGeom],
    focused: Option<&OutputId>,
) -> Option<OutputId> {
    let c = rect.center();
    if let Some(o) = outputs.iter().find(|o| o.global_rect().contains(c)) {
        return Some(o.id.clone());
    }
    let best = outputs
        .iter()
        .map(|o| (o.global_rect().intersection_area(&rect), o))
        .filter(|(a, _)| *a > 0)
        .max_by_key(|(a, _)| *a);
    if let Some((_, o)) = best {
        return Some(o.id.clone());
    }
    focused
        .cloned()
        .or_else(|| outputs.first().map(|o| o.id.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn out(name: &str, x: i32, y: i32, w: i32, h: i32, scale: f32) -> OutputGeom {
        OutputGeom {
            id: OutputId(name.into()),
            origin: Point::new(x, y),
            logical_size: Size { w, h },
            scale,
        }
    }

    #[test]
    fn window_local_to_global() {
        let win = WindowGeom {
            origin: Point::new(1, 41),
        };
        let g = window_to_global(Rect::new(10, 20, 100, 30), &win);
        assert_eq!(g, Rect::<Global>::new(11, 61, 100, 30));
    }

    #[test]
    fn scale_2_at_origin() {
        let o = out("eDP-1", 0, 0, 1440, 900, 2.0);
        assert_eq!(
            global_to_device(Rect::new(100, 50, 20, 10), &o),
            Rect::new(200, 100, 40, 20)
        );
    }

    #[test]
    fn fractional_1_5() {
        let o = out("A", 0, 0, 1920, 1080, 1.5);
        let d = global_to_device(Rect::new(101, 51, 21, 11), &o);
        assert_eq!((d.x, d.y), (152, 77));
        assert_eq!((d.right(), d.bottom()), (183, 93));
        assert_eq!((d.w, d.h), (31, 16));
    }

    #[test]
    fn adjacent_rects_stay_adjacent() {
        for scale in [1.25_f32, 1.5, 1.75, 2.0] {
            for x in 0..50 {
                let a = output_to_device(Rect::new(x, 0, 7, 7), scale);
                let b = output_to_device(Rect::new(x + 7, 0, 7, 7), scale);
                assert_eq!(a.right(), b.x, "scale {scale} x {x}");
            }
        }
    }

    #[test]
    fn negative_offset() {
        let o = out("DP-2", -1920, 0, 1920, 1080, 1.0);
        assert_eq!(
            global_to_device(Rect::new(-1800, 100, 40, 40), &o),
            Rect::new(120, 100, 40, 40)
        );
    }

    #[test]
    fn negative_offset_fractional() {
        let o = out("DP-2", -1920, 0, 1280, 720, 1.5);
        let d = global_to_device(Rect::new(-1900, 10, 20, 20), &o);
        assert_eq!(d, Rect::new(30, 15, 30, 30));
    }

    #[test]
    fn device_size_rounds() {
        assert_eq!(out("e", 0, 0, 1440, 900, 2.0).device_size(), (2880, 1800));
        assert_eq!(out("e", 0, 0, 1280, 853, 1.5).device_size(), (1920, 1280));
    }

    #[test]
    fn seam_straddle_goes_to_centre_output() {
        let outs = [
            out("A", 0, 0, 1920, 1080, 1.0),
            out("B", 1920, 0, 1440, 900, 2.0),
        ];
        // centre x = 1880 + 100/2 = 1930
        let r = Rect::new(1880, 100, 100, 50);
        assert_eq!(pick_output(r, &outs, None), Some(OutputId("B".into())));
    }

    #[test]
    fn centre_outside_falls_back_to_overlap_then_focused() {
        let outs = [
            out("A", 0, 0, 1920, 1080, 1.0),
            out("B", 1920, 0, 1440, 900, 1.0),
        ];
        // centre (2050, 1000) lies in neither output; overlaps B more.
        let r = Rect::new(1900, 850, 300, 300);
        assert_eq!(pick_output(r, &outs, None), Some(OutputId("B".into())));
        let far = Rect::new(10_000, 10_000, 10, 10);
        let f = OutputId("A".into());
        assert_eq!(pick_output(far, &outs, Some(&f)), Some(f));
    }

    #[test]
    fn rect_ops() {
        let a: Rect<Global> = Rect::new(0, 0, 10, 10);
        let b = Rect::new(5, 5, 10, 10);
        assert!(a.contains(Point::new(0, 0)));
        assert!(!a.contains(Point::new(10, 0)));
        assert_eq!(a.center(), Point::new(5, 5));
        assert!(a.intersects(&b));
        assert!(!a.intersects(&Rect::new(10, 0, 5, 5)));
        assert_eq!(a.intersection_area(&b), 25);
        assert!((a.iou(&b) - 25.0 / 175.0).abs() < 1e-6);
    }

    #[test]
    fn rect_json_shape() {
        let r: Rect<Global> = Rect::new(1, 2, 3, 4);
        assert_eq!(
            serde_json::to_string(&r).unwrap(),
            r#"{"x":1,"y":2,"w":3,"h":4}"#
        );
    }
}
