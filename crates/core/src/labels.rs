//! Prefix-free hint labels and priority assignment (design D7, §01/§04).

use crate::geom::{Global, Point, Rect};

/// Longest label the generator produces.
pub const MAX_DEPTH: u32 = 3;

/// A label as alphabet indices; the glyph shown for each index comes from the
/// active keyboard layout (keycode matching), so this stays layout-agnostic.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Label(pub Vec<u8>);

impl Label {
    pub fn render(&self, alphabet: &[char]) -> String {
        self.0.iter().map(|&i| alphabet[i as usize]).collect()
    }
    pub fn len(&self) -> usize {
        self.0.len()
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    pub fn is_prefix_of(&self, other: &Label) -> bool {
        other.0.starts_with(&self.0)
    }
}

/// Generate `n` distinct prefix-free labels over an alphabet of `k` glyphs.
///
/// Singles come from the head of the alphabet; the remaining tail glyphs
/// become prefixes for longer labels (an n-ary prefix tree up to depth 3).
/// The result is ordered shortest first. At most `k^3` labels are returned.
pub fn labels(n: usize, k: usize) -> Vec<Label> {
    let k = k.min(u8::MAX as usize);
    let n = n.min(k.pow(MAX_DEPTH));
    let mut out = Vec::with_capacity(n);
    // Shallowest tree that fits, so 81 targets get 81 pairs, not 8 singles
    // plus a pile of triples.
    let depth = (1..=MAX_DEPTH)
        .find(|&d| k.pow(d) >= n)
        .unwrap_or(MAX_DEPTH);
    build(n, k, depth, &mut Vec::new(), &mut out);
    out.sort_by_key(|l| l.len());
    out
}

fn build(n: usize, k: usize, depth: u32, prefix: &mut Vec<u8>, out: &mut Vec<Label>) {
    if n == 0 || k == 0 {
        return;
    }
    if n <= k || depth <= 1 {
        for i in 0..n.min(k) {
            prefix.push(i as u8);
            out.push(Label(prefix.clone()));
            prefix.pop();
        }
        return;
    }
    let sub_cap = k.pow(depth - 1);
    // Maximise singles: s + (k - s) * sub_cap >= n.
    let singles = (0..k)
        .rev()
        .find(|&s| s + (k - s) * sub_cap >= n)
        .unwrap_or(0);
    for i in 0..singles {
        prefix.push(i as u8);
        out.push(Label(prefix.clone()));
        prefix.pop();
    }
    // Spread the rest evenly over the tail prefixes, using only as many
    // prefixes as needed so subtrees stay as shallow as possible.
    let rest = n - singles;
    let tail = k - singles;
    let per = rest.div_ceil(tail);
    let used = rest.div_ceil(per.max(1)).min(tail);
    let (base, extra) = (rest / used, rest % used);
    for j in 0..used {
        let glyph = (k - tail + j) as u8;
        let size = base + usize::from(j < extra);
        prefix.push(glyph);
        build(size, k, depth - 1, prefix, out);
        prefix.pop();
    }
}

/// Input to [`priority`].
#[derive(Debug, Clone, Copy)]
pub struct PriorityItem {
    pub rect: Rect<Global>,
    pub is_layer: bool,
}

/// Order item indices from highest to lowest priority: larger area and
/// closer to `focus` first (blended ranks), layer targets after toplevels.
pub fn priority(items: &[PriorityItem], focus: Point<Global>) -> Vec<usize> {
    let n = items.len();
    if n == 0 {
        return Vec::new();
    }
    let rank = |key: &dyn Fn(usize) -> i64| -> Vec<f32> {
        let mut idx: Vec<usize> = (0..n).collect();
        idx.sort_by_key(|&i| key(i));
        // Ties share the rank of the first item with that key.
        let mut r = vec![0.0; n];
        let mut first = 0;
        for (pos, &i) in idx.iter().enumerate() {
            if pos > 0 && key(i) != key(idx[pos - 1]) {
                first = pos;
            }
            r[i] = first as f32 / n as f32;
        }
        r
    };
    let area = rank(&|i| -items[i].rect.area());
    let dist = rank(&|i| {
        let c = items[i].rect.center();
        let (dx, dy) = (i64::from(c.x - focus.x), i64::from(c.y - focus.y));
        dx * dx + dy * dy
    });
    let score: Vec<f32> = (0..n).map(|i| 0.6 * area[i] + 0.4 * dist[i]).collect();
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|&a, &b| {
        (items[a].is_layer, score[a])
            .partial_cmp(&(items[b].is_layer, score[b]))
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.cmp(&b))
    });
    order
}

/// Give `labels[0]` (shortest) to `order[0]` and so on; returns labels
/// indexed by item.
pub fn assign(labels: &[Label], order: &[usize]) -> Vec<Option<Label>> {
    let mut out = vec![None; order.len()];
    for (label, &item) in labels.iter().zip(order) {
        out[item] = Some(label.clone());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    const HOME: &str = "asdfghjkl";

    fn alpha() -> Vec<char> {
        HOME.chars().collect()
    }

    fn strings(n: usize) -> Vec<String> {
        labels(n, 9).iter().map(|l| l.render(&alpha())).collect()
    }

    fn prefix_free(ls: &[Label]) -> bool {
        ls.iter().enumerate().all(|(i, a)| {
            ls.iter()
                .enumerate()
                .all(|(j, b)| i == j || !a.is_prefix_of(b))
        })
    }

    #[test]
    fn five_targets() {
        assert_eq!(strings(5), ["a", "s", "d", "f", "g"]);
    }

    #[test]
    fn forty_targets() {
        let ls = labels(40, 9);
        assert_eq!(ls.len(), 40);
        assert_eq!(ls.iter().filter(|l| l.len() == 1).count(), 5);
        let pairs: Vec<_> = ls.iter().filter(|l| l.len() == 2).collect();
        assert_eq!(pairs.len(), 35);
        assert!(
            pairs.iter().all(|l| l.0[0] >= 5),
            "pairs start with the last 4 glyphs"
        );
        assert!(prefix_free(&ls));
    }

    #[test]
    fn two_hundred_targets() {
        let ls = labels(200, 9);
        assert_eq!(ls.len(), 200);
        assert!(ls.iter().all(|l| l.len() <= 3));
        assert!(prefix_free(&ls));
    }

    #[test]
    fn exactly_k_and_k_squared() {
        assert!(labels(9, 9).iter().all(|l| l.len() == 1));
        let ls = labels(81, 9);
        assert!(ls.iter().all(|l| l.len() == 2));
        assert!(prefix_free(&ls));
    }

    proptest! {
        #[test]
        fn prefix_free_and_distinct(n in 1usize..=729) {
            let ls = labels(n, 9);
            prop_assert_eq!(ls.len(), n);
            prop_assert!(ls.iter().all(|l| (1..=3).contains(&l.len())));
            prop_assert!(prefix_free(&ls));
            prop_assert!(ls.windows(2).all(|w| w[0].len() <= w[1].len()));
        }
    }

    #[test]
    fn central_button_gets_single_glyph() {
        let win: Rect<Global> = Rect::new(0, 0, 800, 600);
        let mut items = vec![PriorityItem {
            rect: Rect::new(300, 280, 200, 40),
            is_layer: false,
        }];
        for i in 0..11 {
            items.push(PriorityItem {
                rect: Rect::new(4 + i * 20, 4, 16, 16),
                is_layer: false,
            });
        }
        // Put the button last in input order so ordering is not accidental.
        items.rotate_left(1);
        let order = priority(&items, win.center());
        let ls = labels(items.len(), 9);
        let assigned = assign(&ls, &order);
        assert_eq!(assigned[11].as_ref().unwrap().len(), 1);
        assert_eq!(order[0], 11);
    }

    #[test]
    fn layers_rank_last() {
        let items = [
            PriorityItem {
                rect: Rect::new(0, 0, 1000, 1000),
                is_layer: true,
            },
            PriorityItem {
                rect: Rect::new(0, 0, 5, 5),
                is_layer: false,
            },
        ];
        assert_eq!(priority(&items, Point::new(0, 0)), [1, 0]);
    }
}
