//! Target → application → frame pairing (design D15). Pure functions; the
//! D-Bus side lives in `provider`.

use woints_core::geom::Size;

/// One application under the Registry root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppInfo {
    pub bus_name: String,
    pub name: String,
    pub toolkit: String,
    pub version: String,
    pub pid: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairReason {
    Pid,
    Fuzzy,
}

impl PairReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            PairReason::Pid => "pid",
            PairReason::Fuzzy => "fuzzy",
        }
    }
}

fn norm(s: &str) -> String {
    s.trim()
        .to_lowercase()
        .chars()
        .map(|c| {
            if c == '.' || c == '_' || c == ' ' {
                '-'
            } else {
                c
            }
        })
        .collect()
}

/// MVP matcher: lowercase, `.`/`_` folded to `-`, substring either way, or
/// equal last `-` segment.
pub fn fuzzy_match(class: &str, app_name: &str) -> bool {
    let (a, b) = (norm(class), norm(app_name));
    if a.is_empty() || b.is_empty() {
        return false;
    }
    if a.contains(&b) || b.contains(&a) {
        return true;
    }
    let last = |s: &str| s.rsplit('-').next().map(str::to_owned);
    matches!((last(&a), last(&b)), (Some(x), Some(y)) if !x.is_empty() && x == y)
}

/// Index of the app for `pid`, else the fuzzy match for `class`.
pub fn pick_app(apps: &[AppInfo], pid: Option<u32>, class: &str) -> Option<(usize, PairReason)> {
    if let Some(pid) = pid {
        if let Some(i) = apps.iter().position(|a| a.pid == Some(pid)) {
            return Some((i, PairReason::Pid));
        }
    }
    apps.iter()
        .position(|a| fuzzy_match(class, &a.name))
        .map(|i| (i, PairReason::Fuzzy))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameInfo {
    pub active: bool,
    pub size: Option<Size>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameReason {
    Active,
    Size,
    /// First frame, nothing better known.
    Guess,
}

pub const SIZE_TOLERANCE: i32 = 2;

/// ACTIVE frame, else the one whose size equals `target` within 2 px, else the first.
pub fn pick_frame(frames: &[FrameInfo], target: Size) -> Option<(usize, FrameReason)> {
    if let Some(i) = frames.iter().position(|f| f.active) {
        return Some((i, FrameReason::Active));
    }
    let fits = |s: &Size| {
        (s.w - target.w).abs() <= SIZE_TOLERANCE && (s.h - target.h).abs() <= SIZE_TOLERANCE
    };
    let sized: Vec<usize> = frames
        .iter()
        .enumerate()
        .filter(|(_, f)| f.size.as_ref().is_some_and(fits))
        .map(|(i, _)| i)
        .collect();
    if let [i] = sized[..] {
        return Some((i, FrameReason::Size));
    }
    if !frames.is_empty() {
        return Some((sized.first().copied().unwrap_or(0), FrameReason::Guess));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(bus: &str, name: &str, pid: Option<u32>) -> AppInfo {
        AppInfo {
            bus_name: bus.into(),
            name: name.into(),
            toolkit: "GTK".into(),
            version: "4.22".into(),
            pid,
        }
    }

    #[test]
    fn fuzzy() {
        assert!(fuzzy_match("org.gnome.Calculator", "gnome-calculator"));
        assert!(fuzzy_match("gtk3-widget-factory", "gtk3-widget-factory"));
        assert!(fuzzy_match("qt6ct", "qt6ct"));
        assert!(fuzzy_match("firefox", "Firefox"));
        assert!(fuzzy_match("org.kde.dolphin", "dolphin"));
        assert!(fuzzy_match("com_example_App", "app"));
        assert!(!fuzzy_match("kitty", "gnome-calculator"));
        assert!(!fuzzy_match("", "x"));
        assert!(!fuzzy_match("x", ""));
    }

    #[test]
    fn pid_wins_over_names() {
        let apps = vec![
            app(":1.7", "gnome-calculator", Some(1)),
            app(":1.42", "something", Some(349213)),
        ];
        assert_eq!(
            pick_app(&apps, Some(349213), "org.gnome.Calculator"),
            Some((1, PairReason::Pid))
        );
    }

    #[test]
    fn fuzzy_fallback_and_none() {
        let apps = vec![app(":1.7", "gnome-calculator", Some(1))];
        assert_eq!(
            pick_app(&apps, Some(99), "org.gnome.Calculator"),
            Some((0, PairReason::Fuzzy))
        );
        assert_eq!(pick_app(&apps, Some(99), "kitty"), None);
        assert_eq!(pick_app(&[], None, "kitty"), None);
    }

    fn f(active: bool, w: i32, h: i32) -> FrameInfo {
        FrameInfo {
            active,
            size: Some(Size { w, h }),
        }
    }

    #[test]
    fn frame_active_first() {
        let fr = [f(false, 1438, 858), f(true, 800, 600)];
        assert_eq!(
            pick_frame(&fr, Size { w: 1438, h: 858 }),
            Some((1, FrameReason::Active))
        );
    }

    #[test]
    fn frame_by_size() {
        let fr = [f(false, 800, 600), f(false, 1437, 859)];
        assert_eq!(
            pick_frame(&fr, Size { w: 1438, h: 858 }),
            Some((1, FrameReason::Size))
        );
    }

    #[test]
    fn frame_guess() {
        let fr = [f(false, 10, 10), f(false, 20, 20)];
        assert_eq!(
            pick_frame(&fr, Size { w: 1438, h: 858 }),
            Some((0, FrameReason::Guess))
        );
        let tie = [f(false, 10, 10), f(false, 100, 100), f(false, 100, 100)];
        assert_eq!(
            pick_frame(&tie, Size { w: 100, h: 100 }),
            Some((1, FrameReason::Guess))
        );
        assert_eq!(pick_frame(&[], Size { w: 1, h: 1 }), None);
    }
}
