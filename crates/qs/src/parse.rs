//! Text formats of `qs list --json`, `qs ipc show` and the `woints` IPC target.

use serde::Deserialize;

use woints_core::geom::Rect;
use woints_core::model::{Action, Element, ElementId, ElementSource, Surface, Target};

/// Module protocol version this adapter speaks (design D16).
pub const PROTOCOL: u32 = 1;
/// IPC target name the module registers.
pub const TARGET: &str = "woints";

/// One entry of `qs list --all --json`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Instance {
    pub id: String,
    pub pid: u32,
    #[serde(default)]
    pub config_path: String,
}

pub fn parse_instances(s: &str) -> Result<Vec<Instance>, String> {
    if s.trim().is_empty() {
        return Ok(Vec::new());
    }
    serde_json::from_str(s).map_err(|e| format!("qs list: {e}"))
}

/// Whether `qs ipc show` lists `target woints` with `list(): string` and
/// `invoke(id: string, action: string): string`.
pub fn has_module(show: &str) -> bool {
    let mut in_target = false;
    let (mut list, mut invoke) = (false, false);
    for line in show.lines() {
        if let Some(name) = line.strip_prefix("target ") {
            in_target = name.trim() == TARGET;
            continue;
        }
        if !in_target {
            continue;
        }
        let f = line.trim();
        list |= f == "function list(): string";
        invoke |= f == "function invoke(id: string, action: string): string";
    }
    list && invoke
}

/// One hintable item, window-relative logical px.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Item {
    pub id: String,
    pub ns: String,
    pub screen: String,
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub actions: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ListReply {
    pub protocol: u32,
    #[serde(default)]
    pub items: Vec<Item>,
}

/// Parse a `list` reply, refusing other protocol versions.
pub fn parse_list(s: &str) -> Result<Vec<Item>, String> {
    let r: ListReply = serde_json::from_str(s.trim()).map_err(|e| {
        let first = s.lines().next().unwrap_or("").trim();
        format!("woints list: {e} (got \"{first}\")")
    })?;
    if r.protocol != PROTOCOL {
        return Err(format!(
            "woints module protocol {} unsupported (expected {PROTOCOL}); update the module or woints",
            r.protocol
        ));
    }
    Ok(r.items)
}

/// Actuator handle of a quickshell element: `qs:<instance id>:<item id>`.
pub fn handle(instance: &str, item: &str) -> String {
    format!("qs:{instance}:{item}")
}

/// Split a [`handle`] into instance id and item id.
pub fn split_handle(h: &str) -> Option<(&str, &str)> {
    h.strip_prefix("qs:")?.split_once(':')
}

/// Elements of `target` (a layer) among `items`: same namespace and screen,
/// rect offset by the layer origin.
pub fn elements_for(target: &Target, instance: &str, items: &[Item]) -> Vec<Element> {
    let Surface::Layer { namespace, .. } = &target.surface else {
        return Vec::new();
    };
    let o = target.rect.origin();
    items
        .iter()
        .filter(|i| i.ns == *namespace && i.screen == target.output.0)
        .filter(|i| i.w >= 1.0 && i.h >= 1.0)
        .map(|i| {
            let actions: Vec<Action> = i.actions.iter().filter_map(|a| a.parse().ok()).collect();
            Element {
                id: ElementId(0),
                rect: Rect::new(
                    o.x + i.x.round() as i32,
                    o.y + i.y.round() as i32,
                    i.w.round() as i32,
                    i.h.round() as i32,
                ),
                role: "quickshell".into(),
                name: i.label.clone(),
                source: ElementSource::Quickshell,
                actions: if actions.is_empty() {
                    vec![Action::Click]
                } else {
                    actions
                },
                caps: vec![],
                confidence: 1.0,
                handle: Some(handle(instance, &i.id)),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use woints_core::geom::OutputId;

    const LIST: &str = include_str!("../tests/fixtures/list.json");
    const SHOW: &str = include_str!("../tests/fixtures/show-no-module.txt");
    const SHOW_MODULE: &str = include_str!("../tests/fixtures/show-module.txt");
    const ITEMS: &str = include_str!("../tests/fixtures/items.json");

    fn bar(x: i32, y: i32) -> Target {
        Target {
            surface: Surface::Layer {
                address: "0x1".into(),
                namespace: "quickshell:bar".into(),
            },
            rect: Rect::new(x, y, 1440, 80),
            output: OutputId("eDP-1".into()),
            pid: Some(574865),
            class: "quickshell:bar".into(),
        }
    }

    #[test]
    fn instances_fixture() {
        let i = parse_instances(LIST).unwrap();
        assert_eq!(i.len(), 2);
        assert!(i[0].config_path.ends_with("lock.qml"));
        assert_eq!(i[1].pid, 574865);
        assert!(parse_instances("").unwrap().is_empty());
    }

    #[test]
    fn show_fixture() {
        assert!(!has_module(SHOW));
        assert!(has_module(SHOW_MODULE));
        // A `woints` target missing `invoke` is not the module.
        assert!(!has_module("target woints\n  function list(): string\n"));
        // Functions of other targets do not count.
        assert!(!has_module(
            "target woints\n  function list(): string\ntarget x\n  function invoke(id: string, action: string): string\n"
        ));
    }

    #[test]
    fn bar_button() {
        let items = parse_list(ITEMS).unwrap();
        let e = elements_for(&bar(0, 0), "hkb", &items);
        assert_eq!(e.len(), 2);
        assert_eq!(e[0].rect, Rect::new(12, 24, 32, 32));
        assert_eq!(e[0].name, "Workspace 1");
        assert_eq!(e[0].actions, [Action::Click, Action::Right]);
        assert_eq!(e[0].source, ElementSource::Quickshell);
        assert_eq!(e[0].handle.as_deref(), Some("qs:hkb:quickshell:bar/1"));
        assert_eq!(
            split_handle(e[0].handle.as_ref().unwrap()),
            Some(("hkb", "quickshell:bar/1"))
        );
        // Unknown action names are dropped.
        assert_eq!(e[1].actions, [Action::Click, Action::Scroll]);
    }

    #[test]
    fn offset_by_layer_origin() {
        let items = parse_list(ITEMS).unwrap();
        let e = elements_for(&bar(1920, 30), "hkb", &items);
        assert_eq!(e[0].rect, Rect::new(1932, 54, 32, 32));
    }

    #[test]
    fn other_window_and_screen_ignored() {
        let items = parse_list(ITEMS).unwrap();
        let e = elements_for(&bar(0, 0), "hkb", &items);
        assert!(e.iter().all(|e| !e.name.contains("Close")));
        assert!(e.iter().all(|e| e.rect.x < 1440));
    }

    #[test]
    fn protocol_checked() {
        assert!(
            parse_list(r#"{"protocol":2,"items":[]}"#)
                .unwrap_err()
                .contains("protocol 2 unsupported")
        );
        assert!(
            parse_list("Target not found.")
                .unwrap_err()
                .contains("Target not found.")
        );
    }
}
