//! Frame capture on the overlay's Wayland connection (design D13, D19):
//! `hyprland_toplevel_export_v1` for toplevels, `zwlr_screencopy_v1` regions
//! for layers. One capture at a time into a shared `wl_shm` pool that grows
//! but never shrinks.

use std::time::Instant;

use smithay_client_toolkit::shm::{Shm, raw::RawPool};
use tokio::sync::oneshot;
use wayland_client::{
    Connection, Dispatch, Proxy, QueueHandle, WEnum,
    globals::GlobalList,
    protocol::{wl_buffer, wl_shm},
};
use wayland_protocols_wlr::screencopy::v1::client::{
    zwlr_screencopy_frame_v1 as sc_frame, zwlr_screencopy_manager_v1 as sc_manager,
};

use woints_core::geom::{Global, OutputId, OutputLocal, Rect};
use woints_core::ports::CaptureError;
use woints_core::vision::Frame;

use crate::wayland::State;

#[allow(
    non_upper_case_globals,
    non_camel_case_types,
    missing_docs,
    unused_imports
)]
#[allow(clippy::all)]
pub mod protocol {
    use wayland_client;
    use wayland_client::protocol::*;
    use wayland_protocols_wlr::foreign_toplevel::v1::client::*;

    pub mod __interfaces {
        use wayland_client::protocol::__interfaces::*;
        use wayland_protocols_wlr::foreign_toplevel::v1::client::__interfaces::*;
        wayland_scanner::generate_interfaces!("protocols/hyprland-toplevel-export-v1.xml");
    }
    use self::__interfaces::*;

    wayland_scanner::generate_client_code!("protocols/hyprland-toplevel-export-v1.xml");
}

use protocol::{
    hyprland_toplevel_export_frame_v1 as ex_frame,
    hyprland_toplevel_export_manager_v1 as ex_manager,
};

/// Initial capture pool size; grows to the largest frame seen.
const POOL_INITIAL: usize = 4 << 20;

/// What to capture, already resolved to compositor terms.
#[derive(Debug, Clone, PartialEq)]
pub enum CaptureReq {
    /// Toplevel by Hyprland window address (lower 32 bits, see the protocol).
    Toplevel { handle: u32, rect: Rect<Global> },
    /// Output-local logical region of `output`.
    Region {
        output: OutputId,
        local: Rect<OutputLocal>,
        rect: Rect<Global>,
    },
}

pub(crate) type CaptureReply = oneshot::Sender<Result<Frame, CaptureError>>;

/// Hyprland window address (`0x55d1e7b0…`) to the export protocol's handle.
pub fn toplevel_handle(address: &str) -> Option<u32> {
    let hex = address.trim_start_matches("0x");
    u64::from_str_radix(hex, 16).ok().map(|v| v as u32)
}

#[derive(Debug, Clone, Copy)]
struct ShmSpec {
    format: wl_shm::Format,
    width: u32,
    height: u32,
    stride: u32,
}

enum JobFrame {
    Export(ex_frame::HyprlandToplevelExportFrameV1),
    Screencopy(sc_frame::ZwlrScreencopyFrameV1),
}

impl JobFrame {
    fn destroy(&self) {
        match self {
            JobFrame::Export(f) => f.destroy(),
            JobFrame::Screencopy(f) => f.destroy(),
        }
    }
}

struct Job {
    reply: CaptureReply,
    rect: Rect<Global>,
    frame: JobFrame,
    shm: Option<ShmSpec>,
    buffer: Option<wl_buffer::WlBuffer>,
    y_invert: bool,
    started: Instant,
}

/// Capture managers and the shared pool, bound once at startup.
pub(crate) struct Capturer {
    export: Option<ex_manager::HyprlandToplevelExportManagerV1>,
    screencopy: Option<sc_manager::ZwlrScreencopyManagerV1>,
    pool: Option<RawPool>,
    job: Option<Job>,
}

/// User data of capture `wl_buffer`s.
pub(crate) struct CaptureBuffer;

fn supported(format: wl_shm::Format) -> bool {
    matches!(
        format,
        wl_shm::Format::Argb8888
            | wl_shm::Format::Xrgb8888
            | wl_shm::Format::Abgr8888
            | wl_shm::Format::Xbgr8888
    )
}

/// Copy `src` (`stride` bytes per row, `format`) into tightly packed RGBA8.
fn to_rgba(src: &[u8], spec: ShmSpec, y_invert: bool) -> Vec<u8> {
    let (w, h, stride) = (
        spec.width as usize,
        spec.height as usize,
        spec.stride as usize,
    );
    let mut out = vec![0u8; w * h * 4];
    let bgr = matches!(
        spec.format,
        wl_shm::Format::Argb8888 | wl_shm::Format::Xrgb8888
    );
    let opaque = matches!(
        spec.format,
        wl_shm::Format::Xrgb8888 | wl_shm::Format::Xbgr8888
    );
    for y in 0..h {
        let sy = if y_invert { h - 1 - y } else { y };
        let row = &src[sy * stride..sy * stride + w * 4];
        let dst = &mut out[y * w * 4..(y + 1) * w * 4];
        for (d, s) in dst.chunks_exact_mut(4).zip(row.chunks_exact(4)) {
            if bgr {
                d.copy_from_slice(&[s[2], s[1], s[0], s[3]]);
            } else {
                d.copy_from_slice(s);
            }
            if opaque {
                d[3] = 255;
            }
        }
    }
    out
}

impl Capturer {
    pub(crate) fn bind(globals: &GlobalList, qh: &QueueHandle<State>, shm: &Shm) -> Self {
        let export: Option<ex_manager::HyprlandToplevelExportManagerV1> =
            globals.bind(qh, 1..=2, ()).ok();
        let screencopy: Option<sc_manager::ZwlrScreencopyManagerV1> =
            globals.bind(qh, 1..=3, ()).ok();
        if export.is_none() {
            tracing::info!("hyprland_toplevel_export_manager_v1 not advertised");
        }
        if screencopy.is_none() {
            tracing::info!("zwlr_screencopy_manager_v1 not advertised");
        }
        let pool = if export.is_some() || screencopy.is_some() {
            RawPool::new(POOL_INITIAL, shm)
                .map_err(|e| tracing::warn!("capture shm pool: {e}"))
                .ok()
        } else {
            None
        };
        Self {
            export,
            screencopy,
            pool,
            job: None,
        }
    }

    fn finish(&mut self, res: Result<Frame, CaptureError>) {
        if let Some(job) = self.job.take() {
            job.frame.destroy();
            if let Some(b) = job.buffer {
                b.destroy();
            }
            let _ = job.reply.send(res);
        }
    }

    fn on_buffer(&mut self, format: WEnum<wl_shm::Format>, width: u32, height: u32, stride: u32) {
        let Some(job) = self.job.as_mut() else {
            return;
        };
        if let WEnum::Value(format) = format
            && job.shm.is_none()
            && supported(format)
        {
            job.shm = Some(ShmSpec {
                format,
                width,
                height,
                stride,
            });
        }
    }

    /// All buffer types announced: allocate and request the copy.
    fn on_buffer_done(&mut self, qh: &QueueHandle<State>) {
        let Some(spec) = self.job.as_ref().map(|j| j.shm) else {
            return;
        };
        if self.job.as_ref().is_some_and(|j| j.buffer.is_some()) {
            return;
        }
        let Some(spec) = spec else {
            self.finish(Err(CaptureError::Unavailable(
                "no supported wl_shm format".into(),
            )));
            return;
        };
        let size = spec.stride as usize * spec.height as usize;
        let Some(pool) = self.pool.as_mut() else {
            self.finish(Err(CaptureError::Unavailable("no shm pool".into())));
            return;
        };
        if let Err(e) = pool.resize(size) {
            self.finish(Err(CaptureError::Unavailable(format!("shm resize: {e}"))));
            return;
        }
        let buffer = pool.create_buffer(
            0,
            spec.width as i32,
            spec.height as i32,
            spec.stride as i32,
            spec.format,
            CaptureBuffer,
            qh,
        );
        let Some(job) = self.job.as_mut() else {
            return;
        };
        match &job.frame {
            JobFrame::Export(f) => f.copy(&buffer, 1),
            JobFrame::Screencopy(f) => f.copy(&buffer),
        }
        job.buffer = Some(buffer);
    }

    fn on_ready(&mut self) {
        let Some(job) = self.job.as_ref() else {
            return;
        };
        let (Some(spec), Some(pool)) = (job.shm, self.pool.as_mut()) else {
            return;
        };
        let size = spec.stride as usize * spec.height as usize;
        let rgba = to_rgba(&pool.mmap()[..size], spec, job.y_invert);
        let rect = job.rect;
        let scale = if rect.w > 0 {
            spec.width as f32 / rect.w as f32
        } else {
            1.0
        };
        tracing::debug!(
            w = spec.width,
            h = spec.height,
            scale,
            ms = job.started.elapsed().as_secs_f32() * 1000.0,
            "frame captured"
        );
        self.finish(Ok(Frame {
            rgba,
            width: spec.width,
            height: spec.height,
            rect,
            scale,
        }));
    }
}

impl State {
    pub(crate) fn capture(&mut self, req: CaptureReq, reply: CaptureReply) {
        if let Some(job) = self.capturer.job.as_ref() {
            if job.reply.is_closed() {
                // The caller timed out; drop the stale capture.
                self.capturer.finish(Err(CaptureError::Timeout));
            } else {
                let _ = reply.send(Err(CaptureError::Unavailable("capture busy".into())));
                return;
            }
        }
        if self.capturer.pool.is_none() {
            let _ = reply.send(Err(CaptureError::Unavailable("no capture protocol".into())));
            return;
        }
        let qh = self.qh.clone();
        let (frame, rect) = match req {
            CaptureReq::Toplevel { handle, rect } => {
                let Some(m) = self.capturer.export.as_ref() else {
                    let _ = reply.send(Err(CaptureError::Unavailable(
                        "hyprland_toplevel_export_manager_v1 missing".into(),
                    )));
                    return;
                };
                (
                    JobFrame::Export(m.capture_toplevel(0, handle, &qh, ())),
                    rect,
                )
            }
            CaptureReq::Region {
                output,
                local,
                rect,
            } => {
                let (Some(m), Some(o)) =
                    (self.capturer.screencopy.as_ref(), self.find_output(&output))
                else {
                    let _ = reply.send(Err(CaptureError::Unavailable(format!(
                        "screencopy or output {output} missing"
                    ))));
                    return;
                };
                let f = m.capture_output_region(0, &o, local.x, local.y, local.w, local.h, &qh, ());
                (JobFrame::Screencopy(f), rect)
            }
        };
        self.capturer.job = Some(Job {
            reply,
            rect,
            frame,
            shm: None,
            buffer: None,
            y_invert: false,
            started: Instant::now(),
        });
    }
}

impl Dispatch<ex_manager::HyprlandToplevelExportManagerV1, ()> for State {
    fn event(
        _: &mut Self,
        _: &ex_manager::HyprlandToplevelExportManagerV1,
        _: ex_manager::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<sc_manager::ZwlrScreencopyManagerV1, ()> for State {
    fn event(
        _: &mut Self,
        _: &sc_manager::ZwlrScreencopyManagerV1,
        _: sc_manager::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_buffer::WlBuffer, CaptureBuffer> for State {
    fn event(
        _: &mut Self,
        _: &wl_buffer::WlBuffer,
        _: wl_buffer::Event,
        _: &CaptureBuffer,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ex_frame::HyprlandToplevelExportFrameV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &ex_frame::HyprlandToplevelExportFrameV1,
        event: ex_frame::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        let c = &mut state.capturer;
        match event {
            ex_frame::Event::Buffer {
                format,
                width,
                height,
                stride,
            } => c.on_buffer(format, width, height, stride),
            ex_frame::Event::BufferDone => c.on_buffer_done(qh),
            ex_frame::Event::Flags { flags } => {
                if let (Some(j), WEnum::Value(f)) = (c.job.as_mut(), flags) {
                    j.y_invert = f.contains(ex_frame::Flags::YInvert);
                }
            }
            ex_frame::Event::Ready { .. } => c.on_ready(),
            ex_frame::Event::Failed => c.finish(Err(CaptureError::Unavailable(
                "toplevel export failed".into(),
            ))),
            _ => {}
        }
    }
}

impl Dispatch<sc_frame::ZwlrScreencopyFrameV1, ()> for State {
    fn event(
        state: &mut Self,
        frame: &sc_frame::ZwlrScreencopyFrameV1,
        event: sc_frame::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        let c = &mut state.capturer;
        match event {
            sc_frame::Event::Buffer {
                format,
                width,
                height,
                stride,
            } => {
                c.on_buffer(format, width, height, stride);
                // Before v3 there is no `buffer_done`: copy right away.
                if frame.version() < 3 {
                    c.on_buffer_done(qh);
                }
            }
            sc_frame::Event::BufferDone => c.on_buffer_done(qh),
            sc_frame::Event::Flags { flags } => {
                if let (Some(j), WEnum::Value(f)) = (c.job.as_mut(), flags) {
                    j.y_invert = f.contains(sc_frame::Flags::YInvert);
                }
            }
            sc_frame::Event::Ready { .. } => c.on_ready(),
            sc_frame::Event::Failed => {
                c.finish(Err(CaptureError::Unavailable("screencopy failed".into())))
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn address_to_handle() {
        assert_eq!(toplevel_handle("0xd161e7b0"), Some(3_512_854_448));
        assert_eq!(toplevel_handle("0x55d1d161e7b0"), Some(3_512_854_448));
        assert_eq!(toplevel_handle("nope"), None);
    }

    #[test]
    fn swizzle_and_invert() {
        let spec = ShmSpec {
            format: wl_shm::Format::Xrgb8888,
            width: 1,
            height: 2,
            stride: 8,
        };
        // Row 0: B=1 G=2 R=3; row 1: B=4 G=5 R=6; 4 bytes padding per row.
        let src = [1, 2, 3, 0, 9, 9, 9, 9, 4, 5, 6, 0, 9, 9, 9, 9];
        assert_eq!(to_rgba(&src, spec, false), [3, 2, 1, 255, 6, 5, 4, 255]);
        assert_eq!(to_rgba(&src, spec, true), [6, 5, 4, 255, 3, 2, 1, 255]);
    }
}
