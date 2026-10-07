//! ONNX detector pre/post-processing (M6 design D4): letterbox,
//! normalisation, output decoding, NMS and mapping back to frame pixels.
//! Pure math, compiled without the `onnx` feature so it is always tested.

use woints_core::config::{OnnxConfig, OutputFormat};
use woints_core::geom::Rect;
use woints_core::merge::{MergeParams, merge_detections};
use woints_core::vision::{Detection, DetectionClass, DetectionSource, Frame, FramePx};

/// Where the frame sits inside the model input: input px =
/// `frame px * scale + pad`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Letterbox {
    pub scale: f32,
    pub pad_x: f32,
    pub pad_y: f32,
}

/// Placement of a `fw × fh` frame inside a `w × h` input, aspect preserved
/// and centred.
pub fn fit(fw: u32, fh: u32, w: u32, h: u32) -> (Letterbox, u32, u32) {
    let scale = (w as f32 / fw.max(1) as f32).min(h as f32 / fh.max(1) as f32);
    let nw = ((fw as f32 * scale).round() as u32).clamp(1, w);
    let nh = ((fh as f32 * scale).round() as u32).clamp(1, h);
    let lb = Letterbox {
        scale,
        pad_x: ((w - nw) / 2) as f32,
        pad_y: ((h - nh) / 2) as f32,
    };
    (lb, nw, nh)
}

/// Letterbox `frame` into a `size.0 × size.1` RGB8 image (HWC), bilinear,
/// padded with grey `pad`.
pub fn letterbox(frame: &Frame, size: (u32, u32), pad: u8) -> (Vec<u8>, Letterbox) {
    let (w, h) = (size.0 as usize, size.1 as usize);
    let mut out = vec![pad; w * h * 3];
    let (fw, fh) = (frame.width as usize, frame.height as usize);
    if fw == 0 || fh == 0 || frame.rgba.len() < fw * fh * 4 {
        let (lb, _, _) = fit(frame.width, frame.height, size.0, size.1);
        return (out, lb);
    }
    let (lb, nw, nh) = fit(frame.width, frame.height, size.0, size.1);
    let (px, py) = (lb.pad_x as usize, lb.pad_y as usize);
    let sx = fw as f32 / nw as f32;
    let sy = fh as f32 / nh as f32;
    let at = |x: usize, y: usize, c: usize| f32::from(frame.rgba[(y * fw + x) * 4 + c]);
    for oy in 0..nh as usize {
        let fy = ((oy as f32 + 0.5) * sy - 0.5).clamp(0.0, (fh - 1) as f32);
        let (y0, ty) = (fy as usize, fy.fract());
        let y1 = (y0 + 1).min(fh - 1);
        for ox in 0..nw as usize {
            let fx = ((ox as f32 + 0.5) * sx - 0.5).clamp(0.0, (fw - 1) as f32);
            let (x0, tx) = (fx as usize, fx.fract());
            let x1 = (x0 + 1).min(fw - 1);
            let o = ((oy + py) * w + ox + px) * 3;
            for c in 0..3 {
                let top = at(x0, y0, c) * (1.0 - tx) + at(x1, y0, c) * tx;
                let bot = at(x0, y1, c) * (1.0 - tx) + at(x1, y1, c) * tx;
                out[o + c] = (top * (1.0 - ty) + bot * ty).round() as u8;
            }
        }
    }
    (out, lb)
}

/// Resolved normalisation: `(v * scale - mean[c]) / std[c]`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Norm {
    pub scale: f32,
    pub mean: [f32; 3],
    pub std: [f32; 3],
}

impl Default for Norm {
    /// Ultralytics convention: `/255`, no mean/std.
    fn default() -> Self {
        Self {
            scale: 1.0 / 255.0,
            mean: [0.0; 3],
            std: [1.0; 3],
        }
    }
}

/// RGB8 HWC to normalised float32 NCHW.
pub fn normalize(rgb: &[u8], w: u32, h: u32, n: &Norm) -> Vec<f32> {
    let plane = (w * h) as usize;
    let mut out = vec![0.0; plane * 3];
    for (i, px) in rgb.chunks_exact(3).take(plane).enumerate() {
        for c in 0..3 {
            let s = n.std[c];
            let s = if s == 0.0 { 1.0 } else { s };
            out[c * plane + i] = (f32::from(px[c]) * n.scale - n.mean[c]) / s;
        }
    }
    out
}

/// One model output: shape and row-major data.
#[derive(Debug, Clone, PartialEq)]
pub struct Tensor {
    pub shape: Vec<usize>,
    pub data: Vec<f32>,
}

/// A decoded box in model input pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RawBox {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
    pub score: f32,
    pub class: usize,
}

impl RawBox {
    fn area(&self) -> f32 {
        (self.x1 - self.x0).max(0.0) * (self.y1 - self.y0).max(0.0)
    }
    pub fn iou(&self, o: &Self) -> f32 {
        let w = (self.x1.min(o.x1) - self.x0.max(o.x0)).max(0.0);
        let h = (self.y1.min(o.y1) - self.y0.max(o.y0)).max(0.0);
        let i = w * h;
        let u = self.area() + o.area() - i;
        if u <= 0.0 { 0.0 } else { i / u }
    }
}

/// The decoder `format` picks for these outputs: one rank-3 tensor is YOLO,
/// three or more tensors are DETR `(boxes, scores, labels)`.
pub fn resolve_format(format: OutputFormat, outputs: &[Tensor]) -> Result<OutputFormat, String> {
    match format {
        OutputFormat::Auto => match outputs {
            [t] if t.shape.len() == 3 => Ok(OutputFormat::Yolo),
            [_, _, _, ..] => Ok(OutputFormat::Detr),
            _ => Err(format!(
                "cannot detect output format from shapes {:?} (set vision.onnx.output_format)",
                outputs.iter().map(|t| &t.shape).collect::<Vec<_>>()
            )),
        },
        f => Ok(f),
    }
}

/// Decode `outputs` into boxes in model input pixels. `classes` is the
/// class count; `input` the model `(width, height)` for normalised DETR boxes.
pub fn decode(
    format: OutputFormat,
    outputs: &[Tensor],
    classes: usize,
    input: (u32, u32),
) -> Result<Vec<RawBox>, String> {
    match resolve_format(format, outputs)? {
        OutputFormat::Yolo => decode_yolo(outputs.first().ok_or("no output")?, classes),
        _ => decode_detr(outputs, input),
    }
}

/// `[1, 4+C, N]` (Ultralytics export) or `[1, N, 4+C]`, boxes as
/// `cx, cy, w, h`.
pub fn decode_yolo(t: &Tensor, classes: usize) -> Result<Vec<RawBox>, String> {
    let [1, a, b] = t.shape[..] else {
        return Err(format!("yolo: expected [1, 4+C, N], got {:?}", t.shape));
    };
    let attrs = 4 + classes.max(1);
    let channels_first = if a == attrs {
        true
    } else if b == attrs {
        false
    } else {
        return Err(format!(
            "yolo: shape {:?} does not match {} classes",
            t.shape, classes
        ));
    };
    let n = if channels_first { b } else { a };
    if t.data.len() < n * attrs {
        return Err("yolo: output shorter than its shape".into());
    }
    let get = |i: usize, k: usize| {
        if channels_first {
            t.data[k * n + i]
        } else {
            t.data[i * attrs + k]
        }
    };
    Ok((0..n)
        .map(|i| {
            let (cx, cy, w, h) = (get(i, 0), get(i, 1), get(i, 2), get(i, 3));
            let (class, score) = (0..attrs - 4)
                .map(|c| (c, get(i, 4 + c)))
                .fold((0, f32::MIN), |m, x| if x.1 > m.1 { x } else { m });
            RawBox {
                x0: cx - w / 2.0,
                y0: cy - h / 2.0,
                x1: cx + w / 2.0,
                y1: cy + h / 2.0,
                score,
                class,
            }
        })
        .collect())
}

/// `boxes [1, N, 4]` as `x0, y0, x1, y1` (pixels, or normalised when all
/// coordinates are within `[0, 1]`), `scores [1, N]`, `labels [1, N]`.
pub fn decode_detr(outputs: &[Tensor], input: (u32, u32)) -> Result<Vec<RawBox>, String> {
    let [boxes, scores, labels, ..] = outputs else {
        return Err("detr: expected boxes, scores and labels outputs".into());
    };
    let n = scores.data.len();
    if boxes.data.len() < n * 4 || labels.data.len() < n {
        return Err(format!(
            "detr: inconsistent shapes {:?} {:?} {:?}",
            boxes.shape, scores.shape, labels.shape
        ));
    }
    let normalised = boxes.data[..n * 4].iter().all(|v| (0.0..=1.0).contains(v));
    let (sx, sy) = if normalised {
        (input.0 as f32, input.1 as f32)
    } else {
        (1.0, 1.0)
    };
    Ok((0..n)
        .map(|i| {
            let b = &boxes.data[i * 4..i * 4 + 4];
            RawBox {
                x0: b[0] * sx,
                y0: b[1] * sy,
                x1: b[2] * sx,
                y1: b[3] * sy,
                score: scores.data[i],
                class: labels.data[i].max(0.0) as usize,
            }
        })
        .collect())
}

/// Thresholds applied after decoding; read per request so SIGHUP changes
/// apply without a new session.
#[derive(Debug, Clone, PartialEq)]
pub struct PostParams {
    pub min_confidence: f32,
    pub nms_iou: f32,
    /// Allowed class indices; empty = all.
    pub keep: Vec<usize>,
}

impl PostParams {
    /// From `[vision.onnx]`; `interactive_classes` resolved against `names`.
    pub fn from_config(c: &OnnxConfig, names: &[String]) -> Self {
        Self {
            min_confidence: c.min_confidence,
            nms_iou: c.nms_iou,
            keep: c
                .interactive_classes
                .iter()
                .filter_map(|k| names.iter().position(|n| n == k))
                .collect(),
        }
    }
}

/// `min_confidence` and class filter, then class-agnostic greedy NMS.
pub fn filter_nms(mut boxes: Vec<RawBox>, p: &PostParams) -> Vec<RawBox> {
    boxes.retain(|b| {
        b.score >= p.min_confidence
            && b.area() > 0.0
            && (p.keep.is_empty() || p.keep.contains(&b.class))
    });
    boxes.sort_by(|a, b| b.score.total_cmp(&a.score));
    let mut kept: Vec<RawBox> = Vec::new();
    for b in boxes {
        if kept.iter().all(|k| k.iou(&b) <= p.nms_iou) {
            kept.push(b);
        }
    }
    kept
}

/// Model input box back to frame pixels, clamped to the frame.
pub fn unletterbox(b: &RawBox, lb: &Letterbox, fw: u32, fh: u32) -> Rect<FramePx> {
    let s = lb.scale.max(f32::EPSILON);
    let x = |v: f32| ((v - lb.pad_x) / s).round().clamp(0.0, fw as f32) as i32;
    let y = |v: f32| ((v - lb.pad_y) / s).round().clamp(0.0, fh as f32) as i32;
    let (x0, y0, x1, y1) = (x(b.x0), y(b.y0), x(b.x1), y(b.y1));
    Rect::new(x0, y0, x1 - x0, y1 - y0)
}

/// Element class from a model class name.
pub fn class_of(name: &str) -> DetectionClass {
    let n = name.to_ascii_lowercase();
    if n.contains("link") {
        DetectionClass::Link
    } else if n.contains("text") {
        DetectionClass::TextLine
    } else if n.contains("image") || n.contains("img") || n.contains("picture") {
        DetectionClass::Image
    } else {
        DetectionClass::Box
    }
}

/// Filtered boxes as frame-pixel detections.
pub fn to_detections(
    boxes: &[RawBox],
    lb: &Letterbox,
    frame: &Frame,
    names: &[String],
) -> Vec<Detection> {
    boxes
        .iter()
        .map(|b| Detection {
            rect_px: unletterbox(b, lb, frame.width, frame.height),
            confidence: b.score.clamp(0.0, 1.0),
            class: names
                .get(b.class)
                .map_or(DetectionClass::Unknown, |n| class_of(n)),
            source: DetectionSource::Onnx,
        })
        .filter(|d| d.rect_px.w > 0 && d.rect_px.h > 0)
        .collect()
}

/// `onnx+classical`: ONNX detections first, classical ones not duplicating
/// any of them.
pub fn chain(onnx: Vec<Detection>, classical: Vec<Detection>, p: &MergeParams) -> Vec<Detection> {
    merge_detections(onnx, classical, p)
}

/// Ultralytics `names` metadata: `{0: 'button', 1: 'icon'}`.
pub fn parse_names(s: &str) -> Option<Vec<String>> {
    let body = s.trim().strip_prefix('{')?.strip_suffix('}')?;
    let mut pairs: Vec<(usize, String)> = Vec::new();
    for part in split_top(body) {
        let (k, v) = part.split_once(':')?;
        let v = v.trim();
        let v = v
            .strip_prefix('\'')
            .and_then(|v| v.strip_suffix('\''))
            .or_else(|| v.strip_prefix('"').and_then(|v| v.strip_suffix('"')))
            .unwrap_or(v);
        pairs.push((k.trim().trim_matches(['\'', '"']).parse().ok()?, v.into()));
    }
    pairs.sort_by_key(|p| p.0);
    Some(pairs.into_iter().map(|p| p.1).collect())
}

/// Commas outside quotes.
fn split_top(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let (mut start, mut quote) = (0, None);
    for (i, c) in s.char_indices() {
        match (c, quote) {
            ('\'' | '"', None) => quote = Some(c),
            (c, Some(q)) if c == q => quote = None,
            (',', None) => {
                out.push(&s[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    if !s[start..].trim().is_empty() {
        out.push(&s[start..]);
    }
    out
}

/// `imgsz` / `woints.input_size` metadata: `[640, 640]` or `640`, as
/// `(width, height)`; Ultralytics writes `[h, w]`.
pub fn parse_size(s: &str) -> Option<(u32, u32)> {
    let v: Vec<u32> = s
        .trim()
        .trim_matches(['[', ']', '(', ')'])
        .split(',')
        .map(|x| x.trim().parse().ok())
        .collect::<Option<_>>()?;
    match v[..] {
        [n] => Some((n, n)),
        [h, w] => Some((w, h)),
        _ => None,
    }
}

/// `woints.normalization` metadata: `scale;m0,m1,m2;s0,s1,s2`.
pub fn parse_norm(s: &str) -> Option<Norm> {
    let mut parts = s.split(';');
    let scale = parts.next()?.trim().parse().ok()?;
    let triple = |p: Option<&str>| -> Option<[f32; 3]> {
        let v: Vec<f32> = p?
            .split(',')
            .map(|x| x.trim().parse().ok())
            .collect::<Option<_>>()?;
        v.try_into().ok()
    };
    Some(Norm {
        scale,
        mean: triple(parts.next()).unwrap_or([0.0; 3]),
        std: triple(parts.next()).unwrap_or([1.0; 3]),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use woints_core::geom::Global;

    fn frame(w: u32, h: u32) -> Frame {
        Frame {
            rgba: vec![255; (w * h * 4) as usize],
            width: w,
            height: h,
            rect: Rect::<Global>::new(0, 0, 1440, 900),
            scale: 2.0,
        }
    }

    #[test]
    fn letterbox_2880x1800() {
        let (rgb, lb) = letterbox(&frame(2880, 1800), (640, 640), 114);
        assert!((lb.scale - 640.0 / 2880.0).abs() < 1e-6);
        assert!((lb.scale - 0.2222).abs() < 1e-4);
        assert_eq!((lb.pad_x, lb.pad_y), (0.0, 120.0));
        assert_eq!(rgb.len(), 640 * 640 * 3);
        // Padding above, image inside.
        assert_eq!(rgb[(119 * 640) * 3], 114);
        assert_eq!(rgb[(120 * 640) * 3], 255);
        assert_eq!(rgb[(519 * 640) * 3], 255);
        assert_eq!(rgb[(520 * 640) * 3], 114);
    }

    #[test]
    fn normalisation_nchw() {
        let rgb = [255, 0, 51, 0, 255, 102];
        let n = Norm {
            scale: 1.0 / 255.0,
            mean: [0.5, 0.0, 0.0],
            std: [0.5, 1.0, 1.0],
        };
        let t = normalize(&rgb, 2, 1, &n);
        assert_eq!(t.len(), 6);
        assert!((t[0] - 1.0).abs() < 1e-6 && (t[1] + 1.0).abs() < 1e-6);
        assert_eq!(&t[2..4], &[0.0, 1.0]);
        assert!((t[4] - 0.2).abs() < 1e-6 && (t[5] - 0.4).abs() < 1e-6);
    }

    fn yolo_cf() -> Tensor {
        // [1, 4+2, 2]: box 0 class 1, box 1 class 0.
        Tensor {
            shape: vec![1, 6, 2],
            data: vec![
                10.0, 50.0, 10.0, 50.0, 4.0, 20.0, 4.0, 20.0, 0.1, 0.7, 0.9, 0.2,
            ],
        }
    }

    #[test]
    fn yolo_layouts() {
        let a = decode_yolo(&yolo_cf(), 2).unwrap();
        assert_eq!(a.len(), 2);
        assert_eq!((a[0].x0, a[0].y0, a[0].x1, a[0].y1), (8.0, 8.0, 12.0, 12.0));
        assert_eq!((a[0].class, a[0].score), (1, 0.9));
        assert_eq!((a[1].class, a[1].score), (0, 0.7));
        // Same boxes transposed to [1, N, 4+C].
        let cf = yolo_cf();
        let mut data = Vec::new();
        for i in 0..2 {
            for k in 0..6 {
                data.push(cf.data[k * 2 + i]);
            }
        }
        let b = decode_yolo(
            &Tensor {
                shape: vec![1, 2, 6],
                data,
            },
            2,
        )
        .unwrap();
        assert_eq!(a, b);
        assert!(decode_yolo(&yolo_cf(), 5).is_err());
    }

    #[test]
    fn detr_and_auto() {
        let outs = [
            Tensor {
                shape: vec![1, 1, 4],
                data: vec![0.0, 0.25, 0.5, 0.75],
            },
            Tensor {
                shape: vec![1, 1],
                data: vec![0.8],
            },
            Tensor {
                shape: vec![1, 1],
                data: vec![2.0],
            },
        ];
        assert_eq!(
            resolve_format(OutputFormat::Auto, &outs),
            Ok(OutputFormat::Detr)
        );
        assert_eq!(
            resolve_format(OutputFormat::Auto, &[yolo_cf()]),
            Ok(OutputFormat::Yolo)
        );
        assert!(resolve_format(OutputFormat::Auto, &outs[1..]).is_err());
        let b = decode(OutputFormat::Auto, &outs, 3, (640, 640)).unwrap();
        assert_eq!(
            (b[0].x0, b[0].y0, b[0].x1, b[0].y1),
            (0.0, 160.0, 320.0, 480.0)
        );
        assert_eq!((b[0].class, b[0].score), (2, 0.8));
    }

    fn rb(x0: f32, y0: f32, x1: f32, y1: f32, score: f32, class: usize) -> RawBox {
        RawBox {
            x0,
            y0,
            x1,
            y1,
            score,
            class,
        }
    }

    #[test]
    fn nms_threshold_and_classes() {
        let boxes = vec![
            rb(10.0, 10.0, 30.0, 30.0, 0.9, 0),
            rb(12.0, 11.0, 32.0, 31.0, 0.8, 0),
            rb(45.0, 45.0, 55.0, 55.0, 0.1, 0),
            rb(60.0, 60.0, 70.0, 70.0, 0.6, 1),
        ];
        let p = PostParams {
            min_confidence: 0.3,
            nms_iou: 0.5,
            keep: vec![],
        };
        let k = filter_nms(boxes.clone(), &p);
        assert_eq!(k.len(), 2);
        assert_eq!(k[0].score, 0.9);
        let k = filter_nms(boxes, &PostParams { keep: vec![0], ..p });
        assert_eq!(k.len(), 1);
        let names = vec!["button".to_owned(), "text".to_owned()];
        let c = OnnxConfig {
            interactive_classes: vec!["text".into(), "nope".into()],
            ..OnnxConfig::default()
        };
        assert_eq!(PostParams::from_config(&c, &names).keep, [1]);
    }

    #[test]
    fn full_frame_maps_to_output() {
        let f = frame(2880, 1800);
        let (lb, _, _) = fit(2880, 1800, 640, 640);
        let b = rb(0.0, 120.0, 640.0, 520.0, 0.9, 0);
        let px = unletterbox(&b, &lb, f.width, f.height);
        assert_eq!(px, Rect::new(0, 0, 2880, 1800));
        assert_eq!(f.to_global(px), Rect::<Global>::new(0, 0, 1440, 900));
        let d = to_detections(&[b], &lb, &f, &["link".into()]);
        assert_eq!(d[0].class, DetectionClass::Link);
        assert_eq!(d[0].source, DetectionSource::Onnx);
    }

    #[test]
    fn chain_onnx_wins() {
        let d = |x: i32, source| Detection {
            rect_px: Rect::new(x, 0, 100, 100),
            confidence: 0.5,
            class: DetectionClass::Box,
            source,
        };
        // IoU((0,0,100,100), (10,0,100,100)) = 9000 / 11000 ≈ 0.82.
        let a = d(0, DetectionSource::Onnx);
        let b = d(10, DetectionSource::Classical);
        let far = d(500, DetectionSource::Classical);
        let p = MergeParams {
            iou_threshold: 0.5,
            ..MergeParams::default()
        };
        assert_eq!(chain(vec![a], vec![b, far], &p), vec![a, far]);
        assert_eq!(chain(vec![a], vec![b], &p)[0].source, DetectionSource::Onnx);
    }

    #[test]
    fn metadata_parsing() {
        assert_eq!(
            parse_names("{0: 'button', 1: \"icon, small\", 2: 'link'}").unwrap(),
            ["button", "icon, small", "link"]
        );
        assert_eq!(parse_size("[480, 640]"), Some((640, 480)));
        assert_eq!(parse_size("640"), Some((640, 640)));
        assert_eq!(parse_size("x"), None);
        let n = parse_norm("0.0039;0.485,0.456,0.406;0.229,0.224,0.225").unwrap();
        assert_eq!(n.mean, [0.485, 0.456, 0.406]);
        assert_eq!(parse_norm("1").unwrap().std, [1.0; 3]);
    }
}
