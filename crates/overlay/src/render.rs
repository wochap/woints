//! Headless badge renderer: rounded rects + one text run per badge (design §01).

use std::collections::HashMap;
use std::path::Path;

use fontdue::{Font, FontSettings, Metrics};
use tiny_skia::{FillRule, Paint, PathBuilder, Pixmap, Stroke, Transform};

use woints_core::geom::{
    Device, OutputGeom, OutputLocal, Rect, Size, global_to_output, output_to_device,
};
use woints_core::place::chip_rect;
use woints_core::ports::{BadgeStyle, Chip, Scene, ScrollValue};
use woints_core::theme::{Color, HintLayout, PrefixStyle, font_device_px, size_factor};

/// Opacity of eliminated badges with `hint.eliminated = "fade"`.
pub const FADED_ALPHA: f32 = 0.2;

/// Scroll mode stroke width and inset, logical px (design §06).
pub const SCROLL_STROKE: i32 = 2;
pub const SCROLL_INSET: i32 = 1;
/// Scroll rail width, logical px.
pub const RAIL_W: i32 = 3;
/// Corner radius of the dashed vision target outline, logical px.
pub const VISION_OUTLINE_RADIUS: f32 = 2.0;
/// Drag swatch side in the chip, logical px.
pub const SWATCH: i32 = 8;

/// Rect the scroll stroke occupies: the container inset by 1 px.
pub fn scroll_stroke_rect<S>(r: Rect<S>) -> Rect<S> {
    r.inflate(-SCROLL_INSET)
}

/// Rail inside the stroke at the container's right edge.
pub fn rail_rect<S>(r: Rect<S>) -> Rect<S> {
    let s = scroll_stroke_rect(r);
    let pad = SCROLL_STROKE + 1;
    Rect::new(
        s.right() - pad - RAIL_W,
        s.y + pad,
        RAIL_W,
        (s.h - 2 * pad).max(0),
    )
}

/// Thumb `(offset, length)` along a rail of `rail` px for a viewport of
/// `viewport` px: length is `viewport / content`, content being the viewport
/// plus the value range.
pub fn rail_thumb(rail: i32, viewport: i32, v: ScrollValue) -> (i32, i32) {
    let range = (v.max - v.min).max(0.0);
    let content = f64::from(viewport.max(1)) + range;
    let len = ((f64::from(rail) * f64::from(viewport.max(1)) / content).round() as i32)
        .clamp(RAIL_W.min(rail), rail);
    let frac = if range > 0.0 {
        ((v.current - v.min) / range).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let off = (f64::from(rail - len) * frac).round() as i32;
    (off, len)
}

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
        if let Some(f) = scene.scroll.as_ref().filter(|f| f.output == g.id) {
            let local = global_to_output(f.rect, g);
            let stroke = output_to_device(scroll_stroke_rect(local), g.scale);
            let w = (SCROLL_STROKE as f32 * g.scale).round().max(1.0) as i32;
            ring(&mut pm, stroke, w, tokens.scroll);
            if let Some(v) = f.value {
                let rail = rail_rect(local);
                let (off, len) = rail_thumb(rail.h, local.h, v);
                fill_rect_a(&mut pm, output_to_device(rail, g.scale), tokens.scroll, 0.3);
                let thumb = Rect::new(rail.x, rail.y + off, rail.w, len);
                fill_rect(&mut pm, output_to_device(thumb, g.scale), tokens.scroll);
            }
        }
        if let Some(o) = scene.pinned.as_ref().filter(|o| o.output == g.id) {
            let dev = output_to_device(global_to_output(o.rect, g), g.scale);
            ring(&mut pm, dev, border, tokens.drag_bg);
        }
        let badges = scene
            .badges
            .iter()
            .filter(|b| b.output == g.id && scene.scroll.is_none());
        for b in badges {
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
            let (bg, bd, alpha) = match b.style {
                BadgeStyle::Normal => (tokens.bg, tokens.border, 1.0),
                BadgeStyle::Flash => (tokens.flash_bg, tokens.flash_bg, 1.0),
                BadgeStyle::Faded => (tokens.bg, tokens.border, FADED_ALPHA),
                BadgeStyle::Drag => (tokens.drag_bg, tokens.drag_bg, 1.0),
            };
            let dash = vision_dash(hint, g.scale);
            match (b.vision, b.style) {
                (Some(target), BadgeStyle::Normal | BadgeStyle::Faded) => {
                    let t = output_to_device(global_to_output(target, g), g.scale);
                    stroke_dashed(
                        &mut pm,
                        t,
                        VISION_OUTLINE_RADIUS * g.scale,
                        border,
                        tokens.vision,
                        alpha,
                        dash,
                    );
                    self.draw_box(&mut pm, dev, radius, 0, bg, bd, alpha);
                    stroke_dashed(&mut pm, dev, radius, border, tokens.vision, alpha, dash);
                }
                _ => self.draw_box(&mut pm, dev, radius, border, bg, bd, alpha),
            }
            let colors: Vec<Color> = (0..label.chars().count())
                .map(|i| match (b.style, b.matched, hint.prefix_style) {
                    (BadgeStyle::Flash, _, _) => tokens.flash_fg,
                    (BadgeStyle::Drag, _, _) => tokens.drag_fg,
                    (_, 0, _) => tokens.fg,
                    (_, m, PrefixStyle::HighlightMatched) if i < m => tokens.accent,
                    (_, _, PrefixStyle::HighlightMatched) => tokens.fg_dim,
                    (_, m, PrefixStyle::DimMatched) if i < m => tokens.fg_dim,
                    (_, _, PrefixStyle::DimMatched) => tokens.fg,
                })
                .collect();
            self.draw_text(&mut pm, dev, &label, font_px, &colors, alpha);
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
        alpha: f32,
    ) {
        let (x, y, w, h) = (r.x as f32, r.y as f32, r.w as f32, r.h as f32);
        if let Some(path) = rounded_rect(x, y, w, h, radius) {
            pm.fill_path(
                &path,
                &paint_a(bg, alpha),
                FillRule::Winding,
                Transform::identity(),
                None,
            );
        }
        if border <= 0 {
            return;
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
            pm.stroke_path(
                &path,
                &paint_a(bd, alpha),
                &stroke,
                Transform::identity(),
                None,
            );
        }
    }

    fn text_width(&mut self, text: &str, px: f32) -> f32 {
        text.chars()
            .map(|c| self.glyph(c, px).0.advance_width)
            .sum()
    }

    /// One text run centred in `r`; glyph `i` uses `colors[i]` (last colour repeats).
    fn draw_text(
        &mut self,
        pm: &mut Pixmap,
        r: Rect<Device>,
        text: &str,
        px: f32,
        colors: &[Color],
        alpha: f32,
    ) {
        let lm = self.font.horizontal_line_metrics(px);
        let (ascent, descent) = lm
            .map(|m| (m.ascent, m.descent))
            .unwrap_or((px * 0.8, -px * 0.2));
        let width = self.text_width(text, px);
        let mut pen_x = r.x as f32 + (r.w as f32 - width) / 2.0;
        let baseline = r.y as f32 + (r.h as f32 - (ascent - descent)) / 2.0 + ascent;
        for (i, c) in text.chars().enumerate() {
            let color = colors
                .get(i)
                .or(colors.last())
                .copied()
                .unwrap_or(Color::hex(0xffffff));
            let (m, bitmap) = self.glyph(c, px).clone();
            let gx = (pen_x + m.xmin as f32).round() as i32;
            let gy = (baseline - m.height as f32 - m.ymin as f32).round() as i32;
            blit_coverage(pm, gx, gy, m.width, m.height, &bitmap, color, alpha);
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
        let typed_seg = !chip.typed.is_empty();
        let mut segs: Vec<(String, Color)> = Vec::new();
        let mode = match &chip.sub {
            Some(sub) => format!("{} · {sub}", chip.mode),
            None => chip.mode.clone(),
        };
        segs.push((mode, tokens.fg));
        if typed_seg {
            segs.push((chip.typed.clone(), tokens.accent));
        }
        segs.push((chip.scope.as_str().to_owned(), tokens.fg));
        segs.retain(|(s, _)| !s.is_empty());
        let pad = 8;
        let swatch = if chip.swatch { SWATCH + pad / 2 } else { 0 };
        let seg_w: Vec<i32> = segs
            .iter()
            .enumerate()
            .map(|(i, (s, _))| {
                let w = (s.chars().count() as f32 * hint.advance_em * hint.size).ceil() as i32
                    + 2 * pad;
                if i == 0 { w + swatch } else { w }
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
            1.0,
        );

        let mut x = rect.x;
        for (i, ((s, color), w)) in segs.iter().zip(&seg_w).enumerate() {
            let mut text_x = x;
            let mut text_w = *w;
            if i == 0 && chip.swatch {
                let sq = Rect::new(x + pad, rect.y + (rect.h - SWATCH) / 2, SWATCH, SWATCH);
                fill_rect(pm, output_to_device(sq, g.scale), tokens.drag_bg);
                text_x += swatch;
                text_w -= swatch;
            }
            let seg = output_to_device(Rect::new(text_x, rect.y, text_w, rect.h), g.scale);
            if let Some((mode, _)) = s.split_once(" · ").filter(|_| i == 0) {
                // Mode in fg, sub-text dimmed.
                let n = mode.chars().count();
                let colors: Vec<Color> = s
                    .chars()
                    .enumerate()
                    .map(|(k, _)| if k < n { *color } else { tokens.fg_dim })
                    .collect();
                self.draw_text(pm, seg, s, font_px, &colors, 1.0);
            } else {
                self.draw_text(pm, seg, s, font_px, &[*color], 1.0);
            }
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

fn paint_a(c: Color, alpha: f32) -> Paint<'static> {
    let mut p = Paint::default();
    p.set_color_rgba8(c.r, c.g, c.b, (alpha.clamp(0.0, 1.0) * 255.0).round() as u8);
    p.anti_alias = true;
    p
}

/// `hint.vision_dash` in device px for `scale`.
pub fn vision_dash(hint: &HintLayout, scale: f32) -> [f32; 2] {
    hint.vision_dash.map(|d| (d * scale).max(1.0))
}

/// Dashed stroke of `width` device px inside `r`.
fn stroke_dashed(
    pm: &mut Pixmap,
    r: Rect<Device>,
    radius: f32,
    width: i32,
    c: Color,
    alpha: f32,
    dash: [f32; 2],
) {
    let half = width as f32 / 2.0;
    let Some(path) = rounded_rect(
        r.x as f32 + half,
        r.y as f32 + half,
        r.w as f32 - 2.0 * half,
        r.h as f32 - 2.0 * half,
        (radius - half).max(0.0),
    ) else {
        return;
    };
    let stroke = Stroke {
        width: width as f32,
        dash: tiny_skia::StrokeDash::new(dash.to_vec(), 0.0),
        ..Stroke::default()
    };
    pm.stroke_path(
        &path,
        &paint_a(c, alpha),
        &stroke,
        Transform::identity(),
        None,
    );
}

fn fill_rect(pm: &mut Pixmap, r: Rect<Device>, c: Color) {
    fill_rect_a(pm, r, c, 1.0);
}

fn fill_rect_a(pm: &mut Pixmap, r: Rect<Device>, c: Color, alpha: f32) {
    if let Some(rect) = tiny_skia::Rect::from_xywh(r.x as f32, r.y as f32, r.w as f32, r.h as f32) {
        let mut p = paint_a(c, alpha);
        p.anti_alias = false;
        pm.fill_rect(rect, &p, Transform::identity(), None);
    }
}

/// A `w` px band just inside `r`.
fn ring(pm: &mut Pixmap, r: Rect<Device>, w: i32, c: Color) {
    let w = w.min(r.w / 2).min(r.h / 2).max(1);
    fill_rect(pm, Rect::new(r.x, r.y, r.w, w), c);
    fill_rect(pm, Rect::new(r.x, r.bottom() - w, r.w, w), c);
    fill_rect(pm, Rect::new(r.x, r.y + w, w, r.h - 2 * w), c);
    fill_rect(pm, Rect::new(r.right() - w, r.y + w, w, r.h - 2 * w), c);
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

/// Source-over blend of an 8-bit coverage mask in colour `c` at `alpha`.
#[allow(clippy::too_many_arguments)]
fn blit_coverage(
    pm: &mut Pixmap,
    x0: i32,
    y0: i32,
    w: usize,
    h: usize,
    cov: &[u8],
    c: Color,
    alpha: f32,
) {
    let alpha = (alpha.clamp(0.0, 1.0) * 255.0).round() as u32;
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
            let a = (u32::from(cov[(row * w as i32 + col) as usize]) * alpha + 127) / 255;
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
    use woints_core::ports::{Badge, BadgeStyle};
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

    /// Count pixels whose RGB equals `c` after rendering one badge.
    fn count_color(style: BadgeStyle, matched: usize, c: Color) -> usize {
        let theme = Theme::default();
        let s = badge_size(2, &theme.hint);
        let scene = Scene {
            outputs: vec![],
            badges: vec![Badge {
                label: "as".into(),
                rect: Rect::<Global>::new(10, 10, s.w * 3, s.h * 3),
                output: OutputId("T".into()),
                matched,
                style,
                vision: None,
            }],
            chip: None,
            pinned: None,
            scroll: None,
            theme,
        };
        let pm = Renderer::new().render(&scene, &output(1.0));
        pm.data()
            .chunks_exact(4)
            .filter(|p| p[0] == c.r && p[1] == c.g && p[2] == c.b && p[3] == 255)
            .count()
    }

    #[test]
    fn filter_styles() {
        let t = Theme::default().tokens;
        assert_eq!(count_color(BadgeStyle::Normal, 0, t.accent), 0);
        assert!(count_color(BadgeStyle::Normal, 1, t.accent) > 0);
        assert!(count_color(BadgeStyle::Flash, 0, t.flash_bg) > 0);
        // Faded badges are translucent: no fully opaque background pixel.
        assert_eq!(count_color(BadgeStyle::Faded, 0, t.bg), 0);
        assert!(count_color(BadgeStyle::Normal, 0, t.bg) > 0);
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
                matched: 0,
                style: BadgeStyle::Normal,
                vision: None,
            }],
            chip: None,
            pinned: None,
            scroll: None,
            theme,
        };
        let pm = Renderer::new().render(&scene, &output(scale));
        let (_, _, w, h) = bbox(&pm);
        (w, h)
    }

    /// Vision badge at `scale`: (vision-coloured pixels, border-coloured pixels).
    fn vision_render(scale: f32) -> (usize, usize) {
        let theme = Theme::default();
        let t = theme.tokens.clone();
        let s = badge_size(2, &theme.hint);
        let badge = |label: &str, x: i32, vision| Badge {
            label: label.into(),
            rect: Rect::<Global>::new(x, 10, s.w * 2, s.h * 2),
            output: OutputId("T".into()),
            matched: 0,
            style: BadgeStyle::Normal,
            vision,
        };
        let scene = Scene {
            outputs: vec![],
            badges: vec![
                badge("as", 10, None),
                badge("df", 120, Some(Rect::<Global>::new(110, 5, 80, 60))),
            ],
            chip: None,
            pinned: None,
            scroll: None,
            theme,
        };
        let pm = Renderer::new().render(&scene, &output(scale));
        let count = |c: Color, x0: u32, x1: u32| {
            let mut n = 0;
            for y in 0..pm.height() {
                for x in x0..x1.min(pm.width()) {
                    let p = pm.pixel(x, y).unwrap().demultiply();
                    if (p.red(), p.green(), p.blue(), p.alpha()) == (c.r, c.g, c.b, 255) {
                        n += 1;
                    }
                }
            }
            n
        };
        let split = (100.0 * scale) as u32;
        let vision = count(t.vision, split, u32::MAX);
        let solid = count(t.border, 0, split);
        assert_eq!(count(t.vision, 0, split), 0, "AT-SPI badge stays solid");
        assert_eq!(
            count(t.border, split, u32::MAX),
            0,
            "vision badge has no solid border"
        );
        (vision, solid)
    }

    #[test]
    fn vision_badges_dashed() {
        let (v1, b1) = vision_render(1.0);
        let (v2, b2) = vision_render(2.0);
        assert!(v1 > 0 && b1 > 0);
        assert!(v2 > v1 && b2 > b1);
        assert_eq!(vision_dash(&Theme::default().hint, 2.0), [4.0, 4.0]);
        assert_eq!(vision_dash(&Theme::default().hint, 1.0), [2.0, 2.0]);
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
            pinned: None,
            scroll: None,
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
                matched: 0,
                style: BadgeStyle::Normal,
                vision: None,
            }],
            chip: None,
            pinned: None,
            scroll: None,
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
                sub: None,
                swatch: false,
                typed: "s".into(),
                scope: woints_core::Scope::Focused,
                output: OutputId("T".into()),
            }),
            pinned: None,
            scroll: None,
            theme: Theme::default(),
        };
        let pm = Renderer::new().render(&scene, &output(1.0));
        let (x, y, w, h) = bbox(&pm);
        assert_eq!(x + w, 200 - 12);
        assert_eq!((y, h), (100 - 12 - 22, 22));
    }

    fn count_rgb(pm: &Pixmap, c: Color) -> usize {
        pm.data()
            .chunks_exact(4)
            .filter(|p| p[0] == c.r && p[1] == c.g && p[2] == c.b && p[3] == 255)
            .count()
    }

    fn big() -> OutputSurface {
        let mut o = output(1.0);
        o.geom.logical_size = Size { w: 600, h: 500 };
        o
    }

    #[test]
    fn drag_pinned_source() {
        let theme = Theme::default();
        let t = theme.tokens.clone();
        let badge = |label: &str, x: i32, style| Badge {
            label: label.into(),
            rect: Rect::new(x, 10, 22, 17),
            output: OutputId("T".into()),
            matched: 0,
            style,
            vision: None,
        };
        let source = Rect::new(100, 100, 50, 40);
        let scene = Scene {
            outputs: vec![],
            badges: vec![
                badge("as", 10, BadgeStyle::Drag),
                badge("d", 60, BadgeStyle::Normal),
            ],
            chip: None,
            pinned: Some(woints_core::ports::Outline {
                rect: source,
                output: OutputId("T".into()),
            }),
            scroll: None,
            theme,
        };
        let pm = Renderer::new().render(&scene, &big());
        // Outline: 1 px ring around the source rect.
        let px = |x, y| {
            let p = pm.pixel(x, y).unwrap();
            (p.red(), p.green(), p.blue(), p.alpha())
        };
        let drag = (t.drag_bg.r, t.drag_bg.g, t.drag_bg.b, 255);
        assert_eq!(px(100, 100), drag);
        assert_eq!(px(149, 139), drag);
        assert_eq!(px(101, 101).3, 0);
        assert_eq!(px(99, 99).3, 0);
        // Exactly one badge uses drag_bg: the badge area x < 40.
        let in_badges = (10..40)
            .flat_map(|x| (10..27).map(move |y| (x, y)))
            .filter(|&(x, y)| px(x, y) == drag)
            .count();
        assert!(in_badges > 0);
        let other = (60..90)
            .flat_map(|x| (10..27).map(move |y| (x, y)))
            .filter(|&(x, y)| px(x, y) == drag)
            .count();
        assert_eq!(other, 0);
    }

    #[test]
    fn scroll_stroke_hides_badges() {
        let theme = Theme::default();
        let t = theme.tokens.clone();
        let rect = Rect::<Global>::new(100, 100, 400, 300);
        assert_eq!(scroll_stroke_rect(rect), Rect::new(101, 101, 398, 298));
        let scene = |value| Scene {
            outputs: vec![],
            badges: vec![Badge {
                label: "as".into(),
                rect: Rect::new(10, 10, 22, 17),
                output: OutputId("T".into()),
                matched: 0,
                style: BadgeStyle::Normal,
                vision: None,
            }],
            chip: None,
            pinned: None,
            scroll: Some(woints_core::ports::ScrollFrame {
                rect,
                output: OutputId("T".into()),
                value,
            }),
            theme: theme.clone(),
        };
        let pm = Renderer::new().render(&scene(None), &big());
        assert_eq!(bbox(&pm), (101, 101, 398, 298));
        // 2 px stroke: (101,101) and (102,102) set, (103,103) clear.
        assert_eq!(pm.pixel(102, 102).unwrap().alpha(), 255);
        assert_eq!(pm.pixel(103, 103).unwrap().alpha(), 0);
        assert_eq!(count_rgb(&pm, t.bg), 0);
        // With a value: rail pixels inside the stroke.
        let v = ScrollValue {
            current: 0.0,
            min: 0.0,
            max: 300.0,
        };
        let pm = Renderer::new().render(&scene(Some(v)), &big());
        let rail = rail_rect(rect);
        assert_eq!(pm.pixel(rail.x as u32, rail.y as u32).unwrap().alpha(), 255);
    }

    #[test]
    fn thumb_geometry() {
        let v = |current| ScrollValue {
            current,
            min: 0.0,
            max: 300.0,
        };
        assert_eq!(rail_thumb(100, 300, v(0.0)), (0, 50));
        assert_eq!(rail_thumb(100, 300, v(300.0)), (50, 50));
        assert_eq!(rail_thumb(100, 300, v(150.0)), (25, 50));
    }

    #[test]
    fn drag_chip_has_swatch() {
        let theme = Theme::default();
        let t = theme.tokens.clone();
        let chip = |swatch| Scene {
            outputs: vec![],
            badges: vec![],
            chip: Some(Chip {
                mode: "drag 2/2".into(),
                sub: Some("pick destination".into()),
                swatch,
                typed: String::new(),
                scope: woints_core::Scope::Focused,
                output: OutputId("T".into()),
            }),
            pinned: None,
            scroll: None,
            theme: theme.clone(),
        };
        let with = Renderer::new().render(&chip(true), &big());
        let without = Renderer::new().render(&chip(false), &big());
        assert_eq!(count_rgb(&with, t.drag_bg), (SWATCH * SWATCH) as usize);
        assert_eq!(count_rgb(&without, t.drag_bg), 0);
        assert!(count_rgb(&with, t.fg_dim) > 0);
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
