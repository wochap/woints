//! Classical detector (design D15): grayscale → downscale → edges → dilate →
//! connected components → filters → text lines → link hue → confidence.

use std::collections::VecDeque;
use std::time::Instant;

use woints_core::config::{EdgeMode, OnnxConfig, VisionConfig};
use woints_core::geom::Rect;
use woints_core::model::Timings;
use woints_core::vision::{Detection, DetectionClass, DetectionSource, Frame, FramePx};

/// Turns a frame into candidate boxes in frame pixels (design D16).
pub trait Detector: Send {
    fn detect(&mut self, frame: &Frame, params: &DetectParams) -> Vec<Detection>;
    /// Sub-stage timings of the last [`Detector::detect`] (`detect.onnx`, ...).
    fn take_timings(&mut self) -> Timings {
        Timings::default()
    }
}

/// Detector knobs plus the request deadline.
#[derive(Debug, Clone, PartialEq)]
pub struct DetectParams {
    pub downscale_width: u32,
    pub canny: (f32, f32),
    pub edge: EdgeMode,
    pub min_box: i32,
    pub max_box: i32,
    pub max_aspect: f32,
    pub text_height: i32,
    pub link_hue: bool,
    pub max_elements: usize,
    /// ONNX thresholds and metadata overrides, read per request.
    pub onnx: OnnxConfig,
    /// Stages after this instant are skipped; partial results are returned.
    pub deadline: Option<Instant>,
}

impl Default for DetectParams {
    fn default() -> Self {
        Self::from_config(&VisionConfig::default(), None)
    }
}

impl DetectParams {
    pub fn from_config(c: &VisionConfig, deadline: Option<Instant>) -> Self {
        Self {
            downscale_width: c.downscale_width,
            canny: c.canny,
            edge: c.edge,
            min_box: c.min_box,
            max_box: c.max_box,
            max_aspect: c.max_aspect,
            text_height: c.text_height,
            link_hue: c.link_hue,
            max_elements: c.max_elements,
            onnx: c.onnx.clone(),
            deadline,
        }
    }

    fn expired(&self) -> bool {
        self.deadline.is_some_and(|d| Instant::now() >= d)
    }
}

/// Nested detection whose area is at least this share of its parent's is a duplicate.
pub const NEST_AREA: f64 = 0.85;
/// Parents with more nested detections than this are containers.
pub const NEST_MAX_INNER: usize = 6;
/// Text-line merge: vertical centre tolerance and horizontal gap, logical px.
pub const TEXT_CENTRE_TOL: f32 = 3.0;
pub const TEXT_GAP: f32 = 4.0;
/// Link heuristic: share of ink pixels that must be saturated blue.
pub const LINK_SHARE: f32 = 0.4;
pub const LINK_HUE: (f32, f32) = (200.0, 270.0);
pub const LINK_SAT: f32 = 0.5;

pub const CONF_BOX: f32 = 0.5;
pub const CONF_TEXT: f32 = 0.4;
pub const CONF_ROUNDED: f32 = 0.2;
pub const CONF_LINK: f32 = 0.2;

/// Reusable per-size buffers (design D19).
#[derive(Default)]
struct Scratch {
    size: (usize, usize),
    gray: Vec<u8>,
    mag: Vec<f32>,
    dir: Vec<u8>,
    edges: Vec<u8>,
    tmp: Vec<u8>,
    mask: Vec<u8>,
    labels: Vec<u32>,
    queue: VecDeque<usize>,
}

impl Scratch {
    fn ensure(&mut self, w: usize, h: usize) {
        if self.size == (w, h) {
            return;
        }
        let n = w * h;
        self.size = (w, h);
        self.gray = vec![0; n];
        self.mag = vec![0.0; n];
        self.dir = vec![0; n];
        self.edges = vec![0; n];
        self.tmp = vec![0; n];
        self.mask = vec![0; n];
        self.labels = vec![0; n];
    }
}

/// A component in downscaled pixels, half-open bounds.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Comp {
    x0: i32,
    y0: i32,
    x1: i32,
    y1: i32,
    class: DetectionClass,
    rounded: bool,
}

impl Comp {
    fn w(&self) -> i32 {
        self.x1 - self.x0
    }
    fn h(&self) -> i32 {
        self.y1 - self.y0
    }
    fn area(&self) -> i64 {
        i64::from(self.w()) * i64::from(self.h())
    }
    fn contains(&self, o: &Comp) -> bool {
        o.x0 >= self.x0 && o.y0 >= self.y0 && o.x1 <= self.x1 && o.y1 <= self.y1
    }
}

#[derive(Default)]
pub struct ClassicalDetector {
    scratch: Scratch,
}

impl ClassicalDetector {
    pub fn new() -> Self {
        Self::default()
    }
}

/// Integer downscale factor so the width fits `max_w`.
pub fn downscale_factor(width: u32, max_w: u32) -> usize {
    (width as usize).div_ceil(max_w.max(1) as usize).max(1)
}

/// Dilation kernel side: `max(2, round(3 × scale / factor))`.
pub fn dilate_kernel(scale: f32, factor: usize) -> usize {
    ((3.0 * scale / factor as f32).round() as usize).max(2)
}

/// Grayscale and box-downscale `frame` by `f` into `out` (`w × h`).
fn gray_downscale(frame: &Frame, f: usize, w: usize, h: usize, out: &mut [u8]) {
    let fw = frame.width as usize;
    let n = (f * f) as u32;
    for y in 0..h {
        for x in 0..w {
            let mut sum = 0u32;
            for dy in 0..f {
                let row = (y * f + dy) * fw;
                for dx in 0..f {
                    let i = (row + x * f + dx) * 4;
                    let p = &frame.rgba[i..i + 3];
                    sum +=
                        (77 * u32::from(p[0]) + 150 * u32::from(p[1]) + 29 * u32::from(p[2])) >> 8;
                }
            }
            out[y * w + x] = (sum / n) as u8;
        }
    }
}

/// Canny on `gray`: Sobel (L1), non-maximum suppression, hysteresis.
fn canny(s: &mut Scratch, w: usize, h: usize, low: f32, high: f32) {
    let g = &s.gray;
    s.mag.iter_mut().for_each(|m| *m = 0.0);
    for y in 1..h.saturating_sub(1) {
        for x in 1..w.saturating_sub(1) {
            let p = |dx: isize, dy: isize| {
                i32::from(g[(y as isize + dy) as usize * w + (x as isize + dx) as usize])
            };
            let gx = p(1, -1) + 2 * p(1, 0) + p(1, 1) - p(-1, -1) - 2 * p(-1, 0) - p(-1, 1);
            let gy = p(-1, 1) + 2 * p(0, 1) + p(1, 1) - p(-1, -1) - 2 * p(0, -1) - p(1, -1);
            let i = y * w + x;
            s.mag[i] = (gx.abs() + gy.abs()) as f32;
            // 0: horizontal gradient, 1: 45°, 2: vertical, 3: 135°.
            let (ax, ay) = (gx.abs() as f32, gy.abs() as f32);
            s.dir[i] = if ay <= ax * 0.4142 {
                0
            } else if ay >= ax * 2.4142 {
                2
            } else if (gx > 0) == (gy > 0) {
                1
            } else {
                3
            };
        }
    }
    // Non-maximum suppression into `tmp`: 2 strong, 1 weak, 0 none.
    s.tmp.iter_mut().for_each(|v| *v = 0);
    for y in 1..h.saturating_sub(1) {
        for x in 1..w.saturating_sub(1) {
            let i = y * w + x;
            let m = s.mag[i];
            if m < low {
                continue;
            }
            let (a, b) = match s.dir[i] {
                0 => (i - 1, i + 1),
                2 => (i - w, i + w),
                1 => (i - w - 1, i + w + 1),
                _ => (i - w + 1, i + w - 1),
            };
            if m >= s.mag[a] && m > s.mag[b] {
                s.tmp[i] = if m >= high { 2 } else { 1 };
            }
        }
    }
    // Hysteresis: weak pixels connected to strong ones survive.
    s.edges.iter_mut().for_each(|v| *v = 0);
    s.queue.clear();
    for i in 0..w * h {
        if s.tmp[i] == 2 {
            s.edges[i] = 1;
            s.queue.push_back(i);
        }
    }
    while let Some(i) = s.queue.pop_front() {
        let (x, y) = ((i % w) as isize, (i / w) as isize);
        for dy in -1..=1 {
            for dx in -1..=1 {
                let (nx, ny) = (x + dx, y + dy);
                if nx < 0 || ny < 0 || nx >= w as isize || ny >= h as isize {
                    continue;
                }
                let j = ny as usize * w + nx as usize;
                if s.tmp[j] == 1 && s.edges[j] == 0 {
                    s.edges[j] = 1;
                    s.queue.push_back(j);
                }
            }
        }
    }
}

/// Absolute 4-neighbour Laplacian above `threshold`.
fn laplacian(s: &mut Scratch, w: usize, h: usize, threshold: f32) {
    s.edges.iter_mut().for_each(|v| *v = 0);
    for y in 1..h.saturating_sub(1) {
        for x in 1..w.saturating_sub(1) {
            let i = y * w + x;
            let g = |j: usize| i32::from(s.gray[j]);
            let l = 4 * g(i) - g(i - 1) - g(i + 1) - g(i - w) - g(i + w);
            if l.abs() as f32 >= threshold {
                s.edges[i] = 1;
            }
        }
    }
}

/// Square dilation of `edges` by `k` (separable max) into `mask`; the kernel
/// reaches `k / 2` before and `(k - 1) / 2` after each pixel.
fn dilate(s: &mut Scratch, w: usize, h: usize, k: usize) {
    let (before, after) = (k / 2, (k - 1) / 2);
    for y in 0..h {
        for x in 0..w {
            let lo = x.saturating_sub(after);
            let hi = (x + before).min(w - 1);
            s.tmp[y * w + x] = u8::from((lo..=hi).any(|xx| s.edges[y * w + xx] != 0));
        }
    }
    for y in 0..h {
        let lo = y.saturating_sub(after);
        let hi = (y + before).min(h - 1);
        for x in 0..w {
            s.mask[y * w + x] = u8::from((lo..=hi).any(|yy| s.tmp[yy * w + x] != 0));
        }
    }
}

/// 8-connected components of `mask`, as bounding rects.
fn components(s: &mut Scratch, w: usize, h: usize) -> Vec<Comp> {
    s.labels.iter_mut().for_each(|l| *l = 0);
    let mut out = Vec::new();
    for start in 0..w * h {
        if s.mask[start] == 0 || s.labels[start] != 0 {
            continue;
        }
        let label = out.len() as u32 + 1;
        let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0, 0);
        s.labels[start] = label;
        s.queue.clear();
        s.queue.push_back(start);
        while let Some(i) = s.queue.pop_front() {
            let (x, y) = (i % w, i / w);
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x + 1);
            y1 = y1.max(y + 1);
            for dy in -1isize..=1 {
                for dx in -1isize..=1 {
                    let (nx, ny) = (x as isize + dx, y as isize + dy);
                    if nx < 0 || ny < 0 || nx >= w as isize || ny >= h as isize {
                        continue;
                    }
                    let j = ny as usize * w + nx as usize;
                    if s.mask[j] != 0 && s.labels[j] == 0 {
                        s.labels[j] = label;
                        s.queue.push_back(j);
                    }
                }
            }
        }
        // Rounded-rect-like: dense outline along the box, empty extreme corners.
        let at = |x: usize, y: usize| s.labels[y * w + x] == label;
        let corners = [(x0, y0), (x1 - 1, y0), (x0, y1 - 1), (x1 - 1, y1 - 1)];
        let empty_corners = corners.iter().filter(|&&(x, y)| !at(x, y)).count();
        let mid_x = (x0 + x1) / 2;
        let mid_y = (y0 + y1) / 2;
        let sides = [(mid_x, y0), (mid_x, y1 - 1), (x0, mid_y), (x1 - 1, mid_y)];
        let solid_sides = sides.iter().filter(|&&(x, y)| at(x, y)).count();
        out.push(Comp {
            x0: x0 as i32,
            y0: y0 as i32,
            x1: x1 as i32,
            y1: y1 as i32,
            class: DetectionClass::Box,
            rounded: empty_corners == 4 && solid_sides == 4 && x1 - x0 > 4 && y1 - y0 > 4,
        });
    }
    out
}

/// Merge text-height components on one line until stable (`unit`: logical px
/// per downscaled px).
fn merge_text(comps: &mut Vec<Comp>, text_h: f32, unit: f32) {
    let is_text = |c: &Comp| c.h() as f32 * unit <= text_h;
    loop {
        let mut merged = false;
        'outer: for i in 0..comps.len() {
            if !is_text(&comps[i]) {
                continue;
            }
            for j in i + 1..comps.len() {
                let (a, b) = (comps[i], comps[j]);
                if !is_text(&b) {
                    continue;
                }
                let ca = (a.y0 + a.y1) as f32 / 2.0;
                let cb = (b.y0 + b.y1) as f32 / 2.0;
                let gap = (a.x0.max(b.x0) - a.x1.min(b.x1)) as f32;
                if (ca - cb).abs() * unit <= TEXT_CENTRE_TOL && gap * unit <= TEXT_GAP {
                    let m = Comp {
                        x0: a.x0.min(b.x0),
                        y0: a.y0.min(b.y0),
                        x1: a.x1.max(b.x1),
                        y1: a.y1.max(b.y1),
                        class: DetectionClass::TextLine,
                        rounded: false,
                    };
                    if is_text(&m) {
                        comps[i] = m;
                        comps.swap_remove(j);
                        merged = true;
                        break 'outer;
                    }
                }
            }
        }
        if !merged {
            break;
        }
    }
    for c in comps.iter_mut() {
        if is_text(c) {
            c.class = DetectionClass::TextLine;
            c.rounded = false;
        }
    }
}

/// Drop duplicates nested in a parent and parents that are containers.
fn nest_filter(comps: Vec<Comp>) -> Vec<Comp> {
    let n = comps.len();
    let mut keep = vec![true; n];
    let mut inner = vec![0usize; n];
    for i in 0..n {
        for j in 0..n {
            if i == j || !comps[i].contains(&comps[j]) || comps[i] == comps[j] && j < i {
                continue;
            }
            inner[i] += 1;
            if comps[j].area() as f64 >= NEST_AREA * comps[i].area() as f64 {
                keep[j] = false;
            }
        }
    }
    for i in 0..n {
        if inner[i] > NEST_MAX_INNER {
            keep[i] = false;
        }
    }
    comps
        .into_iter()
        .zip(keep)
        .filter_map(|(c, k)| k.then_some(c))
        .collect()
}

/// RGB to (hue degrees, saturation) in HSV.
fn hue_sat(r: u8, g: u8, b: u8) -> (f32, f32) {
    let (r, g, b) = (
        f32::from(r) / 255.0,
        f32::from(g) / 255.0,
        f32::from(b) / 255.0,
    );
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let d = max - min;
    if max <= 0.0 || d <= 0.0 {
        return (0.0, 0.0);
    }
    let h = if max == r {
        60.0 * ((g - b) / d).rem_euclid(6.0)
    } else if max == g {
        60.0 * ((b - r) / d + 2.0)
    } else {
        60.0 * ((r - g) / d + 4.0)
    };
    (h, d / max)
}

/// Share of ink pixels (differing from the colour just outside `r`'s
/// top-left corner) in `r` that are saturated blue.
fn link_share(frame: &Frame, r: Rect<FramePx>) -> f32 {
    let w = frame.width as usize;
    let px = |x: i32, y: i32| {
        let i = (y as usize * w + x as usize) * 4;
        &frame.rgba[i..i + 3]
    };
    let bg = px((r.x - 2).max(0), (r.y - 2).max(0)).to_vec();
    let (mut ink, mut blue) = (0u32, 0u32);
    for y in r.y..r.bottom() {
        for x in r.x..r.right() {
            let p = px(x, y);
            let diff: i32 = p
                .iter()
                .zip(&bg)
                .map(|(a, b)| (i32::from(*a) - i32::from(*b)).abs())
                .sum();
            if diff < 60 {
                continue;
            }
            ink += 1;
            let (h, s) = hue_sat(p[0], p[1], p[2]);
            if s > LINK_SAT && (LINK_HUE.0..=LINK_HUE.1).contains(&h) {
                blue += 1;
            }
        }
    }
    if ink == 0 {
        0.0
    } else {
        blue as f32 / ink as f32
    }
}

impl Detector for ClassicalDetector {
    fn detect(&mut self, frame: &Frame, p: &DetectParams) -> Vec<Detection> {
        let (fw, fh) = (frame.width as usize, frame.height as usize);
        if fw < 3 || fh < 3 || frame.rgba.len() < fw * fh * 4 {
            return Vec::new();
        }
        let f = downscale_factor(frame.width, p.downscale_width);
        let (w, h) = (fw / f, fh / f);
        if w < 3 || h < 3 {
            return Vec::new();
        }
        let scale = frame.scale.max(f32::EPSILON);
        let unit = f as f32 / scale;
        let s = &mut self.scratch;
        s.ensure(w, h);

        gray_downscale(frame, f, w, h, &mut s.gray);
        if p.expired() {
            return Vec::new();
        }
        match p.edge {
            EdgeMode::Canny => canny(s, w, h, p.canny.0, p.canny.1),
            EdgeMode::Laplacian => laplacian(s, w, h, p.canny.0),
        }
        if p.expired() {
            return Vec::new();
        }
        let k = dilate_kernel(scale, f);
        dilate(s, w, h, k);
        let mut comps = components(s, w, h);
        // Undo the dilation reach so rects hug the edges.
        let (before, after) = ((k / 2) as i32, ((k - 1) / 2) as i32);
        for c in comps.iter_mut() {
            if c.w() > before + after && c.h() > before + after {
                c.x0 += after;
                c.y0 += after;
                c.x1 -= before;
                c.y1 -= before;
            }
        }
        let partial = p.expired();
        if partial {
            tracing::debug!("vision_budget_exceeded after components");
        } else {
            merge_text(&mut comps, p.text_height as f32, unit);
        }

        comps.retain(|c| {
            let (lw, lh) = (c.w() as f32 * unit, c.h() as f32 * unit);
            let aspect = lw.max(lh) / lw.min(lh).max(f32::EPSILON);
            lw >= p.min_box as f32
                && lh >= p.min_box as f32
                && lw <= p.max_box as f32
                && lh <= p.max_box as f32
                && aspect <= p.max_aspect
        });
        let comps = nest_filter(comps);

        let mut out: Vec<Detection> = comps
            .into_iter()
            .map(|c| {
                let x0 = (c.x0 as usize * f) as i32;
                let y0 = (c.y0 as usize * f) as i32;
                let x1 = ((c.x1 as usize * f).min(fw)) as i32;
                let y1 = ((c.y1 as usize * f).min(fh)) as i32;
                let rect_px = Rect::new(x0, y0, x1 - x0, y1 - y0);
                let mut class = c.class;
                let mut conf = match class {
                    DetectionClass::TextLine => CONF_TEXT,
                    _ => CONF_BOX,
                };
                if c.rounded {
                    conf += CONF_ROUNDED;
                }
                if p.link_hue
                    && !partial
                    && class == DetectionClass::TextLine
                    && link_share(frame, rect_px) >= LINK_SHARE
                {
                    class = DetectionClass::Link;
                    conf += CONF_LINK;
                }
                Detection {
                    rect_px,
                    confidence: conf.min(1.0),
                    class,
                    source: DetectionSource::Classical,
                }
            })
            .collect();
        if out.len() > p.max_elements {
            out.sort_by(|a, b| b.confidence.total_cmp(&a.confidence));
            out.truncate(p.max_elements);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use woints_core::geom::Global;

    struct Canvas {
        w: u32,
        h: u32,
        rgba: Vec<u8>,
    }

    impl Canvas {
        fn new(w: u32, h: u32, bg: u32) -> Self {
            let mut c = Self {
                w,
                h,
                rgba: vec![0; (w * h * 4) as usize],
            };
            c.fill(0, 0, w as i32, h as i32, bg);
            c
        }
        fn put(&mut self, x: i32, y: i32, rgb: u32) {
            if x < 0 || y < 0 || x >= self.w as i32 || y >= self.h as i32 {
                return;
            }
            let i = ((y as u32 * self.w + x as u32) * 4) as usize;
            self.rgba[i..i + 4].copy_from_slice(&[
                (rgb >> 16) as u8,
                (rgb >> 8) as u8,
                rgb as u8,
                255,
            ]);
        }
        fn fill(&mut self, x: i32, y: i32, w: i32, h: i32, rgb: u32) {
            for yy in y..y + h {
                for xx in x..x + w {
                    self.put(xx, yy, rgb);
                }
            }
        }
        /// Filled rect with corners cut by radius `r`.
        fn rounded(&mut self, x: i32, y: i32, w: i32, h: i32, r: i32, rgb: u32) {
            for yy in 0..h {
                for xx in 0..w {
                    let cx = if xx < r {
                        r - xx
                    } else if xx >= w - r {
                        xx - (w - r - 1)
                    } else {
                        0
                    };
                    let cy = if yy < r {
                        r - yy
                    } else if yy >= h - r {
                        yy - (h - r - 1)
                    } else {
                        0
                    };
                    if cx * cx + cy * cy <= r * r {
                        self.put(x + xx, y + yy, rgb);
                    }
                }
            }
        }
        fn frame(self, scale: f32) -> Frame {
            Frame {
                rect: Rect::<Global>::new(
                    0,
                    0,
                    (self.w as f32 / scale) as i32,
                    (self.h as f32 / scale) as i32,
                ),
                width: self.w,
                height: self.h,
                rgba: self.rgba,
                scale,
            }
        }
    }

    const BG: u32 = 0xeff1f5;
    const BTN: u32 = 0x4c4f69;

    fn near(r: Rect<FramePx>, x: i32, y: i32, w: i32, h: i32) -> bool {
        (r.x - x).abs() <= 2
            && (r.y - y).abs() <= 2
            && (r.right() - (x + w)).abs() <= 2
            && (r.bottom() - (y + h)).abs() <= 2
    }

    fn detect(frame: &Frame) -> Vec<Detection> {
        ClassicalDetector::new().detect(frame, &DetectParams::default())
    }

    #[test]
    fn five_buttons() {
        let mut c = Canvas::new(800, 600, BG);
        let at = [(40, 40), (300, 40), (560, 40), (40, 300), (300, 300)];
        for (x, y) in at {
            c.rounded(x, y, 120, 36, 6, BTN);
        }
        let d = detect(&c.frame(1.0));
        assert_eq!(d.len(), 5, "{d:?}");
        for (x, y) in at {
            let hit = d.iter().find(|d| near(d.rect_px, x, y, 120, 36));
            let hit = hit.unwrap_or_else(|| panic!("button at {x},{y}: {d:?}"));
            assert_eq!(hit.class, DetectionClass::Box);
            assert!((hit.confidence - 0.7).abs() < 1e-6, "rounded: {hit:?}");
        }
    }

    #[test]
    fn plain_box_confidence() {
        let mut c = Canvas::new(200, 100, BG);
        c.fill(40, 30, 100, 40, BTN);
        let d = detect(&c.frame(1.0));
        assert_eq!(d.len(), 1, "{d:?}");
        assert!((d[0].confidence - CONF_BOX).abs() < 1e-6);
    }

    #[test]
    fn dense_toolbar() {
        let mut c = Canvas::new(480, 64, BG);
        let xs: Vec<i32> = (0..12).map(|i| 20 + i * 32).collect();
        for &x in &xs {
            c.fill(x, 20, 24, 24, BTN);
        }
        let d = detect(&c.frame(1.0));
        assert_eq!(d.len(), 12, "{d:?}");
        for det in &d {
            assert!(det.rect_px.w <= 28, "spans two icons: {det:?}");
        }
        for x in xs {
            assert!(
                d.iter().any(|det| near(det.rect_px, x, 20, 24, 24)),
                "icon at {x}"
            );
        }
    }

    #[test]
    fn words_become_one_line() {
        let mut c = Canvas::new(300, 60, 0xffffff);
        // "Open" 40 px, "recent" 60 px, "file" 30 px, 12 px tall, 3 px gaps.
        c.fill(20, 20, 40, 12, 0x000000);
        c.fill(63, 20, 60, 12, 0x000000);
        c.fill(126, 20, 30, 12, 0x000000);
        let d = detect(&c.frame(1.0));
        assert_eq!(d.len(), 1, "{d:?}");
        assert_eq!(d[0].class, DetectionClass::TextLine);
        assert!(near(d[0].rect_px, 20, 20, 136, 12), "{d:?}");
        assert!((d[0].confidence - CONF_TEXT).abs() < 1e-6);
    }

    #[test]
    fn blue_text_is_link() {
        let mut c = Canvas::new(300, 60, 0xffffff);
        c.fill(20, 20, 40, 12, 0x1e66f5);
        c.fill(63, 20, 60, 12, 0x1e66f5);
        let d = detect(&c.frame(1.0));
        assert_eq!(d.len(), 1, "{d:?}");
        assert_eq!(d[0].class, DetectionClass::Link);
        assert!((d[0].confidence - (CONF_TEXT + CONF_LINK)).abs() < 1e-6);
    }

    #[test]
    fn window_sized_rect_dropped() {
        let mut c = Canvas::new(800, 600, BG);
        c.fill(20, 15, 760, 570, 0xacb0be);
        for i in 0..20 {
            let (x, y) = (60 + (i % 5) * 140, 60 + (i / 5) * 120);
            c.fill(x, y, 80, 40, BTN);
        }
        // Allow the outer box past the size cap so nesting decides.
        let p = DetectParams {
            max_box: 2000,
            ..DetectParams::default()
        };
        let d = ClassicalDetector::new().detect(&c.frame(1.0), &p);
        assert_eq!(d.len(), 20, "{d:?}");
        assert!(d.iter().all(|d| d.rect_px.w < 100));
    }

    #[test]
    fn tiny_noise_dropped() {
        let mut c = Canvas::new(100, 100, BG);
        c.fill(40, 40, 4, 4, 0x000000);
        assert!(detect(&c.frame(1.0)).is_empty());
    }

    #[test]
    fn deadline_returns_partial() {
        let mut c = Canvas::new(400, 200, BG);
        c.fill(40, 40, 100, 40, BTN);
        let p = DetectParams {
            deadline: Some(Instant::now()),
            ..DetectParams::default()
        };
        let d = ClassicalDetector::new().detect(&c.frame(1.0), &p);
        assert!(d.len() <= 1);
    }

    #[test]
    fn hidpi_downscale_maps_back() {
        // 2× frame wider than 1920: factor 2, rects mapped to frame px.
        let mut c = Canvas::new(2400, 400, BG);
        c.fill(200, 100, 240, 72, BTN);
        let f = c.frame(2.0);
        assert_eq!(downscale_factor(f.width, 1920), 2);
        assert_eq!(dilate_kernel(2.0, 2), 3);
        let d = detect(&f);
        assert_eq!(d.len(), 1, "{d:?}");
        let r = d[0].rect_px;
        assert!(
            (r.x - 200).abs() <= 4 && (r.right() - 440).abs() <= 4,
            "{r:?}"
        );
    }

    #[test]
    fn scratch_reused() {
        let mut det = ClassicalDetector::new();
        let c = Canvas::new(200, 100, BG).frame(1.0);
        det.detect(&c, &DetectParams::default());
        let ptr = det.scratch.gray.as_ptr();
        det.detect(&c, &DetectParams::default());
        assert_eq!(ptr, det.scratch.gray.as_ptr());
    }
}

/// `cargo test -p woints-vision --release -- --ignored --nocapture detect_timing`
#[cfg(test)]
mod timing {
    use super::*;
    use woints_core::geom::Global;

    #[test]
    #[ignore]
    fn detect_timing() {
        let (w, h) = (2880u32, 1800u32);
        let mut rgba = vec![240u8; (w * h * 4) as usize];
        for i in 0..400u32 {
            let (x0, y0) = ((i % 20) * 140 + 10, (i / 20) * 88 + 10);
            for y in y0..y0 + 48 {
                for x in x0..x0 + 112 {
                    let k = ((y * w + x) * 4) as usize;
                    rgba[k..k + 3].copy_from_slice(&[60, 70, 90]);
                }
            }
        }
        let frame = Frame {
            rgba,
            width: w,
            height: h,
            rect: Rect::<Global>::new(0, 0, 1440, 900),
            scale: 2.0,
        };
        let mut d = ClassicalDetector::new();
        let mut ms = Vec::new();
        for _ in 0..10 {
            let t = Instant::now();
            let n = d.detect(&frame, &DetectParams::default()).len();
            ms.push(t.elapsed().as_secs_f64() * 1000.0);
            assert!(n > 0);
        }
        ms.sort_by(f64::total_cmp);
        eprintln!("detect 2880x1800: p50 {:.1} ms, max {:.1} ms", ms[5], ms[9]);
    }
}
