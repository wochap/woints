//! Badge geometry and placement (design §01, §04, §05, §09).

use crate::geom::{OutputLocal, Rect, Size};
use crate::theme::{ChipLayout, HintLayout};

/// Logical badge size for a label of `glyphs` characters:
/// `w = ceil(n · advance · size) + 2·pad_x + 2·border`, `h = size + 2·pad_y + 2·border`.
pub fn badge_size(glyphs: usize, hint: &HintLayout) -> Size {
    let size = hint.size.max(hint.min_size);
    let text_w = (glyphs as f32 * hint.advance_em * size).ceil() as i32;
    let b = hint.border_width;
    Size {
        w: text_w + 2 * hint.padding[1] + 2 * b,
        h: size.round() as i32 + 2 * hint.padding[0] + 2 * b,
    }
}

/// One badge to place: its target and its size, both output-local logical.
#[derive(Debug, Clone, Copy)]
pub struct PlaceInput {
    pub target: Rect<OutputLocal>,
    pub size: Size,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Placed {
    pub rect: Rect<OutputLocal>,
    /// Rule 4: no free slot was found; drawn over its neighbour.
    pub collided: bool,
}

/// Inset from the output edge used when clamping.
pub const EDGE_INSET: i32 = 1;
/// Rule 3: maximum "right of blocker" hops.
pub const MAX_HOPS: usize = 3;

fn clamp(r: Rect<OutputLocal>, out: Size) -> Rect<OutputLocal> {
    let max_x = (out.w - r.w - EDGE_INSET).max(EDGE_INSET);
    let max_y = (out.h - r.h - EDGE_INSET).max(EDGE_INSET);
    Rect::new(
        r.x.clamp(EDGE_INSET, max_x),
        r.y.clamp(EDGE_INSET, max_y),
        r.w,
        r.h,
    )
}

/// Rule 1: preferred slot, top-left + offset; tiny targets are centred.
pub fn preferred_slot(input: &PlaceInput, hint: &HintLayout) -> Rect<OutputLocal> {
    let t = input.target;
    let s = input.size;
    if t.w < hint.center_below || t.h < hint.center_below {
        let c = t.center();
        Rect::new(c.x - s.w / 2, c.y - s.h / 2, s.w, s.h)
    } else {
        Rect::new(t.x + hint.offset[1], t.y + hint.offset[0], s.w, s.h)
    }
}

/// Place badges on one output following rules 1–4.
///
/// Badges are processed in reading order (target y, then x); the returned
/// vector is indexed like `inputs`.
pub fn place(inputs: &[PlaceInput], output: Size, hint: &HintLayout) -> Vec<Placed> {
    let mut order: Vec<usize> = (0..inputs.len()).collect();
    order.sort_by_key(|&i| (inputs[i].target.y, inputs[i].target.x));

    let gap = hint.gap;
    let mut placed: Vec<Rect<OutputLocal>> = Vec::with_capacity(inputs.len());
    let mut out = vec![
        Placed {
            rect: Rect::new(0, 0, 0, 0),
            collided: false
        };
        inputs.len()
    ];

    let blocker = |r: &Rect<OutputLocal>, placed: &[Rect<OutputLocal>]| {
        let grown = r.inflate(gap);
        placed.iter().copied().find(|p| grown.intersects(p))
    };

    for i in order {
        let input = &inputs[i];
        let t = input.target;
        let pref = clamp(preferred_slot(input, hint), output);
        let mut chosen = None;

        if blocker(&pref, &placed).is_none() {
            chosen = Some(pref);
        }
        // Below: top = target bottom + 1 (and clear of the preferred slot,
        // which covers the target when it is centred).
        if chosen.is_none() {
            let top = t.bottom().max(pref.bottom() + gap) + 1;
            let below = clamp(Rect::new(pref.x, top, pref.w, pref.h), output);
            if blocker(&below, &placed).is_none() {
                chosen = Some(below);
            }
        }
        // Right of the blocker, up to MAX_HOPS times.
        if chosen.is_none() {
            let mut cand = pref;
            for _ in 0..MAX_HOPS {
                let Some(b) = blocker(&cand, &placed) else {
                    break;
                };
                cand = clamp(Rect::new(b.right() + gap, cand.y, cand.w, cand.h), output);
                if blocker(&cand, &placed).is_none() {
                    chosen = Some(cand);
                    break;
                }
            }
        }
        // Above: bottom = target top - 1 (clear of the preferred slot).
        if chosen.is_none() {
            let bottom = t.y.min(pref.y - gap) - 1;
            let above = clamp(Rect::new(pref.x, bottom - pref.h, pref.w, pref.h), output);
            if blocker(&above, &placed).is_none() {
                chosen = Some(above);
            }
        }

        let (rect, collided) = match chosen {
            Some(r) => (r, false),
            None => {
                tracing::debug!(?t, "badge collision unresolved; keeping preferred slot");
                (pref, true)
            }
        };
        placed.push(rect);
        out[i] = Placed { rect, collided };
    }
    out
}

/// Mode chip rect: bottom-right of the output with `inset`, moved to
/// bottom-left when any badge intersects it.
pub fn chip_rect(
    chip_w: i32,
    output: Size,
    chip: &ChipLayout,
    badges: &[Rect<OutputLocal>],
) -> Rect<OutputLocal> {
    let y = output.h - chip.inset - chip.height;
    let right = Rect::new(output.w - chip.inset - chip_w, y, chip_w, chip.height);
    let left = Rect::new(chip.inset, y, chip_w, chip.height);
    let preferred = match chip.corner {
        crate::theme::Corner::BottomRight => (right, left),
        crate::theme::Corner::BottomLeft => (left, right),
    };
    if badges.iter().any(|b| b.intersects(&preferred.0)) {
        preferred.1
    } else {
        preferred.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hint() -> HintLayout {
        HintLayout {
            advance_em: 0.6,
            ..HintLayout::default()
        }
    }

    #[test]
    fn badge_width_follows_advance() {
        let h = HintLayout {
            advance_em: 0.5,
            padding: [2, 3],
            ..hint()
        };
        assert_eq!(badge_size(2, &h), Size { w: 19, h: 17 });
    }

    const OUT: Size = Size { w: 1920, h: 1080 };

    #[test]
    fn two_glyph_badge_is_24x21() {
        assert_eq!(badge_size(2, &hint()), Size { w: 24, h: 21 });
        assert_eq!(badge_size(1, &hint()), Size { w: 17, h: 21 });
    }

    #[test]
    fn flush_top_left_clamps_to_1_1() {
        let s = badge_size(1, &hint());
        let p = place(
            &[PlaceInput {
                target: Rect::new(0, 0, 40, 40),
                size: s,
            }],
            OUT,
            &hint(),
        );
        assert_eq!((p[0].rect.x, p[0].rect.y), (1, 1));
    }

    #[test]
    fn flush_bottom_right_clamps() {
        let s = badge_size(2, &hint());
        let p = place(
            &[PlaceInput {
                target: Rect::new(1910, 1070, 10, 10),
                size: s,
            }],
            OUT,
            &hint(),
        );
        assert_eq!(p[0].rect.right(), 1919);
        assert_eq!(p[0].rect.bottom(), 1079);
    }

    #[test]
    fn large_target_uses_offset() {
        let s = badge_size(1, &hint());
        let p = place(
            &[PlaceInput {
                target: Rect::new(100, 200, 80, 30),
                size: s,
            }],
            OUT,
            &hint(),
        );
        assert_eq!((p[0].rect.x, p[0].rect.y), (97, 197));
    }

    #[test]
    fn tiny_target_is_centred() {
        let s = badge_size(1, &hint());
        let p = place(
            &[PlaceInput {
                target: Rect::new(100, 100, 16, 16),
                size: s,
            }],
            OUT,
            &hint(),
        );
        assert_eq!(
            p[0].rect.center(),
            Rect::<OutputLocal>::new(100, 100, 16, 16).center()
        );
    }

    #[test]
    fn dense_toolbar_alternates_below() {
        let s = badge_size(2, &hint());
        let inputs: Vec<_> = (0..12)
            .map(|i| PlaceInput {
                target: Rect::new(10 + i * 18, 100, 16, 16),
                size: s,
            })
            .collect();
        let p = place(&inputs, OUT, &hint());
        for (i, pl) in p.iter().enumerate() {
            assert!(!pl.collided, "badge {i} collided");
            let below = pl.rect.y > 100;
            assert_eq!(below, i % 2 == 1, "badge {i} at {:?}", pl.rect);
        }
        for a in 0..p.len() {
            for b in a + 1..p.len() {
                assert!(!p[a].rect.intersects(&p[b].rect), "{a} and {b} overlap");
            }
        }
    }

    #[test]
    fn spaced_toolbar_fits_as_is() {
        let s = badge_size(2, &hint());
        let inputs: Vec<_> = (0..12)
            .map(|i| PlaceInput {
                target: Rect::new(10 + i * 26, 100, 16, 16),
                size: s,
            })
            .collect();
        let p = place(&inputs, OUT, &hint());
        assert!(p.iter().all(|pl| pl.rect.y <= 100 && !pl.collided));
    }

    #[test]
    fn chip_moves_left_on_collision() {
        let chip = ChipLayout::default();
        let r = chip_rect(120, OUT, &chip, &[]);
        assert_eq!(r, Rect::new(1920 - 12 - 120, 1080 - 12 - 22, 120, 22));
        let badge = Rect::new(r.x + 5, r.y + 5, 24, 21);
        let l = chip_rect(120, OUT, &chip, &[badge]);
        assert_eq!(l, Rect::new(12, 1080 - 12 - 22, 120, 22));
    }
}
