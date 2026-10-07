//! Phantom filters (design D18): predicates applied once extents are known.

use woints_core::geom::{Global, Rect};
use woints_core::profile::PhantomFilter;

/// Parent facts needed by `NotInsideParent`.
#[derive(Debug, Clone, PartialEq)]
pub struct ParentInfo {
    pub role: String,
    pub rect: Rect<Global>,
}

/// One candidate after extents were read.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    pub rect: Rect<Global>,
    pub role: String,
    pub parent: Option<ParentInfo>,
    /// `Some(expanded)` when a combo box is among the nearest ancestors.
    pub combo_expanded: Option<bool>,
}

fn contains(outer: &Rect<Global>, inner: &Rect<Global>) -> bool {
    inner.x >= outer.x
        && inner.y >= outer.y
        && inner.x + inner.w <= outer.x + outer.w
        && inner.y + inner.h <= outer.y + outer.h
}

/// The first filter that rejects `c`, if any.
pub fn rejected_by<'a>(
    c: &Candidate,
    filters: &'a [PhantomFilter],
    target: &Rect<Global>,
) -> Option<&'a PhantomFilter> {
    filters.iter().find(|f| match f {
        PhantomFilter::ZeroSize => c.rect.w <= 0 || c.rect.h <= 0,
        PhantomFilter::OutsideTarget => !c.rect.intersects(target),
        PhantomFilter::NotInsideParent { parent_roles } => c.parent.as_ref().is_some_and(|p| {
            parent_roles.iter().any(|r| r == &p.role) && !contains(&p.rect, &c.rect)
        }),
        PhantomFilter::CollapsedComboPopup => {
            c.role == "list item" && c.combo_expanded == Some(false)
        }
    })
}

/// Indices of candidates that pass every filter.
pub fn keep(cands: &[Candidate], filters: &[PhantomFilter], target: &Rect<Global>) -> Vec<usize> {
    cands
        .iter()
        .enumerate()
        .filter(|(_, c)| {
            let r = rejected_by(c, filters, target);
            if let Some(f) = r {
                tracing::trace!(role = %c.role, rect = ?c.rect, filter = ?f, "dropped");
            }
            r.is_none()
        })
        .map(|(i, _)| i)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use woints_core::profile::{ToolkitProfile, base_filters};

    fn qt_filters() -> Vec<PhantomFilter> {
        let mut f = base_filters();
        f.extend(ToolkitProfile::lookup("Qt", "6.11", "qt6ct").phantom_filters);
        f
    }

    fn cand(x: i32, y: i32, w: i32, h: i32, role: &str) -> Candidate {
        Candidate {
            rect: Rect::new(x, y, w, h),
            role: role.into(),
            parent: None,
            combo_expanded: None,
        }
    }

    const TARGET: Rect<Global> = Rect::new(100, 50, 640, 480);

    #[test]
    fn zero_size_dropped() {
        let c = cand(100, 50, 0, 0, "push button");
        assert_eq!(
            rejected_by(&c, &base_filters(), &TARGET),
            Some(&PhantomFilter::ZeroSize)
        );
    }

    #[test]
    fn outside_target_dropped() {
        let c = cand(2000, 50, 30, 20, "push button");
        assert_eq!(
            rejected_by(&c, &base_filters(), &TARGET),
            Some(&PhantomFilter::OutsideTarget)
        );
        assert!(
            rejected_by(
                &cand(120, 60, 30, 20, "push button"),
                &base_filters(),
                &TARGET
            )
            .is_none()
        );
    }

    #[test]
    fn not_inside_list_parent() {
        let mut c = cand(120, 400, 100, 20, "list item");
        c.parent = Some(ParentInfo {
            role: "list".into(),
            rect: Rect::new(110, 60, 200, 100),
        });
        assert!(matches!(
            rejected_by(&c, &base_filters(), &TARGET),
            Some(PhantomFilter::NotInsideParent { .. })
        ));
        // A panel parent is not checked.
        c.parent.as_mut().unwrap().role = "panel".into();
        assert!(rejected_by(&c, &base_filters(), &TARGET).is_none());
    }

    #[test]
    fn qt_collapsed_combo_popup() {
        // qt6ct: list items at window (0,0,640,31) under a closed combo box.
        let mut item = cand(100, 50, 640, 31, "list item");
        item.combo_expanded = Some(false);
        let mut combo = cand(300, 200, 120, 28, "combo box");
        combo.combo_expanded = None;
        let mut open = item.clone();
        open.combo_expanded = Some(true);
        let plain = cand(100, 90, 640, 31, "list item");
        let cands = [item, combo, open, plain];
        assert_eq!(keep(&cands, &qt_filters(), &TARGET), [1, 2, 3]);
        // GTK has no combo filter.
        assert_eq!(keep(&cands, &base_filters(), &TARGET), [0, 1, 2, 3]);
    }
}
