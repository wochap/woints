//! `doctor` report: grouped checks built by the daemon, rendered by the CLI
//! (design D18).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Ok,
    Warn,
    Fail,
}

impl Level {
    pub fn as_str(&self) -> &'static str {
        match self {
            Level::Ok => "ok",
            Level::Warn => "warn",
            Level::Fail => "fail",
        }
    }
}

/// One line of the report.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Check {
    pub level: Level,
    pub message: String,
}

impl Check {
    pub fn ok(m: impl Into<String>) -> Self {
        Self {
            level: Level::Ok,
            message: m.into(),
        }
    }
    pub fn warn(m: impl Into<String>) -> Self {
        Self {
            level: Level::Warn,
            message: m.into(),
        }
    }
    pub fn fail(m: impl Into<String>) -> Self {
        Self {
            level: Level::Fail,
            message: m.into(),
        }
    }
}

/// Every group in report order.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Report {
    /// Daemon pid; environment checks read this process's env.
    pub pid: u32,
    pub environment: Vec<Check>,
    pub a11y: Vec<Check>,
    pub focused: Vec<Check>,
    pub wayland: Vec<Check>,
    pub quickshell: Vec<Check>,
    pub hyprland: Vec<Check>,
    pub config: Vec<Check>,
    #[serde(default)]
    pub onnx: Vec<Check>,
}

impl Report {
    pub fn groups(&self) -> [(&'static str, &[Check]); 8] {
        [
            ("environment", &self.environment),
            ("a11y bus", &self.a11y),
            ("focused window", &self.focused),
            ("wayland", &self.wayland),
            ("quickshell", &self.quickshell),
            ("hyprland", &self.hyprland),
            ("config", &self.config),
            ("onnx", &self.onnx),
        ]
    }

    pub fn failed(&self) -> bool {
        self.groups()
            .iter()
            .any(|(_, c)| c.iter().any(|c| c.level == Level::Fail))
    }
}

/// `model license:` line from `<model>.license` or `LICENSE*` beside the
/// model; absence is a warning.
pub fn model_license(model: &std::path::Path) -> Check {
    let dir = model.parent().unwrap_or(std::path::Path::new("."));
    let mut candidates = vec![model.with_extension("license")];
    if let Ok(rd) = std::fs::read_dir(dir) {
        let mut l: Vec<_> = rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("LICENSE"))
            })
            .collect();
        l.sort();
        candidates.extend(l);
    }
    for c in candidates {
        if let Ok(text) = std::fs::read_to_string(&c) {
            let first = text.lines().map(str::trim).find(|l| !l.is_empty());
            let name = c.file_name().and_then(|n| n.to_str()).unwrap_or("?");
            return Check::ok(format!("model license: {} ({name})", first.unwrap_or("")));
        }
    }
    Check::warn("license: not found (warning)")
}

/// Environment variables relevant to accessibility, in report order.
pub const ENV_VARS: [&str; 5] = [
    "NO_AT_BRIDGE",
    "GTK_A11Y",
    "QT_LINUX_ACCESSIBILITY_ALWAYS_ON",
    "ACCESSIBILITY_ENABLED",
    "GNOME_ACCESSIBILITY",
];

/// Environment group from `(name, value)` lookups. `NO_AT_BRIDGE=1` fails:
/// GTK3 apps then never register on the bus.
pub fn env_checks(get: impl Fn(&str) -> Option<String>) -> Vec<Check> {
    ENV_VARS
        .iter()
        .map(|&k| match (k, get(k).as_deref()) {
            ("NO_AT_BRIDGE", Some("1")) => {
                Check::fail("NO_AT_BRIDGE=1 disables GTK3 accessibility; unset it in your session")
            }
            ("GTK_A11Y", Some("none")) => {
                Check::fail("GTK_A11Y=none disables GTK4 accessibility; unset it")
            }
            ("QT_LINUX_ACCESSIBILITY_ALWAYS_ON", None) => Check::warn(
                "QT_LINUX_ACCESSIBILITY_ALWAYS_ON unset; Qt apps register only when the bus is enabled",
            ),
            ("ACCESSIBILITY_ENABLED", None) => Check::warn(
                "ACCESSIBILITY_ENABLED unset; Electron apps need it (or --force-renderer-accessibility)",
            ),
            (k, Some(v)) => Check::ok(format!("{k}={v}")),
            (k, None) => Check::ok(format!("{k} unset")),
        })
        .collect()
}

/// Accessible tree state of a Chromium window after the wake phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChromiumState {
    /// The frame has children; `usize` usable elements.
    Woken(usize),
    Excluded,
    Disabled,
    TimedOut,
}

impl ChromiumState {
    pub fn check(self) -> Check {
        match self {
            Self::Woken(n) => Check::ok(format!("Chromium tree: woken ({n} elements)")),
            Self::Disabled => Check::warn("Chromium tree: dormant (wake disabled)"),
            Self::Excluded => Check::warn("Chromium tree: dormant (class excluded)"),
            Self::TimedOut => Check::fail(
                "Chromium tree: dormant after wake; the app may run with --disable-renderer-accessibility",
            ),
        }
    }
}

/// Focused-window lines from the AT-SPI facts collected for it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FocusedFacts {
    pub class: String,
    pub pid: Option<u32>,
    /// `GTK 4.20.1`, `Qt 6.11.0`, ...
    pub toolkit: Option<String>,
    /// Pairing reason when an a11y app matched.
    pub paired: Option<String>,
    pub collection: Option<bool>,
    /// Frames (direct children) under the app root.
    pub root_children: Option<usize>,
    pub collection_count: Option<usize>,
    pub walk_count: Option<usize>,
    /// The application maps WebKitGTK.
    pub webkit: bool,
    pub web_roots: Option<usize>,
    pub web_count: Option<usize>,
    pub usable: usize,
    /// Set when the toolkit is Chromium.
    pub chromium: Option<ChromiumState>,
    pub vision: (bool, String),
}

pub fn focused_checks(f: &FocusedFacts) -> Vec<Check> {
    let mut out = vec![Check::ok(format!(
        "class {} pid {}",
        f.class,
        f.pid.map_or("?".into(), |p| p.to_string())
    ))];
    let tk = f.toolkit.clone().unwrap_or_default();
    let gtk4 = tk.starts_with("GTK 4");
    let qt = tk.starts_with("Qt");
    match &f.paired {
        Some(p) => out.push(Check::ok(format!("a11y app matched ({p}), toolkit {tk}"))),
        None => out.push(Check::warn(
            "no a11y app matched; see the toolkit table in the README",
        )),
    }
    if qt && f.root_children == Some(0) {
        out.push(Check::warn(
            "Qt QML window: not reachable from AT-SPI root; use the Woints quickshell module or vision",
        ));
    }
    match f.collection {
        Some(true) => out.push(Check::ok(format!(
            "Collection present: {} elements",
            f.collection_count.unwrap_or(0)
        ))),
        Some(false) if gtk4 => out.push(Check::ok("Collection absent (expected for GTK4)")),
        Some(false) => out.push(Check::warn("Collection absent; tree walk used")),
        None => {}
    }
    if let Some(n) = f.walk_count {
        out.push(Check::ok(format!("walk: {n} elements")));
    }
    if f.webkit {
        out.push(webkit_check(
            f.web_roots.unwrap_or(0),
            f.web_count.unwrap_or(0),
        ));
    }
    if let Some(st) = f.chromium {
        out.push(st.check());
    }
    if f.paired.is_some() {
        let usable = format!("{} elements with usable extents", f.usable);
        out.push(if f.usable == 0 {
            Check::warn(usable)
        } else {
            Check::ok(usable)
        });
    }
    let (run, reason) = &f.vision;
    out.push(Check::ok(format!(
        "vision {} ({reason})",
        if *run { "would run" } else { "would not run" }
    )));
    out
}

fn webkit_check(roots: usize, elements: usize) -> Check {
    let plural = |n: usize, w: &str| format!("{n} {w}{}", if n == 1 { "" } else { "s" });
    let m = format!(
        "WebKitGTK: {}, {}",
        plural(roots, "web root"),
        plural(elements, "web element")
    );
    if roots == 0 {
        Check::warn(m)
    } else {
        Check::ok(m)
    }
}

/// Wayland globals woints uses; the first two are blocking.
pub const GLOBALS: [(&str, bool); 6] = [
    ("zwlr_layer_shell_v1", true),
    ("zwlr_virtual_pointer_manager_v1", true),
    ("zwlr_screencopy_manager_v1", false),
    ("hyprland_toplevel_export_manager_v1", false),
    ("wp_fractional_scale_manager_v1", false),
    ("wp_viewporter", false),
];

pub fn wayland_checks(advertised: &[String]) -> Vec<Check> {
    GLOBALS
        .iter()
        .map(|&(g, blocking)| {
            if advertised.iter().any(|a| a == g) {
                Check::ok(g)
            } else if blocking {
                Check::fail(format!("{g} missing"))
            } else {
                Check::warn(format!("{g} missing"))
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_at_bridge_fails() {
        let c = env_checks(|k| (k == "NO_AT_BRIDGE").then(|| "1".into()));
        assert_eq!(
            c[0],
            Check::fail("NO_AT_BRIDGE=1 disables GTK3 accessibility; unset it in your session")
        );
        let r = Report {
            environment: c,
            ..Default::default()
        };
        assert!(r.failed());
    }

    #[test]
    fn healthy_env_passes() {
        let c = env_checks(|k| match k {
            "QT_LINUX_ACCESSIBILITY_ALWAYS_ON" | "ACCESSIBILITY_ENABLED" => Some("1".into()),
            _ => None,
        });
        assert!(c.iter().all(|c| c.level == Level::Ok));
    }

    #[test]
    fn gtk4_collection_absent_is_ok() {
        let c = focused_checks(&FocusedFacts {
            class: "org.gnome.Nautilus".into(),
            toolkit: Some("GTK 4.20.1".into()),
            paired: Some("pid".into()),
            collection: Some(false),
            walk_count: Some(40),
            usable: 38,
            ..Default::default()
        });
        assert!(c.contains(&Check::ok("Collection absent (expected for GTK4)")));
        assert!(c.contains(&Check::ok("walk: 40 elements")));
    }

    #[test]
    fn qt_qml_window_warns() {
        let c = focused_checks(&FocusedFacts {
            toolkit: Some("Qt 6.11.0".into()),
            paired: Some("pid".into()),
            root_children: Some(0),
            ..Default::default()
        });
        assert!(c.contains(&Check::warn(
            "Qt QML window: not reachable from AT-SPI root; use the Woints quickshell module or vision"
        )));
    }

    #[test]
    fn webkit_line() {
        let f = FocusedFacts {
            toolkit: Some("GTK 3.24.52".into()),
            paired: Some("pid".into()),
            webkit: true,
            web_roots: Some(1),
            web_count: Some(11),
            ..Default::default()
        };
        assert!(focused_checks(&f).contains(&Check::ok("WebKitGTK: 1 web root, 11 web elements")));
        let none = FocusedFacts {
            web_roots: Some(0),
            web_count: Some(0),
            ..f.clone()
        };
        assert!(
            focused_checks(&none).contains(&Check::warn("WebKitGTK: 0 web roots, 0 web elements"))
        );
        let plain = FocusedFacts { webkit: false, ..f };
        assert!(
            !focused_checks(&plain)
                .iter()
                .any(|c| c.message.starts_with("WebKitGTK"))
        );
    }

    #[test]
    fn chromium_lines() {
        let at = |st| {
            focused_checks(&FocusedFacts {
                toolkit: Some("Chromium 150".into()),
                paired: Some("pid".into()),
                chromium: Some(st),
                ..Default::default()
            })
        };
        assert!(
            at(ChromiumState::Woken(165))
                .contains(&Check::ok("Chromium tree: woken (165 elements)"))
        );
        assert!(
            at(ChromiumState::Excluded)
                .contains(&Check::warn("Chromium tree: dormant (class excluded)"))
        );
        assert!(
            at(ChromiumState::Disabled)
                .contains(&Check::warn("Chromium tree: dormant (wake disabled)"))
        );
        let t = at(ChromiumState::TimedOut);
        assert!(
            t.iter().any(|c| c.level == Level::Fail
                && c.message.contains("--disable-renderer-accessibility"))
        );
    }

    #[test]
    fn missing_globals() {
        let c = wayland_checks(&["zwlr_layer_shell_v1".into()]);
        assert_eq!(c[0].level, Level::Ok);
        assert_eq!(c[1].level, Level::Fail);
        assert_eq!(c[2].level, Level::Warn);
    }

    #[test]
    fn json_groups() {
        let v = serde_json::to_value(Report::default()).unwrap();
        for k in [
            "environment",
            "a11y",
            "focused",
            "wayland",
            "quickshell",
            "hyprland",
            "config",
        ] {
            assert!(v.get(k).is_some(), "{k}");
        }
    }

    #[test]
    fn license_beside_model() {
        let dir = std::env::temp_dir().join(format!("woints-lic-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let m = dir.join("ui.onnx");
        assert_eq!(model_license(&m).level, Level::Warn);
        std::fs::write(dir.join("LICENSE"), "Apache License\nVersion 2.0\n").unwrap();
        assert_eq!(
            model_license(&m).message,
            "model license: Apache License (LICENSE)"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
