//! Per-toolkit behaviour table (design D13, D18, D19). Plain data so M5 can
//! override entries per class from configuration.

use serde::{Deserialize, Serialize};

/// Element collection strategy, tried in profile order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Strategy {
    /// `org.a11y.atspi.Collection.GetMatches`.
    Collection,
    /// Bounded breadth-first walk of the accessible tree.
    TreeWalk,
}

impl Strategy {
    pub fn as_str(&self) -> &'static str {
        match self {
            Strategy::Collection => "collection",
            Strategy::TreeWalk => "walk",
        }
    }
}

/// Geometry predicates that drop elements which are reported but not visible.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PhantomFilter {
    /// Width or height is 0.
    ZeroSize,
    /// No overlap with the target rect.
    OutsideTarget,
    /// Not contained in the parent's extents when the parent has one of these roles.
    NotInsideParent { parent_roles: Vec<String> },
    /// List items under a combo box that lacks EXPANDED (Qt).
    CollapsedComboPopup,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolkitProfile {
    pub strategies: Vec<Strategy>,
    pub phantom_filters: Vec<PhantomFilter>,
    pub skip_web_content: bool,
    pub action_preference: Vec<String>,
}

/// Classes of Chromium-based browsers whose page content Vimium covers.
pub const CHROMIUM_BROWSERS: &[&str] = &["brave-browser", "google-chrome", "chromium"];

pub const DEFAULT_ACTION_PREFERENCE: &[&str] = &["click", "press", "activate", "toggle"];

/// Filters every provider applies regardless of profile (geometry filters
/// requirement); profiles add toolkit-specific ones.
pub fn base_filters() -> Vec<PhantomFilter> {
    vec![
        PhantomFilter::ZeroSize,
        PhantomFilter::OutsideTarget,
        PhantomFilter::NotInsideParent {
            parent_roles: vec!["list".into(), "menu".into()],
        },
    ]
}

impl Default for ToolkitProfile {
    fn default() -> Self {
        Self {
            strategies: vec![Strategy::Collection, Strategy::TreeWalk],
            phantom_filters: Vec::new(),
            skip_web_content: false,
            action_preference: DEFAULT_ACTION_PREFERENCE
                .iter()
                .map(|s| (*s).to_owned())
                .collect(),
        }
    }
}

impl ToolkitProfile {
    /// Profile for an AT-SPI `ToolkitName`/`Version` and the Hyprland class.
    pub fn lookup(toolkit: &str, version: &str, class: &str) -> Self {
        let mut p = Self::default();
        let major = version
            .split('.')
            .next()
            .and_then(|v| v.trim().parse::<u32>().ok());
        match toolkit.to_ascii_lowercase().as_str() {
            "gtk" if major.is_some_and(|m| m >= 4) => {
                p.strategies = vec![Strategy::TreeWalk];
            }
            "qt" => p.phantom_filters.push(PhantomFilter::CollapsedComboPopup),
            "gecko" => p.skip_web_content = true,
            "chromium" => {
                let class = class.to_ascii_lowercase();
                p.skip_web_content = CHROMIUM_BROWSERS.contains(&class.as_str());
            }
            _ => {}
        }
        p
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gtk4_walks_only() {
        let p = ToolkitProfile::lookup("GTK", "4.22.1", "org.gnome.Calculator");
        assert_eq!(p.strategies, [Strategy::TreeWalk]);
        assert!(!p.skip_web_content);
    }

    #[test]
    fn gtk3_and_qt() {
        let g = ToolkitProfile::lookup("gtk", "3.24.43", "gtk3-widget-factory");
        assert_eq!(g.strategies, [Strategy::Collection, Strategy::TreeWalk]);
        assert!(
            !g.phantom_filters
                .contains(&PhantomFilter::CollapsedComboPopup)
        );
        let q = ToolkitProfile::lookup("Qt", "6.11.0", "qt6ct");
        assert_eq!(q.strategies, [Strategy::Collection, Strategy::TreeWalk]);
        assert!(
            q.phantom_filters
                .contains(&PhantomFilter::CollapsedComboPopup)
        );
    }

    #[test]
    fn browsers_prune_web_content() {
        assert!(ToolkitProfile::lookup("Gecko", "140", "firefox").skip_web_content);
        for c in CHROMIUM_BROWSERS {
            assert!(ToolkitProfile::lookup("Chromium", "", c).skip_web_content);
        }
        assert!(ToolkitProfile::lookup("Chromium", "", "Brave-browser").skip_web_content);
    }

    #[test]
    fn electron_keeps_web_content() {
        assert!(!ToolkitProfile::lookup("Chromium", "", "Element").skip_web_content);
    }

    #[test]
    fn unknown_toolkit_is_default() {
        let p = ToolkitProfile::lookup("frobkit", "1", "x");
        assert_eq!(p, ToolkitProfile::default());
        assert_eq!(p.strategies, [Strategy::Collection, Strategy::TreeWalk]);
        assert!(!p.skip_web_content);
        assert!(p.phantom_filters.is_empty());
        assert_eq!(p.action_preference, DEFAULT_ACTION_PREFERENCE);
    }
}
