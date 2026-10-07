//! Config keys and defaults; the file loader arrives in M5.

use serde::{Deserialize, Serialize};

use crate::merge::MergeParams;

/// When the vision provider runs (`vision.mode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VisionMode {
    #[default]
    Fallback,
    Always,
    Off,
}

/// Edge operator of the classical detector.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EdgeMode {
    #[default]
    Canny,
    Laplacian,
}

/// `[vision]` keys.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct VisionConfig {
    pub mode: VisionMode,
    pub min_atspi_elements: usize,
    /// Hyprland classes (glob, `*` and `?`) that always get vision.
    pub force_classes: Vec<String>,
    /// Smallest box side, logical px.
    pub min_box: i32,
    /// Largest box side, logical px.
    pub max_box: i32,
    pub max_aspect: f32,
    /// Tallest component merged into text lines, logical px.
    pub text_height: i32,
    pub downscale_width: u32,
    /// Canny `(low, high)` thresholds on gradient magnitude.
    pub canny: (f32, f32),
    pub edge: EdgeMode,
    pub budget_ms: u64,
    pub max_elements: usize,
    pub link_hue: bool,
}

impl Default for VisionConfig {
    fn default() -> Self {
        Self {
            mode: VisionMode::Fallback,
            min_atspi_elements: 3,
            force_classes: Vec::new(),
            min_box: 8,
            max_box: 600,
            max_aspect: 20.0,
            text_height: 15,
            downscale_width: 1920,
            canny: (70.0, 220.0),
            edge: EdgeMode::Canny,
            budget_ms: 250,
            max_elements: 150,
            link_hue: true,
        }
    }
}

/// Every config section with its defaults.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub vision: VisionConfig,
    pub merge: MergeParams,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_partial_json() {
        let c: Config = serde_json::from_str(
            r#"{"vision":{"mode":"always","force_classes":["steam*"]},"merge":{"iou_threshold":0.5,"containment_threshold":0.9}}"#,
        )
        .unwrap();
        assert_eq!(c.vision.mode, VisionMode::Always);
        assert_eq!(c.vision.min_atspi_elements, 3);
        assert_eq!(c.vision.canny, (70.0, 220.0));
        assert_eq!(c.merge.iou_threshold, 0.5);
        assert_eq!(Config::default().vision.budget_ms, 250);
    }
}
