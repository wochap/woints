//! When vision runs for a target (design D18): a pure function of the
//! config, the AT-SPI result and the target kind.

use woints_core::config::{VisionConfig, VisionMode};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    pub run: bool,
    pub reason: &'static str,
}

impl Decision {
    fn run(reason: &'static str) -> Self {
        Self { run: true, reason }
    }
    fn skip(reason: &'static str) -> Self {
        Self { run: false, reason }
    }
}

pub use woints_core::config::glob;

/// Decide whether vision runs for one target. `atspi_count` is the number of
/// usable AT-SPI elements; `other_served` whether any provider returned
/// elements for it; `wake_failed` whether a Chromium tree stayed dormant
/// after a wake attempt.
pub fn decide_vision(
    cfg: &VisionConfig,
    atspi_count: usize,
    class: &str,
    is_layer: bool,
    other_served: bool,
    wake_failed: bool,
) -> Decision {
    match cfg.mode {
        VisionMode::Off => return Decision::skip("mode_off"),
        VisionMode::Always => return Decision::run("mode_always"),
        VisionMode::Fallback => {}
    }
    if cfg.force_classes.iter().any(|g| glob(g, class)) {
        return Decision::run("forced_class");
    }
    if wake_failed {
        return Decision::skip("chromium_dormant");
    }
    if is_layer {
        return if other_served {
            Decision::skip("layer_served")
        } else {
            Decision::run("layer_without_provider")
        };
    }
    if atspi_count < cfg.min_atspi_elements {
        Decision::run("too_few_atspi")
    } else {
        Decision::skip("enough_atspi")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(mode: VisionMode) -> VisionConfig {
        VisionConfig {
            mode,
            force_classes: vec!["steam*".into()],
            ..VisionConfig::default()
        }
    }

    #[test]
    fn enough_atspi() {
        let d = decide_vision(
            &cfg(VisionMode::Fallback),
            12,
            "org.gnome.Nautilus",
            false,
            true,
            false,
        );
        assert_eq!(d, Decision::skip("enough_atspi"));
    }

    #[test]
    fn too_few_atspi() {
        let d = decide_vision(&cfg(VisionMode::Fallback), 2, "kitty", false, true, false);
        assert_eq!(d, Decision::run("too_few_atspi"));
    }

    #[test]
    fn forced_class() {
        let d = decide_vision(
            &cfg(VisionMode::Fallback),
            50,
            "steamwebhelper",
            false,
            true,
            false,
        );
        assert_eq!(d, Decision::run("forced_class"));
    }

    #[test]
    fn layer_without_provider() {
        let d = decide_vision(&cfg(VisionMode::Fallback), 0, "", true, false, false);
        assert_eq!(d, Decision::run("layer_without_provider"));
        assert!(!decide_vision(&cfg(VisionMode::Fallback), 4, "", true, true, false).run);
    }

    #[test]
    fn mode_always() {
        assert!(decide_vision(&cfg(VisionMode::Always), 99, "foot", false, true, false).run);
    }

    #[test]
    fn mode_off() {
        let d = decide_vision(&cfg(VisionMode::Off), 0, "steam", false, false, false);
        assert_eq!(d, Decision::skip("mode_off"));
    }

    #[test]
    fn chromium_dormant() {
        let f = cfg(VisionMode::Fallback);
        let d = decide_vision(&f, 0, "bruno", false, false, true);
        assert_eq!(d, Decision::skip("chromium_dormant"));
        let d = decide_vision(&f, 0, "steamwebhelper", false, false, true);
        assert_eq!(d, Decision::run("forced_class"));
        let d = decide_vision(&cfg(VisionMode::Always), 0, "bruno", false, false, true);
        assert_eq!(d, Decision::run("mode_always"));
        let d = decide_vision(&f, 0, "google-chrome", false, false, false);
        assert_eq!(d, Decision::run("too_few_atspi"));
    }

    #[test]
    fn globs() {
        assert!(glob("steam*", "steam"));
        assert!(glob("*code*", "visual-code-oss"));
        assert!(glob("fo?t", "foot"));
        assert!(!glob("steam*", "kitty"));
        assert!(glob("*", ""));
    }
}
