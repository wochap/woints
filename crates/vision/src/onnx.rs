//! ONNX Runtime detector (M6): one warm session, metadata-driven
//! pre/post-processing from [`crate::prep`].

use std::path::{Path, PathBuf};
use std::time::Instant;

use ort::session::Session;
use ort::value::Tensor as OrtTensor;
use woints_core::config::{Ep, OnnxConfig, OutputFormat};
use woints_core::model::Timings;
use woints_core::vision::{Detection, Frame};

use crate::detect::{DetectParams, Detector};
use crate::prep::{self, Norm, PostParams, Tensor};

/// What `doctor` and `status` show about a loaded model.
#[derive(Debug, Clone, PartialEq)]
pub struct OnnxInfo {
    pub path: PathBuf,
    pub input_shape: Vec<i64>,
    pub output_names: Vec<String>,
    pub ep_requested: Ep,
    pub ep_active: Ep,
    pub threads: usize,
    pub warmup_ms: f64,
}

/// Metadata read from the model, before TOML overrides.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ModelMeta {
    pub names: Option<Vec<String>>,
    pub input_size: Option<(u32, u32)>,
    pub norm: Option<Norm>,
    pub format: Option<OutputFormat>,
}

/// Metadata with overrides applied.
#[derive(Debug, Clone, PartialEq)]
pub struct Resolved {
    pub names: Vec<String>,
    pub input: (u32, u32),
    pub norm: Norm,
    pub format: OutputFormat,
    pub pad: u8,
}

impl Resolved {
    /// Overrides win; missing keys are errors naming the config key.
    pub fn new(meta: &ModelMeta, c: &OnnxConfig) -> Result<Self, String> {
        let names = if c.class_names.is_empty() {
            meta.names
                .clone()
                .ok_or("class_names missing (set vision.onnx.class_names)")?
        } else {
            c.class_names.clone()
        };
        let input = match c.input_size[..] {
            [w, h] if w > 0 && h > 0 => (w, h),
            [] => meta
                .input_size
                .ok_or("input_size missing (set vision.onnx.input_size)")?,
            _ => return Err("vision.onnx.input_size: expected [width, height]".into()),
        };
        let n = &c.normalization;
        let base = meta.norm.unwrap_or_default();
        let triple = |v: &[f32], d: [f32; 3]| <[f32; 3]>::try_from(v).unwrap_or(d);
        let norm = Norm {
            scale: if n.scale > 0.0 { n.scale } else { base.scale },
            mean: triple(&n.mean, base.mean),
            std: triple(&n.std, base.std),
        };
        let format = match c.output_format {
            OutputFormat::Auto => meta.format.unwrap_or(OutputFormat::Auto),
            f => f,
        };
        Ok(Self {
            names,
            input,
            norm,
            format,
            pad: c.pad,
        })
    }
}

pub struct OnnxDetector {
    session: Session,
    input_name: String,
    meta: ModelMeta,
    info: OnnxInfo,
    timings: Timings,
}

/// Physical cores from `/proc/cpuinfo`, else logical CPUs.
fn physical_cores() -> usize {
    let logical = std::thread::available_parallelism().map_or(2, |n| n.get());
    let Ok(s) = std::fs::read_to_string("/proc/cpuinfo") else {
        return logical;
    };
    let mut cores = std::collections::HashSet::new();
    let mut phys = "";
    for line in s.lines() {
        if let Some((k, v)) = line.split_once(':') {
            match k.trim() {
                "physical id" => phys = v.trim(),
                "core id" => {
                    cores.insert((phys.to_owned(), v.trim().to_owned()));
                }
                _ => {}
            }
        }
    }
    if cores.is_empty() {
        logical
    } else {
        cores.len()
    }
}

/// `vision.onnx.threads`, 0 = physical cores / 2.
pub fn threads(c: &OnnxConfig) -> usize {
    if c.threads > 0 {
        c.threads
    } else {
        (physical_cores() / 2).max(1)
    }
}

/// Whether this build can register `ep`.
fn ep_compiled(ep: Ep) -> bool {
    match ep {
        Ep::Cpu => true,
        Ep::Cuda => cfg!(feature = "cuda"),
        Ep::Migraphx => cfg!(feature = "migraphx"),
    }
}

/// Load ONNX Runtime from `ORT_DYLIB_PATH` (or the linker path) once,
/// returning an error instead of panicking.
fn init_runtime() -> Result<(), String> {
    static INIT: std::sync::OnceLock<Result<(), String>> = std::sync::OnceLock::new();
    INIT.get_or_init(|| {
        let lib = std::env::var("ORT_DYLIB_PATH")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "libonnxruntime.so".into());
        ort::init_from(&lib)
            .map(|b| {
                b.with_name("woints").commit();
            })
            .map_err(|e| format!("cannot load ONNX Runtime {lib}: {e}"))
    })
    .clone()
}

fn read_meta(session: &Session) -> ModelMeta {
    let Ok(m) = session.metadata() else {
        return ModelMeta::default();
    };
    let get = |k: &str| m.custom(k).filter(|s| !s.trim().is_empty());
    let names = get("woints.class_names")
        .map(|s| s.split(',').map(|n| n.trim().to_owned()).collect())
        .or_else(|| get("names").and_then(|s| prep::parse_names(&s)));
    let input_size = get("woints.input_size")
        .or_else(|| get("imgsz"))
        .and_then(|s| prep::parse_size(&s));
    let norm = get("woints.normalization").and_then(|s| prep::parse_norm(&s));
    let format = get("woints.output_format").and_then(|s| match s.trim() {
        "yolo" => Some(OutputFormat::Yolo),
        "detr" => Some(OutputFormat::Detr),
        _ => None,
    });
    ModelMeta {
        names,
        input_size,
        norm,
        format,
    }
}

fn build(c: &OnnxConfig, path: &Path) -> Result<(Session, Ep, usize), String> {
    let e = |e: ort::Error<_>| e.to_string();
    let threads = threads(c);
    let mut b = Session::builder()
        .map_err(|e| e.to_string())?
        .with_intra_threads(threads)
        .map_err(e)?;
    let mut active = Ep::Cpu;
    if c.ep != Ep::Cpu {
        let registered = ep_compiled(c.ep) && register(c.ep, &mut b);
        if registered {
            active = c.ep;
        } else {
            tracing::warn!("onnx: ep {} unavailable, using cpu", c.ep.as_str());
        }
    }
    let s = b.commit_from_file(path).map_err(|e| e.to_string())?;
    Ok((s, active, threads))
}

#[allow(unused_variables)]
fn register(ep: Ep, b: &mut ort::session::builder::SessionBuilder) -> bool {
    match ep {
        #[cfg(feature = "cuda")]
        Ep::Cuda => ort::ep::ExecutionProvider::register(&ort::ep::CUDA::default(), b).is_ok(),
        #[cfg(feature = "migraphx")]
        Ep::Migraphx => {
            ort::ep::ExecutionProvider::register(&ort::ep::MIGraphX::default(), b).is_ok()
        }
        _ => false,
    }
}

impl OnnxDetector {
    /// Load the model, resolve metadata against `c`, run one warm-up.
    pub fn new(c: &OnnxConfig) -> Result<Self, String> {
        if c.model_path.is_empty() {
            return Err("model_path not set (set vision.onnx.model_path)".into());
        }
        let path = c.model_path();
        if !path.is_file() {
            return Err(format!("model not found: {}", path.display()));
        }
        init_runtime()?;
        let (session, active, threads) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(
            || build(c, &path),
        ))
        .map_err(|_| "ONNX Runtime panicked while loading (version mismatch?)".to_owned())??;
        let input = session.inputs().first().ok_or("model has no inputs")?;
        let input_name = input.name().to_owned();
        let input_shape: Vec<i64> = input
            .dtype()
            .tensor_shape()
            .map(|s| s.iter().copied().collect())
            .unwrap_or_default();
        let output_names = session
            .outputs()
            .iter()
            .map(|o| o.name().to_owned())
            .collect();
        let mut meta = read_meta(&session);
        if meta.input_size.is_none()
            && let [_, _, h, w] = input_shape[..]
            && h > 0
            && w > 0
        {
            meta.input_size = Some((w as u32, h as u32));
        }
        let r = Resolved::new(&meta, c)?;
        let mut d = Self {
            session,
            input_name,
            meta,
            info: OnnxInfo {
                path,
                input_shape,
                output_names,
                ep_requested: c.ep,
                ep_active: active,
                threads,
                warmup_ms: 0.0,
            },
            timings: Timings::default(),
        };
        let t = Instant::now();
        let blank = vec![f32::from(r.pad) * r.norm.scale; (r.input.0 * r.input.1 * 3) as usize];
        d.infer(blank, r.input)?;
        d.info.warmup_ms = t.elapsed().as_secs_f64() * 1000.0;
        tracing::info!(
            "onnx: loaded {} input={:?} ep={} warmup_ms={:.0}",
            d.info.path.display(),
            d.info.input_shape,
            active.as_str(),
            d.info.warmup_ms
        );
        Ok(d)
    }

    pub fn info(&self) -> &OnnxInfo {
        &self.info
    }

    fn infer(&mut self, data: Vec<f32>, input: (u32, u32)) -> Result<Vec<Tensor>, String> {
        let shape = vec![1i64, 3, i64::from(input.1), i64::from(input.0)];
        let t = OrtTensor::from_array((shape, data)).map_err(|e| e.to_string())?;
        let outs = self
            .session
            .run(ort::inputs![self.input_name.as_str() => t])
            .map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        for (_, v) in &outs {
            let (shape, data): (Vec<usize>, Vec<f32>) =
                if let Ok((s, d)) = v.try_extract_tensor::<f32>() {
                    (s.iter().map(|&x| x.max(0) as usize).collect(), d.to_vec())
                } else if let Ok((s, d)) = v.try_extract_tensor::<i64>() {
                    (
                        s.iter().map(|&x| x.max(0) as usize).collect(),
                        d.iter().map(|&x| x as f32).collect(),
                    )
                } else {
                    return Err("output is neither f32 nor i64".into());
                };
            out.push(Tensor { shape, data });
        }
        Ok(out)
    }

    fn run(&mut self, frame: &Frame, c: &OnnxConfig) -> Result<Vec<Detection>, String> {
        let r = Resolved::new(&self.meta, c)?;
        let t = Instant::now();
        let (rgb, lb) = prep::letterbox(frame, r.input, r.pad);
        let data = prep::normalize(&rgb, r.input.0, r.input.1, &r.norm);
        self.timings.record("detect.onnx.pre", t.elapsed());
        let t = Instant::now();
        let outs = self.infer(data, r.input)?;
        self.timings.record("detect.onnx.run", t.elapsed());
        let t = Instant::now();
        let boxes = prep::decode(r.format, &outs, r.names.len(), r.input)?;
        let kept = prep::filter_nms(boxes, &PostParams::from_config(c, &r.names));
        let d = prep::to_detections(&kept, &lb, frame, &r.names);
        self.timings.record("detect.onnx.post", t.elapsed());
        Ok(d)
    }
}

impl Detector for OnnxDetector {
    fn detect(&mut self, frame: &Frame, p: &DetectParams) -> Vec<Detection> {
        let _span = tracing::debug_span!("detect.onnx").entered();
        self.timings = Timings::default();
        let t = Instant::now();
        let out = self.run(frame, &p.onnx).unwrap_or_else(|e| {
            tracing::warn!("onnx: {e}");
            Vec::new()
        });
        let mut all = Timings::default();
        all.record("detect.onnx", t.elapsed());
        all.0.append(&mut self.timings.0);
        self.timings = all;
        out
    }

    fn take_timings(&mut self) -> Timings {
        std::mem::take(&mut self.timings)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overrides_and_missing_keys() {
        let meta = ModelMeta {
            input_size: Some((64, 64)),
            ..Default::default()
        };
        let c = OnnxConfig::default();
        assert_eq!(
            Resolved::new(&meta, &c).unwrap_err(),
            "class_names missing (set vision.onnx.class_names)"
        );
        let c = OnnxConfig {
            class_names: vec!["button".into()],
            input_size: vec![320, 240],
            ..c
        };
        let r = Resolved::new(&meta, &c).unwrap();
        assert_eq!(r.input, (320, 240));
        assert_eq!(r.norm, Norm::default());
    }

    /// Needs `libonnxruntime.so` (`ORT_DYLIB_PATH`); skipped otherwise.
    #[test]
    fn const_model_keeps_one_box() {
        if std::env::var_os("ORT_DYLIB_PATH").is_none() {
            eprintln!("ORT_DYLIB_PATH unset; skipping");
            return;
        }
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/const_boxes.onnx"
        );
        let c = OnnxConfig {
            model_path: path.into(),
            min_confidence: 0.3,
            nms_iou: 0.5,
            ..OnnxConfig::default()
        };
        let mut d = OnnxDetector::new(&c).unwrap();
        assert_eq!(d.info().input_shape, [1, 3, 64, 64]);
        assert_eq!(d.info().output_names, ["output0"]);
        let frame = Frame {
            rgba: vec![0; 64 * 64 * 4],
            width: 64,
            height: 64,
            rect: woints_core::geom::Rect::new(0, 0, 64, 64),
            scale: 1.0,
        };
        let p = DetectParams {
            onnx: c,
            ..DetectParams::default()
        };
        let out = d.detect(&frame, &p);
        assert_eq!(out.len(), 1, "{out:?}");
        assert_eq!(out[0].confidence, 0.9);
        assert_eq!(out[0].rect_px, woints_core::geom::Rect::new(10, 10, 20, 20));
        let stages: Vec<_> = d.take_timings().0.into_iter().map(|s| s.stage).collect();
        assert_eq!(
            stages,
            [
                "detect.onnx",
                "detect.onnx.pre",
                "detect.onnx.run",
                "detect.onnx.post"
            ]
        );
    }
}
