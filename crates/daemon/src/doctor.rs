//! `doctor` report builder (design D18). Subprocesses (`gsettings`, `qs`)
//! run only here, never on the hints path except `qs ipc`.

use std::time::{Duration, Instant};

use woints_core::config::ProviderMode;
use woints_core::doctor::{
    Check, FocusedFacts, Report, env_checks, focused_checks, wayland_checks,
};
use woints_core::ports::Budget;

use crate::daemon::{Daemon, Provider};

/// Hyprland version woints was verified against.
const VERIFIED_HYPRLAND: &str = "0.56.2";

impl Daemon {
    pub async fn doctor(&self) -> Report {
        let (a11y, focused, wayland, quickshell, hyprland) = tokio::join!(
            self.a11y_checks(),
            self.focused_checks(),
            self.wayland_checks(),
            self.quickshell_checks(),
            self.hyprland_checks(),
        );
        Report {
            pid: std::process::id(),
            environment: env_checks(|k| std::env::var(k).ok()),
            a11y,
            focused,
            wayland,
            quickshell,
            hyprland,
            config: self.config_checks().await,
            onnx: self.onnx_checks(),
        }
    }

    async fn a11y_checks(&self) -> Vec<Check> {
        let info = woints_a11y::bus_info().await;
        let mut out = vec![match &info.address {
            Ok(a) => Check::ok(format!("a11y bus reachable at {a}")),
            Err(e) => Check::fail(format!("a11y bus unreachable: {e}")),
        }];
        out.push(match info.is_enabled {
            Some(true) => Check::ok("org.a11y.Status.IsEnabled = true"),
            Some(false) => Check::warn(
                "org.a11y.Status.IsEnabled = false; GTK3 apps may not register \
                 (busctl --user set-property org.a11y.Bus /org/a11y/bus org.a11y.Status IsEnabled b true)",
            ),
            None => Check::warn("org.a11y.Status.IsEnabled unreadable"),
        });
        let gs = woints_qs::proc::run(
            "gsettings",
            &[
                "get",
                "org.gnome.desktop.interface",
                "toolkit-accessibility",
            ],
            Duration::from_secs(2),
        )
        .await;
        out.push(match gs.as_deref().map(str::trim) {
            Ok("true") => Check::ok("gsettings toolkit-accessibility = true"),
            Ok(v) => Check::warn(format!(
                "gsettings toolkit-accessibility = {v}; \
                 gsettings set org.gnome.desktop.interface toolkit-accessibility true"
            )),
            Err(e) => Check::warn(format!("gsettings unavailable: {e}")),
        });
        out
    }

    async fn focused_checks(&self) -> Vec<Check> {
        let Some(target) = self.active_target().await else {
            return vec![Check::ok("no focused window")];
        };
        let budget = || Budget {
            deadline: Instant::now() + Duration::from_millis(500),
        };
        match &self.provider {
            Provider::Demo(_) => vec![Check::ok(format!(
                "class {}: demo provider (WOINTS_DEMO=1)",
                target.class
            ))],
            Provider::Atspi(p) => match p.diagnose(&target, budget()).await {
                Ok(d) => {
                    let vision = self.vision_decision(d.usable, &target).await;
                    focused_checks(&FocusedFacts {
                        class: target.class.clone(),
                        pid: target.pid,
                        toolkit: d.toolkit,
                        paired: d.paired,
                        collection: d.collection,
                        root_children: d.root_children,
                        collection_count: d.collection_count,
                        walk_count: d.walk_count,
                        usable: d.usable,
                        vision,
                    })
                }
                Err(e) => vec![Check::warn(format!(
                    "class {}: AT-SPI unavailable: {e}",
                    target.class
                ))],
            },
        }
    }

    async fn wayland_checks(&self) -> Vec<Check> {
        match self.overlay_globals().await {
            Ok(g) => wayland_checks(&g),
            Err(e) => vec![Check::fail(format!("Wayland connection unavailable: {e}"))],
        }
    }

    async fn quickshell_checks(&self) -> Vec<Check> {
        let Some(q) = &self.quickshell else {
            return vec![Check::ok("provider not built in (demo mode)")];
        };
        if self.settings().config.providers.quickshell.mode == ProviderMode::Off {
            return vec![Check::ok(
                "provider disabled (providers.quickshell.mode = \"off\")",
            )];
        }
        match q.doctor().await {
            Err(e) => vec![Check::warn(format!(
                "qs unavailable ({e}); quickshell layers fall back to vision"
            ))],
            Ok(list) if list.is_empty() => vec![Check::ok("no quickshell instances running")],
            Ok(list) => list
                .into_iter()
                .map(|i| {
                    let head = format!("{} (pid {}, {})", i.id, i.pid, i.config_path);
                    match (i.module, i.items) {
                        (true, Ok(n)) => Check::ok(format!("{head}: Woints module, {n} items")),
                        (true, Err(e)) => {
                            Check::warn(format!("{head}: Woints module, list failed: {e}"))
                        }
                        (false, _) => Check::warn(format!(
                            "{head}: Woints module not imported; its layers use vision"
                        )),
                    }
                })
                .collect(),
        }
    }

    async fn hyprland_checks(&self) -> Vec<Check> {
        if self.hypr.is_none() {
            return vec![Check::fail(
                "not running under Hyprland (HYPRLAND_INSTANCE_SIGNATURE unset)",
            )];
        }
        vec![match self.hyprland_version_pub().await {
            Some(v) if v == VERIFIED_HYPRLAND => Check::ok(format!("Hyprland {v}")),
            Some(v) => Check::warn(format!(
                "Hyprland {v}; verified against {VERIFIED_HYPRLAND}"
            )),
            None => Check::warn("Hyprland version unknown"),
        }]
    }

    /// ONNX detector section (M6).
    fn onnx_checks(&self) -> Vec<Check> {
        #[cfg(feature = "vision")]
        {
            if !woints_vision::ONNX_COMPILED {
                return vec![Check::ok(woints_vision::NOT_COMPILED)];
            }
            let v = self.settings().config.vision_config();
            let mut out = vec![
                Check::ok("feature: yes"),
                Check::ok(format!("detector: {}", v.detector.as_str())),
            ];
            let slot = self.onnx_slot();
            match &slot.state {
                None => out.push(Check::ok(
                    "model: not used (vision.detector = \"classical\")",
                )),
                Some(Err(e)) => {
                    out.push(Check::fail(format!("model: {e}")));
                    if v.onnx.model_path.is_empty() {
                        return out;
                    }
                }
                Some(Ok((_, s))) => {
                    out.push(Check::ok(format!("model: {} ok", s.path)));
                    let shape: Vec<String> = s.input_shape.iter().map(i64::to_string).collect();
                    out.push(Check::ok(format!("input: [{}]", shape.join(","))));
                    out.push(Check::ok(format!("outputs: {}", s.output_names.join(", "))));
                    let ep = format!(
                        "ep: {} (requested {})",
                        s.ep_active.as_str(),
                        s.ep_requested.as_str()
                    );
                    out.push(if s.ep_active == s.ep_requested {
                        Check::ok(ep)
                    } else {
                        Check::warn(ep)
                    });
                    out.push(Check::ok(format!("warmup: {:.0} ms", s.warmup_ms)));
                }
            }
            if slot.state.is_some() {
                out.push(woints_core::doctor::model_license(&v.onnx.model_path()));
            }
            out
        }
        #[cfg(not(feature = "vision"))]
        vec![Check::ok("onnx: not compiled in (built without vision)")]
    }

    async fn config_checks(&self) -> Vec<Check> {
        let s = self.config_status().await;
        let mut out = vec![Check::ok(match &s.path {
            Some(p) if s.source == "defaults" => format!("{p}: not loaded, using defaults"),
            Some(p) => format!("loaded {p}"),
            None => "no config file; built-in defaults".into(),
        })];
        out.extend(
            s.errors
                .iter()
                .map(|e| Check::fail(format!("config error: {e}"))),
        );
        out.extend(
            s.warnings
                .iter()
                .map(|w| Check::warn(format!("config: {w}"))),
        );
        out
    }
}
