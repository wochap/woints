//! Headless badge renderer: rounded rects + one text run per badge (design §01).

use std::collections::HashMap;
use std::path::Path;

use fontdue::{Font, FontSettings, Metrics};
use tiny_skia::{FillRule, Paint, PathBuilder, Pixmap, Stroke, Transform};

use woints_core::geom::{
    Device, OutputGeom, OutputLocal, Rect, Size, global_to_output, output_to_device,
};
use woints_core::place::chip_rect;
use woints_core::ports::{Chip, Scene};
use woints_core::theme::{Color, HintLayout, font_device_px, size_factor};

/// Atkinson Hyperlegible Mono Bold, SIL OFL 1.1 (see `assets/OFL.txt`).
pub static FONT_BYTES: &[u8] = include_bytes!("../assets/AtkinsonHyperlegibleMono-Bold.ttf");

/// Per-output facts the renderer needs besides the logical geometry.
#[derive(Debug, Clone, PartialEq)]
pub struct OutputSurface {
    pub geom: OutputGeom,
    /// Physical width from `wl_output.geometry`, 0 when unknown.
    pub phys_width_mm: i32,
}

pub struct Renderer {
    font: Font,
    glyphs: HashMap<(char, u32), (Metrics, Vec<u8>)>,
}

impl Default for Renderer {
    fn default() -> Self {
        Self::new()
    }
}

impl Renderer {
    pub fn new() -> Self {
        let font = Font::from_bytes(FONT_BYTES, FontSettings::default())
            .expect("embedded font must parse");
        Self {
            font,
            glyphs: HashMap::new(),
        }
    }

    fn glyph(&mut self, c: char, px: f32) -> &(Metrics, Vec<u8>) {
        let key = (c, px.to_bits());
        let font = &self.font;
        self.glyphs
            .entry(key)
            .or_insert_with(|| font.rasterize(c, px))
    }

    /// Render every badge on `out` (plus the chip if it targets `out`) into a
    /// transparent pixmap of the output's device size.
    pub fn render(&mut self, scene: &Scene, out: &OutputSurface) -> Pixmap {
        let g = &out.geom;
        let (w, h) = g.device_size();
        let mut pm = Pixmap::new(w, h).expect("non-zero output size");
        let hint = &scene.theme.hint;
        let tokens = &scene.theme.tokens;

        let font_px = font_device_px(hint, g.scale, out.phys_width_mm, w as i32);
        let factor = size_factor(hint, g.scale, font_px);
        let border = (g.scale.floor() as i32).max(1);

        let mut local_badges = Vec::new();
        for b in scene.badges.iter().filter(|b| b.output == g.id) {
            let local = global_to_output(b.rect, g);
            local_badges.push(local);
            let mut dev = output_to_device(local, g.scale);
            if (factor - 1.0).abs() > f32::EPSILON {
                dev.w = (dev.w as f32 * factor).round() as i32;
                dev.h = (dev.h as f32 * factor).round() as i32;
            }
            let label = if hint.uppercase {
                b.label.to_uppercase()
            } else {
                b.label.clone()
            };
            let radius = hint.radius * g.scale * factor;
            self.draw_box(&mut pm, dev, radius, border, tokens.bg, tokens.border);
            self.draw_text(&mut pm, dev, &label, font_px, tokens.fg);
        }

        if let Some(chip) = scene.chip.as_ref().filter(|c| c.output == g.id) {
            self.draw_chip(&mut pm, scene, chip, g, &local_badges, font_px, border);
        }
        pm
    }

    fn draw_box(
        &self,
        pm: &mut Pixmap,
        r: Rect<Device>,
        radius: f32,
        border: i32,
        bg: Color,
        bd: Color,
    ) {
        let (x, y, w, h) = (r.x as f32, r.y as f32, r.w as f32, r.h as f32);
        if let Some(path) = rounded_rect(x, y, w, h, radius) {
            pm.fill_path(
                &path,
                &paint(bg),
                FillRule::Winding,
                Transform::identity(),
                None,
            );
        }
        let half = border as f32 / 2.0;
        if let Some(path) = rounded_rect(
            x + half,
            y + half,
            w - 2.0 * half,
            h - 2.0 * half,
            (radius - half).max(0.0),
        ) {
            let stroke = Stroke {
                width: border as f32,
                ..Stroke::default()
            };
            pm.stroke_path(&path, &paint(bd), &stroke, Transform::identity(), None);
        }
    }

    fn text_width(&mut self, text: &str, px: f32) -> f32 {
        text.chars()
            .map(|c| self.glyph(c, px).0.advance_width)
            .sum()
    }

    /// One text run centred in `r`.
    fn draw_text(&mut self, pm: &mut Pixmap, r: Rect<Device>, text: &str, px: f32, color: Color) {
        let lm = self.font.horizontal_line_metrics(px);
        let (ascent, descent) = lm
            .map(|m| (m.ascent, m.descent))
            .unwrap_or((px * 0.8, -px * 0.2));
        let width = self.text_width(text, px);
        let mut pen_x = r.x as f32 + (r.w as f32 - width) / 2.0;
        let baseline = r.y as f32 + (r.h as f32 - (ascent - descent)) / 2.0 + ascent;
        for c in text.chars() {
            let (m, bitmap) = self.glyph(c, px).clone();
            let gx = (pen_x + m.xmin as f32).round() as i32;
            let gy = (baseline - m.height as f32 - m.ymin as f32).round() as i32;
            blit_coverage(pm, gx, gy, m.width, m.height, &bitmap, color);
            pen_x += m.advance_width;
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_chip(
        &mut self,
        pm: &mut Pixmap,
        scene: &Scene,
        chip: &Chip,
        g: &OutputGeom,
        badges: &[Rect<OutputLocal>],
        font_px: f32,
        border: i32,
    ) {
        let hint: &HintLayout = &scene.theme.hint;
        let tokens = &scene.theme.tokens;
        let layout = &scene.theme.chip;
        let segs: Vec<&str> = [chip.mode.as_str(), chip.typed.as_str(), chip.scope.as_str()]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect();
        let pad = 8;
        let seg_w: Vec<i32> = segs
            .iter()
            .map(|s| {
                (s.chars().count() as f32 * hint.advance_em * hint.size).ceil() as i32 + 2 * pad
            })
            .collect();
        let total: i32 = seg_w.iter().sum::<i32>() + (segs.len() as i32 - 1).max(0);
        let size = Size {
            w: g.logical_size.w,
            h: g.logical_size.h,
        };
        let rect = chip_rect(total, size, layout, badges);
        let dev = output_to_device(rect, g.scale);
        self.draw_box(
            pm,
            dev,
            hint.radius * g.scale,
            border,
            tokens.bg,
            tokens.border,
        );

        let mut x = rect.x;
        for (i, (s, w)) in segs.iter().zip(&seg_w).enumerate() {
            let seg = output_to_device(Rect::new(x, rect.y, *w, rect.h), g.scale);
            let color = if i == 1 && segs.len() == 3 {
                tokens.accent
            } else {
                tokens.fg
            };
            self.draw_text(pm, seg, s, font_px, color);
            x += w;
            if i + 1 < segs.len() {
                let div = output_to_device(Rect::new(x, rect.y + 4, 1, rect.h - 8), g.scale);
                let div = Rect::<Device>::new(div.x, div.y, border, div.h);
                fill_rect(pm, div, tokens.border);
                x += 1;
            }
        }
    }
}

fn paint(c: Color) -> Paint<'static> {
    let mut p = Paint::default();
    p.set_color_rgba8(c.r, c.g, c.b, 255);
    p.anti_alias = true;
    p
}

fn fill_rect(pm: &mut Pixmap, r: Rect<Device>, c: Color) {
    if let Some(rect) = tiny_skia::Rect::from_xywh(r.x as f32, r.y as f32, r.w as f32, r.h as f32) {
        pm.fill_rect(rect, &paint(c), Transform::identity(), None);
    }
}

fn rounded_rect(x: f32, y: f32, w: f32, h: f32, r: f32) -> Option<tiny_skia::Path> {
    if w <= 0.0 || h <= 0.0 {
        return None;
    }
    let r = r.min(w / 2.0).min(h / 2.0).max(0.0);
    let k = r * 0.552_284_8;
    let (x1, y1) = (x + w, y + h);
    let mut pb = PathBuilder::new();
    pb.move_to(x + r, y);
    pb.line_to(x1 - r, y);
    pb.cubic_to(x1 - r + k, y, x1, y + r - k, x1, y + r);
    pb.line_to(x1, y1 - r);
    pb.cubic_to(x1, y1 - r + k, x1 - r + k, y1, x1 - r, y1);
    pb.line_to(x + r, y1);
    pb.cubic_to(x + r - k, y1, x, y1 - r + k, x, y1 - r);
    pb.line_to(x, y + r);
    pb.cubic_to(x, y + r - k, x + r - k, y, x + r, y);
    pb.close();
    pb.finish()
}

/// Source-over blend of an 8-bit coverage mask in an opaque colour.
fn blit_coverage(pm: &mut Pixmap, x0: i32, y0: i32, w: usize, h: usize, cov: &[u8], c: Color) {
    let (pw, ph) = (pm.width() as i32, pm.height() as i32);
    let data = pm.data_mut();
    for row in 0..h as i32 {
        let y = y0 + row;
        if y < 0 || y >= ph {
            continue;
        }
        for col in 0..w as i32 {
            let x = x0 + col;
            if x < 0 || x >= pw {
                continue;
            }
            let a = u32::from(cov[(row * w as i32 + col) as usize]);
            if a == 0 {
                continue;
            }
            let i = ((y * pw + x) * 4) as usize;
            let inv = 255 - a;
            for (ch, src) in [c.r, c.g, c.b, 255].into_iter().enumerate() {
                let d = u32::from(data[i + ch]);
                data[i + ch] = ((u32::from(src) * a + d * inv + 127) / 255) as u8;
            }
        }
    }
}

/// Copy a premultiplied RGBA pixmap into an `ARGB8888` (little-endian BGRA) buffer.
pub fn to_argb8888(pm: &Pixmap, dst: &mut [u8]) {
    for (s, d) in pm.data().chunks_exact(4).zip(dst.chunks_exact_mut(4)) {
        d[0] = s[2];
        d[1] = s[1];
        d[2] = s[0];
        d[3] = s[3];
    }
}

/// `WOINTS_DUMP=<dir>`: write `<dir>/<output>.png`.
pub fn dump_if_requested(pm: &Pixmap, output: &str) {
    let Some(dir) = std::env::var_os("WOINTS_DUMP") else {
        return;
    };
    let path = Path::new(&dir).join(format!("{output}.png"));
    match pm.save_png(&path) {
        Ok(()) => tracing::info!(path = %path.display(), "dumped overlay buffer"),
        Err(e) => tracing::warn!(path = %path.display(), "dump failed: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use woints_core::geom::{Global, OutputId, Point};
    use woints_core::place::badge_size;
    use woints_core::ports::Badge;
    use woints_core::theme::Theme;

    fn output(scale: f32) -> OutputSurface {
        OutputSurface {
            geom: OutputGeom {
                id: OutputId("T".into()),
                origin: Point::new(0, 0),
                logical_size: Size { w: 200, h: 100 },
                scale,
            },
            phys_width_mm: 0,
        }
    }

    /// Bounding box of non-transparent pixels.
    fn bbox(pm: &Pixmap) -> (u32, u32, u32, u32) {
        let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0, 0);
        for y in 0..pm.height() {
            for x in 0..pm.width() {
                if pm.pixel(x, y).unwrap().alpha() > 0 {
                    x0 = x0.min(x);
                    y0 = y0.min(y);
                    x1 = x1.max(x + 1);
                    y1 = y1.max(y + 1);
                }
            }
        }
        (x0, y0, x1 - x0, y1 - y0)
    }

    fn badge_pixels(scale: f32) -> (u32, u32) {
        let theme = Theme::default();
        let s = badge_size(2, &theme.hint);
        let scene = Scene {
            outputs: vec![],
            badges: vec![Badge {
                label: "as".into(),
                rect: Rect::<Global>::new(10, 10, s.w, s.h),
                output: OutputId("T".into()),
            }],
            chip: None,
            theme,
        };
        let pm = Renderer::new().render(&scene, &output(scale));
        let (_, _, w, h) = bbox(&pm);
        (w, h)
    }

    #[test]
    fn two_glyph_badge_device_sizes() {
        assert_eq!(badge_pixels(1.0), (22, 17));
        assert_eq!(badge_pixels(1.5), (33, 26));
        assert_eq!(badge_pixels(2.0), (44, 34));
    }

    #[test]
    fn buffer_is_device_size() {
        let mut o = output(2.0);
        o.geom.logical_size = Size { w: 1440, h: 900 };
        let scene = Scene {
            outputs: vec![],
            badges: vec![],
            chip: None,
            theme: Theme::default(),
        };
        let pm = Renderer::new().render(&scene, &o);
        assert_eq!((pm.width(), pm.height()), (2880, 1800));
    }

    #[test]
    fn text_is_drawn_in_fg() {
        let theme = Theme::default();
        let fg = theme.tokens.fg;
        let scene = Scene {
            outputs: vec![],
            badges: vec![Badge {
                label: "as".into(),
                rect: Rect::new(10, 10, 22, 17),
                output: OutputId("T".into()),
            }],
            chip: None,
            theme,
        };
        let pm = Renderer::new().render(&scene, &output(2.0));
        let hit = pm
            .pixels()
            .iter()
            .any(|p| (p.red(), p.green(), p.blue()) == (fg.r, fg.g, fg.b));
        assert!(hit, "no fully covered text pixel");
    }

    #[test]
    fn chip_renders_bottom_right() {
        let scene = Scene {
            outputs: vec![],
            badges: vec![],
            chip: Some(Chip {
                mode: "click".into(),
                typed: "s".into(),
                scope: woints_core::Scope::Focused,
                output: OutputId("T".into()),
            }),
            theme: Theme::default(),
        };
        let pm = Renderer::new().render(&scene, &output(1.0));
        let (x, y, w, h) = bbox(&pm);
        assert_eq!(x + w, 200 - 12);
        assert_eq!((y, h), (100 - 12 - 22, 22));
    }

    #[test]
    fn argb_swizzle() {
        let mut pm = Pixmap::new(1, 1).unwrap();
        pm.fill(tiny_skia::Color::from_rgba8(1, 2, 3, 255));
        let mut out = [0u8; 4];
        to_argb8888(&pm, &mut out);
        assert_eq!(out, [3, 2, 1, 255]);
    }
}
