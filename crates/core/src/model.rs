//! Shared domain types: scopes, targets, elements, timings.

use std::fmt;
use std::str::FromStr;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::geom::{Global, OutputId, Rect};

/// Which surfaces a hint session covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum Scope {
    #[default]
    #[serde(rename = "focused")]
    Focused,
    #[serde(rename = "focused+layers")]
    FocusedAndLayers,
    #[serde(rename = "layers")]
    Layers,
    #[serde(rename = "all")]
    All,
}

impl Scope {
    pub fn as_str(&self) -> &'static str {
        match self {
            Scope::Focused => "focused",
            Scope::FocusedAndLayers => "focused+layers",
            Scope::Layers => "layers",
            Scope::All => "all",
        }
    }
}

impl fmt::Display for Scope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown scope: {0}")]
pub struct ParseScopeError(pub String);

impl FromStr for Scope {
    type Err = ParseScopeError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "focused" => Ok(Scope::Focused),
            "focused+layers" => Ok(Scope::FocusedAndLayers),
            "layers" => Ok(Scope::Layers),
            "all" => Ok(Scope::All),
            _ => Err(ParseScopeError(s.to_owned())),
        }
    }
}

/// The compositor surface a target refers to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Surface {
    Toplevel { address: String },
    Layer { address: String, namespace: String },
}

impl Surface {
    pub fn is_layer(&self) -> bool {
        matches!(self, Surface::Layer { .. })
    }
}

/// One surface to collect elements from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Target {
    pub surface: Surface,
    pub rect: Rect<Global>,
    pub output: OutputId,
    pub pid: Option<u32>,
    pub class: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ElementId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ElementSource {
    Demo,
    Atspi,
    Vision,
    Quickshell,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionKind {
    Click,
    RightClick,
    MiddleClick,
    DoubleClick,
    Focus,
    Hover,
    Scroll,
    Drag,
}

/// One hintable thing on screen, in global logical coordinates.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Element {
    pub id: ElementId,
    pub rect: Rect<Global>,
    pub role: String,
    pub name: String,
    pub source: ElementSource,
    pub actions: Vec<ActionKind>,
    pub confidence: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StageTiming {
    pub stage: String,
    pub ms: f64,
}

/// Per-stage durations of one pipeline run, in execution order.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Timings(pub Vec<StageTiming>);

impl Timings {
    pub fn record(&mut self, stage: &str, d: Duration) {
        let ms = d.as_secs_f64() * 1000.0;
        tracing::debug!(stage = %stage, ms = ms as f32, "stage done");
        self.0.push(StageTiming {
            stage: stage.to_owned(),
            ms,
        });
    }
    pub fn get(&self, stage: &str) -> Option<f64> {
        self.0.iter().find(|s| s.stage == stage).map(|s| s.ms)
    }
    pub fn total_ms(&self) -> f64 {
        self.0.iter().map(|s| s.ms).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_parse() {
        assert_eq!(
            "focused+layers".parse::<Scope>(),
            Ok(Scope::FocusedAndLayers)
        );
        assert_eq!("focused".parse::<Scope>(), Ok(Scope::Focused));
        assert!("nope".parse::<Scope>().is_err());
        assert_eq!(
            serde_json::to_string(&Scope::FocusedAndLayers).unwrap(),
            "\"focused+layers\""
        );
    }

    #[test]
    fn element_round_trip() {
        let e = Element {
            id: ElementId(3),
            rect: Rect::new(10, 20, 30, 40),
            role: "push button".into(),
            name: "OK".into(),
            source: ElementSource::Demo,
            actions: vec![ActionKind::Click, ActionKind::RightClick],
            confidence: 1.0,
        };
        let v: serde_json::Value = serde_json::to_value(&e).unwrap();
        let mut keys: Vec<_> = v.as_object().unwrap().keys().cloned().collect();
        keys.sort();
        assert_eq!(
            keys,
            [
                "actions",
                "confidence",
                "id",
                "name",
                "rect",
                "role",
                "source"
            ]
        );
        let back: Element = serde_json::from_value(v).unwrap();
        assert_eq!(back, e);
    }

    #[test]
    fn target_json() {
        let t = Target {
            surface: Surface::Toplevel {
                address: "0x1".into(),
            },
            rect: Rect::new(0, 0, 1, 1),
            output: OutputId("eDP-1".into()),
            pid: Some(4),
            class: "kitty".into(),
        };
        let s = serde_json::to_string(&t).unwrap();
        assert!(
            s.contains(r#""surface":{"kind":"toplevel","address":"0x1"}"#),
            "{s}"
        );
        assert_eq!(serde_json::from_str::<Target>(&s).unwrap(), t);
    }
}
