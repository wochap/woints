//! JSON-lines protocol between `woints` and `wointsd` (design D8).
//!
//! Requests are objects tagged by `"cmd"`; responses carry `"ok": true` plus
//! a flattened payload or `"ok": false, "error": "<message>"`.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::act::Axis;
use crate::geom::{Global, OutputId, Rect};
use crate::model::{Action, Element, ElementId, Scope, Target, Timings};
use crate::ports::CollectReport;
use crate::vision::VisionReport;

/// Socket file name under `$XDG_RUNTIME_DIR`.
pub const SOCKET_NAME: &str = "woints.sock";

pub fn socket_path() -> std::path::PathBuf {
    let dir = std::env::var_os("XDG_RUNTIME_DIR").unwrap_or_else(|| "/tmp".into());
    std::path::PathBuf::from(dir).join(SOCKET_NAME)
}

fn default_iterations() -> u32 {
    10
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Request {
    Hints {
        #[serde(default)]
        scope: Scope,
        /// Action performed when a label is matched (default `click`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        action: Option<Action>,
    },
    Cancel,
    Status,
    Quit,
    Snapshot {
        #[serde(default)]
        scope: Scope,
    },
    Bench {
        #[serde(default = "default_iterations")]
        iterations: u32,
        #[serde(default)]
        scope: Scope,
    },
    /// Act on an element of a stored snapshot without showing the overlay.
    Act {
        snapshot_id: u64,
        id: ElementId,
        #[serde(default)]
        action: Action,
        /// Drag destination.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        to: Option<ElementId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        axis: Option<Axis>,
        /// Scroll lines; negative scrolls up or left.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        amount: Option<i32>,
    },
}

const COMMANDS: &[&str] = &[
    "hints", "cancel", "status", "quit", "snapshot", "bench", "act",
];

/// Parse one request line, producing the protocol's error message on failure.
pub fn parse_request(line: &str) -> Result<Request, String> {
    let v: Value = serde_json::from_str(line).map_err(|e| format!("invalid json: {e}"))?;
    let cmd = v
        .get("cmd")
        .and_then(Value::as_str)
        .ok_or_else(|| "missing cmd".to_owned())?
        .to_owned();
    if !COMMANDS.contains(&cmd.as_str()) {
        return Err(format!("unknown command: {cmd}"));
    }
    serde_json::from_value(v).map_err(|e| format!("bad {cmd} request: {e}"))
}

/// Response envelope.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Response {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(flatten)]
    pub payload: Map<String, Value>,
}

impl Response {
    pub fn ok() -> Self {
        Self {
            ok: true,
            error: None,
            payload: Map::new(),
        }
    }

    pub fn with<T: Serialize>(payload: &T) -> Self {
        let payload = match serde_json::to_value(payload) {
            Ok(Value::Object(m)) => m,
            Ok(_) | Err(_) => Map::new(),
        };
        Self {
            ok: true,
            error: None,
            payload,
        }
    }

    pub fn err(msg: impl Into<String>) -> Self {
        Self {
            ok: false,
            error: Some(msg.into()),
            payload: Map::new(),
        }
    }

    pub fn to_line(&self) -> String {
        let mut s = serde_json::to_string(self)
            .unwrap_or_else(|_| r#"{"ok":false,"error":"serialisation failed"}"#.to_owned());
        s.push('\n');
        s
    }

    /// Decode the payload into a typed reply.
    pub fn payload<T: for<'de> Deserialize<'de>>(&self) -> Result<T, serde_json::Error> {
        serde_json::from_value(Value::Object(self.payload.clone()))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    Idle,
    Showing,
    /// Drag phase 2: picking the destination.
    ShowingSecond,
    Scrolling,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HintsReply {
    pub state: State,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub targets: Option<usize>,
    pub elements: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vision: Option<VisionReport>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StateReply {
    pub state: State,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OutputInfo {
    pub name: OutputId,
    pub scale: f32,
    pub rect: Rect<Global>,
    pub focused: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StatusReply {
    pub state: State,
    pub version: String,
    pub uptime_ms: u64,
    pub outputs: Vec<OutputInfo>,
    #[serde(default)]
    pub hyprland_version: Option<String>,
    pub overlay: bool,
    pub last_timings: Timings,
    #[serde(default)]
    pub providers: Vec<ProviderStatus>,
    /// Reports from the last collection, one per target.
    #[serde(default)]
    pub last_collect: Vec<CollectReport>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_action: Option<ActionOutcome>,
    /// `movecursor`, `absolute` or `unavailable`.
    #[serde(default)]
    pub pointer: String,
    /// `data-control` or `unavailable`.
    #[serde(default)]
    pub clipboard: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderStatus {
    pub name: String,
    pub available: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

/// Result of the most recent action, from the overlay or `act`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActionOutcome {
    pub element: ElementId,
    pub action: Action,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub ms: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActReply {
    pub performed: Action,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SnapshotReply {
    pub snapshot_id: u64,
    pub targets: Vec<Target>,
    pub elements: Vec<Element>,
    pub timings: Timings,
    #[serde(default)]
    pub vision: VisionReport,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BenchRow {
    pub stage: String,
    pub p50_ms: f64,
    pub p95_ms: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BenchReply {
    pub iterations: u32,
    pub rows: Vec<BenchRow>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn rt(req: Request, wire: Value) {
        assert_eq!(serde_json::to_value(&req).unwrap(), wire);
        assert_eq!(parse_request(&wire.to_string()).unwrap(), req);
    }

    #[test]
    fn every_command() {
        rt(
            Request::Hints {
                scope: Scope::Focused,
                action: None,
            },
            json!({"cmd":"hints","scope":"focused"}),
        );
        rt(
            Request::Hints {
                scope: Scope::All,
                action: Some(Action::Drag),
            },
            json!({"cmd":"hints","scope":"all","action":"drag"}),
        );
        rt(
            Request::Act {
                snapshot_id: 17,
                id: ElementId(3),
                action: Action::Right,
                to: None,
                axis: None,
                amount: None,
            },
            json!({"cmd":"act","snapshot_id":17,"id":3,"action":"right"}),
        );
        rt(
            Request::Act {
                snapshot_id: 7,
                id: ElementId(3),
                action: Action::Drag,
                to: Some(ElementId(9)),
                axis: None,
                amount: None,
            },
            json!({"cmd":"act","snapshot_id":7,"id":3,"action":"drag","to":9}),
        );
        rt(
            Request::Act {
                snapshot_id: 7,
                id: ElementId(3),
                action: Action::Scroll,
                to: None,
                axis: Some(Axis::Vertical),
                amount: Some(-3),
            },
            json!({"cmd":"act","snapshot_id":7,"id":3,"action":"scroll","axis":"vertical","amount":-3}),
        );
        rt(Request::Cancel, json!({"cmd":"cancel"}));
        rt(Request::Status, json!({"cmd":"status"}));
        rt(Request::Quit, json!({"cmd":"quit"}));
        rt(
            Request::Snapshot {
                scope: Scope::Focused,
            },
            json!({"cmd":"snapshot","scope":"focused"}),
        );
        rt(
            Request::Bench {
                iterations: 20,
                scope: Scope::Focused,
            },
            json!({"cmd":"bench","iterations":20,"scope":"focused"}),
        );
    }

    #[test]
    fn defaults() {
        assert_eq!(
            parse_request(r#"{"cmd":"hints"}"#).unwrap(),
            Request::Hints {
                scope: Scope::Focused,
                action: None
            }
        );
        assert_eq!(
            parse_request(r#"{"cmd":"bench"}"#).unwrap(),
            Request::Bench {
                iterations: 10,
                scope: Scope::Focused
            }
        );
    }

    #[test]
    fn act_defaults_and_reply() {
        assert_eq!(
            parse_request(r#"{"cmd":"act","snapshot_id":17,"id":3}"#).unwrap(),
            Request::Act {
                snapshot_id: 17,
                id: ElementId(3),
                action: Action::Click,
                to: None,
                axis: None,
                amount: None,
            }
        );
        assert!(parse_request(r#"{"cmd":"act","snapshot_id":1,"id":1,"action":"jump"}"#).is_err());
        assert_eq!(
            serde_json::to_value(Response::with(&ActReply {
                performed: Action::Click
            }))
            .unwrap(),
            json!({"ok":true,"performed":"click"})
        );
    }

    #[test]
    fn unknown_command() {
        assert_eq!(
            parse_request(r#"{"cmd":"frobnicate"}"#).unwrap_err(),
            "unknown command: frobnicate"
        );
        assert_eq!(
            Response::err("unknown command: frobnicate").to_line(),
            "{\"ok\":false,\"error\":\"unknown command: frobnicate\"}\n"
        );
    }

    #[test]
    fn hints_reply_shape() {
        let r = Response::with(&HintsReply {
            state: State::Showing,
            targets: Some(1),
            elements: 5,
            vision: None,
        });
        let v: Value = serde_json::from_str(&r.to_line()).unwrap();
        assert_eq!(
            v,
            json!({"ok":true,"state":"showing","targets":1,"elements":5})
        );
        let idle = Response::with(&HintsReply {
            state: State::Idle,
            targets: None,
            elements: 0,
            vision: None,
        });
        assert_eq!(
            serde_json::to_value(&idle).unwrap(),
            json!({"ok":true,"state":"idle","elements":0})
        );
    }

    #[test]
    fn snapshot_vision_field() {
        let r = SnapshotReply {
            snapshot_id: 1,
            targets: vec![],
            elements: vec![],
            timings: Timings::default(),
            vision: VisionReport {
                ran: true,
                reason: "too_few_atspi".into(),
            },
        };
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(v["vision"], json!({"ran": true, "reason": "too_few_atspi"}));
        assert_eq!(serde_json::from_value::<SnapshotReply>(v).unwrap(), r);
    }

    #[test]
    fn response_round_trip() {
        let r = Response::with(&StateReply { state: State::Idle });
        let back: Response = serde_json::from_str(&r.to_line()).unwrap();
        assert!(back.ok);
        assert_eq!(back.payload::<StateReply>().unwrap().state, State::Idle);
        assert_eq!(
            serde_json::to_string(&Response::ok()).unwrap(),
            r#"{"ok":true}"#
        );
    }
}
