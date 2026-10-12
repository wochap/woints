//! `woints`: thin client of the `wointsd` JSON-lines socket.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::process::ExitCode;
use std::time::Duration;

use clap::{Parser, Subcommand};
use serde_json::Value;

use woints_core::act::Axis;
use woints_core::doctor::Level;
use woints_core::model::{Action, ElementId, Scope};
use woints_core::proto::{
    ActReply, BenchReply, DoctorReply, ReloadReply, Request, Response, SnapshotReply, StatusReply,
    socket_path,
};

#[derive(Parser)]
#[command(name = "woints", version, about = "Keyboard hints for Hyprland")]
struct Cli {
    /// Print the raw JSON response.
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Show hints.
    Hints {
        /// focused | focused+layers | layers | all (default: `scope.default`)
        #[arg(long)]
        scope: Option<Scope>,
        /// click | right | middle | double | hover | focus | drag | scroll | copy
        #[arg(long)]
        action: Option<Action>,
    },
    /// Hide hints.
    Cancel,
    /// Daemon state, outputs and last timings.
    Status,
    /// Stop the daemon.
    Quit,
    /// Targets and elements as JSON, without showing the overlay.
    Snapshot {
        #[arg(long)]
        scope: Option<Scope>,
    },
    /// Act on an element of a snapshot without showing the overlay.
    Act {
        #[arg(long)]
        snapshot: u64,
        #[arg(long)]
        id: u32,
        /// click | right | middle | double | hover | focus | drag | scroll | copy
        #[arg(long, default_value = "click")]
        action: Action,
        /// Drag destination element id.
        #[arg(long)]
        to: Option<u32>,
        /// Scroll axis: v | h (vertical | horizontal).
        #[arg(long)]
        axis: Option<Axis>,
        /// Scroll lines; negative scrolls up or left.
        #[arg(long, allow_negative_numbers = true)]
        amount: Option<i32>,
    },
    /// Run the pipeline N times (no overlay) and print p50/p95 per stage.
    Bench {
        #[arg(short = 'n', long, default_value_t = 10)]
        iterations: u32,
        #[arg(long)]
        scope: Option<Scope>,
    },
    /// Re-read the config file (same as SIGHUP).
    Reload,
    /// Diagnose accessibility, Wayland, quickshell and config; exit 1 on failures.
    Doctor,
}

/// The request for `cmd` and the reply timeout in seconds.
fn to_request(cmd: Cmd) -> (Request, u64) {
    match cmd {
        Cmd::Hints { scope, action } => (Request::Hints { scope, action }, 5),
        Cmd::Cancel => (Request::Cancel, 5),
        Cmd::Status => (Request::Status, 5),
        Cmd::Quit => (Request::Quit, 5),
        Cmd::Snapshot { scope } => (Request::Snapshot { scope }, 5),
        Cmd::Bench { iterations, scope } => (Request::Bench { iterations, scope }, 300),
        Cmd::Reload => (Request::Reload, 5),
        Cmd::Doctor => (Request::Doctor, 30),
        Cmd::Act {
            snapshot,
            id,
            action,
            to,
            axis,
            amount,
        } => (
            Request::Act {
                snapshot_id: snapshot,
                id: ElementId(id),
                action,
                to: to.map(ElementId),
                axis,
                amount,
            },
            10,
        ),
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let (req, timeout) = to_request(cli.cmd);
    let path = socket_path();
    let Ok(mut stream) = UnixStream::connect(&path) else {
        eprintln!("wointsd is not running (socket: {})", path.display());
        return ExitCode::from(2);
    };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(timeout)));
    let mut line = serde_json::to_string(&req).expect("request serialises");
    line.push('\n');
    if stream.write_all(line.as_bytes()).is_err() {
        eprintln!("wointsd is not running (socket: {})", path.display());
        return ExitCode::from(2);
    }
    let mut reply = String::new();
    if BufReader::new(&stream).read_line(&mut reply).is_err() || reply.is_empty() {
        eprintln!("woints: no reply from wointsd");
        return ExitCode::from(2);
    }
    let resp: Response = match serde_json::from_str(&reply) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("woints: bad reply: {e}");
            return ExitCode::from(1);
        }
    };
    if cli.json {
        print!("{reply}");
    } else if resp.ok {
        print_human(&req, &resp);
    }
    if !resp.ok {
        eprintln!("woints: {}", resp.error.as_deref().unwrap_or("error"));
        return ExitCode::from(1);
    }
    if req == Request::Doctor
        && resp
            .payload::<DoctorReply>()
            .is_ok_and(|d| d.report.failed())
    {
        return ExitCode::from(1);
    }
    ExitCode::SUCCESS
}

fn print_human(req: &Request, resp: &Response) {
    match req {
        Request::Status => match resp.payload::<StatusReply>() {
            Ok(s) => {
                println!("state     {:?}", s.state);
                println!("version   {}", s.version);
                println!("uptime    {:.1}s", s.uptime_ms as f64 / 1000.0);
                println!("hyprland  {}", s.hyprland_version.as_deref().unwrap_or("?"));
                println!(
                    "overlay   {}",
                    if s.overlay { "running" } else { "unavailable" }
                );
                println!("pointer   {}", s.pointer);
                if !s.keys.is_empty() {
                    println!("keys      {}", s.keys);
                }
                if !s.detector.is_empty() {
                    println!("detector  {}", s.detector);
                }
                if let Some(o) = &s.onnx {
                    println!("onnx      {o}");
                }
                println!("clipboard {}", s.clipboard);
                for o in &s.outputs {
                    let r = o.rect;
                    let f = if o.focused { " *" } else { "" };
                    println!(
                        "output    {} {}x{}+{}+{} @ {}{f}",
                        o.name, r.w, r.h, r.x, r.y, o.scale
                    );
                }
                for p in &s.providers {
                    let state = if p.available {
                        "available"
                    } else {
                        "unavailable"
                    };
                    match &p.last_error {
                        Some(e) => println!("provider  {}: {state} ({e})", p.name),
                        None => println!("provider  {}: {state}", p.name),
                    }
                }
                for r in &s.last_collect {
                    println!(
                        "collect   {} paired: {} used: {} truncated: {}{}",
                        r.provider,
                        r.paired.as_deref().unwrap_or("-"),
                        r.used.as_deref().unwrap_or("-"),
                        r.truncated,
                        if r.reasons.is_empty() {
                            String::new()
                        } else {
                            format!(" ({})", r.reasons.join(", "))
                        }
                    );
                }
                if let Some(a) = &s.last_action {
                    println!(
                        "action    {} on {}: {} ({:.1} ms)",
                        a.action,
                        a.element.0,
                        if a.ok {
                            "ok"
                        } else {
                            a.error.as_deref().unwrap_or("failed")
                        },
                        a.ms
                    );
                }
                println!(
                    "config    {}{}",
                    s.config.source,
                    if s.config.errors.is_empty() {
                        String::new()
                    } else {
                        format!(" (last reload failed: {})", s.config.errors.join("; "))
                    }
                );
                println!("scheme    {} ({})", s.scheme.as_str(), s.source.as_str());
                if !s.last_timings.0.is_empty() {
                    println!("last hints:");
                    for t in &s.last_timings.0 {
                        println!("  {:<16} {:>8.3} ms", t.stage, t.ms);
                    }
                    println!("  {:<16} {:>8.3} ms", "total", s.last_timings.total_ms());
                }
            }
            Err(_) => println!("{}", Value::Object(resp.payload.clone())),
        },
        Request::Bench { .. } => match resp.payload::<BenchReply>() {
            Ok(b) => {
                println!("{} iterations", b.iterations);
                println!("{:<16} {:>9} {:>9}", "stage", "p50 ms", "p95 ms");
                for r in &b.rows {
                    println!("{:<16} {:>9.3} {:>9.3}", r.stage, r.p50_ms, r.p95_ms);
                }
            }
            Err(_) => println!("{}", Value::Object(resp.payload.clone())),
        },
        Request::Snapshot { .. } => {
            if let Ok(s) = resp.payload::<SnapshotReply>() {
                println!(
                    "snapshot {}: {} targets, {} elements",
                    s.snapshot_id,
                    s.targets.len(),
                    s.elements.len()
                );
                println!(
                    "vision: {} ({})",
                    if s.vision.ran { "ran" } else { "skipped" },
                    s.vision.reason
                );
            }
        }
        Request::Reload => match resp.payload::<ReloadReply>() {
            Ok(r) => {
                println!("reloaded {}", r.config.source);
                for w in &r.config.warnings {
                    println!("warn  {w}");
                }
            }
            Err(_) => println!("ok"),
        },
        Request::Doctor => match resp.payload::<DoctorReply>() {
            Ok(d) => print!("{}", render_doctor(&d.report)),
            Err(e) => println!("bad doctor reply: {e}"),
        },
        Request::Act { .. } => match resp.payload::<ActReply>() {
            Ok(a) => println!("performed {}", a.performed),
            Err(_) => println!("ok"),
        },
        _ => {
            let parts: Vec<String> = resp
                .payload
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect();
            if parts.is_empty() {
                println!("ok");
            } else {
                println!("{}", parts.join(" "));
            }
        }
    }
}

/// Grouped `ok/warn/fail` lines.
fn render_doctor(r: &woints_core::doctor::Report) -> String {
    let mut out = format!("wointsd pid {} (environment is the daemon's)\n", r.pid);
    for (name, checks) in r.groups() {
        out.push_str(&format!("\n{name}\n"));
        for c in checks {
            out.push_str(&format!("  {:<5} {}\n", c.level.as_str(), c.message));
        }
    }
    let n = |l: Level| {
        r.groups()
            .iter()
            .flat_map(|(_, c)| c.iter())
            .filter(|c| c.level == l)
            .count()
    };
    out.push_str(&format!(
        "\n{} ok, {} warn, {} fail\n",
        n(Level::Ok),
        n(Level::Warn),
        n(Level::Fail)
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use woints_core::doctor::{Check, Report};

    #[test]
    fn doctor_rendering() {
        let r = Report {
            pid: 7,
            environment: vec![Check::fail(
                "NO_AT_BRIDGE=1 disables GTK3 accessibility; unset it in your session",
            )],
            config: vec![Check::ok("no config file; built-in defaults")],
            ..Default::default()
        };
        let s = render_doctor(&r);
        assert!(s.contains(
            "  fail  NO_AT_BRIDGE=1 disables GTK3 accessibility; unset it in your session\n"
        ));
        assert!(s.ends_with("1 ok, 0 warn, 1 fail\n"));
        assert!(r.failed());
    }

    #[test]
    fn new_commands() {
        assert_eq!(
            wire(&["woints", "reload"]),
            serde_json::json!({"cmd":"reload"})
        );
        assert_eq!(
            wire(&["woints", "doctor"]),
            serde_json::json!({"cmd":"doctor"})
        );
        assert_eq!(
            wire(&["woints", "hints"]),
            serde_json::json!({"cmd":"hints"})
        );
        assert_eq!(
            wire(&["woints", "hints", "--scope", "focused+layers"]),
            serde_json::json!({"cmd":"hints","scope":"focused+layers"})
        );
    }

    fn wire(args: &[&str]) -> Value {
        let cli = Cli::try_parse_from(args).unwrap();
        serde_json::to_value(to_request(cli.cmd).0).unwrap()
    }

    #[test]
    fn hints_action() {
        assert_eq!(
            wire(&["woints", "hints", "--action", "drag"]),
            serde_json::json!({"cmd":"hints","action":"drag"})
        );
    }

    #[test]
    fn act_drag_and_scroll() {
        assert_eq!(
            wire(&[
                "woints",
                "act",
                "--snapshot",
                "7",
                "--id",
                "3",
                "--action",
                "drag",
                "--to",
                "9"
            ]),
            serde_json::json!({"cmd":"act","snapshot_id":7,"id":3,"action":"drag","to":9})
        );
        assert_eq!(
            wire(&[
                "woints",
                "act",
                "--snapshot",
                "7",
                "--id",
                "3",
                "--action",
                "scroll",
                "--axis",
                "v",
                "--amount",
                "-3"
            ]),
            serde_json::json!({"cmd":"act","snapshot_id":7,"id":3,"action":"scroll","axis":"vertical","amount":-3})
        );
        assert!(Cli::try_parse_from(["woints", "hints", "--action", "teleport"]).is_err());
    }

    /// `name)` arms of the `case "$subcmd" in` block, keyed by each name.
    fn completion_arms(src: &str) -> std::collections::BTreeMap<String, String> {
        let block = src
            .split_once("case \"$subcmd\" in")
            .expect("subcommand case block")
            .1
            .split_once("esac")
            .expect("esac")
            .0;
        let mut arms = std::collections::BTreeMap::new();
        for chunk in block.split(";;") {
            let Some((names, body)) = chunk.trim().split_once(')') else {
                continue;
            };
            for name in names.split('|') {
                arms.insert(name.trim().to_owned(), body.to_owned());
            }
        }
        arms
    }

    /// Every `--word` flag in `text`, plus `-x` short flags written as the
    /// `{-x,--word}` alias pairs of `_arguments` specs.
    fn flags_in(text: &str) -> std::collections::BTreeSet<String> {
        let mut out = std::collections::BTreeSet::new();
        for (i, _) in text.match_indices("--") {
            let word: String = text[i + 2..]
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '-')
                .collect();
            if !word.is_empty() && !text[..i].ends_with('-') {
                out.insert(format!("--{word}"));
            }
        }
        for (i, _) in text.match_indices("{-") {
            if let Some(c) = text[i + 2..]
                .chars()
                .next()
                .filter(char::is_ascii_alphanumeric)
            {
                out.insert(format!("-{c}"));
            }
        }
        out
    }

    /// Long and short flags of `cmd`, built-in `--help`/`--version` included.
    fn clap_flags(cmd: &clap::Command) -> std::collections::BTreeSet<String> {
        let mut out = std::collections::BTreeSet::new();
        for a in cmd.get_arguments() {
            out.extend(a.get_long().map(|l| format!("--{l}")));
            out.extend(a.get_short().map(|s| format!("-{s}")));
        }
        out
    }

    #[test]
    fn completions_match_cli() {
        use clap::CommandFactory;
        let raw = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../integrations/zsh/_woints"
        ));
        let src: String = raw
            .lines()
            .map(|l| match l.find('#') {
                Some(i) if l[..i].trim().is_empty() || l[..i].ends_with(' ') => &l[..i],
                _ => l,
            })
            .collect::<Vec<_>>()
            .join("\n");
        let mut cli = Cli::command();
        cli.build();

        let (root, rest) = src.split_once("case \"$subcmd\" in").unwrap();
        let common = src
            .split_once("common=(")
            .and_then(|(_, r)| r.split_once("\n  )"))
            .map(|(c, _)| c)
            .expect("common=( block");
        let root_flags = flags_in(root.split_once("_woints() {").unwrap().1);
        assert_eq!(root_flags, clap_flags(&cli), "root flags");
        let common = flags_in(common);

        let menu = src
            .split_once("commands=(")
            .and_then(|(_, r)| r.split_once("\n  )"))
            .unwrap()
            .0;
        let menu: std::collections::BTreeSet<_> = menu
            .lines()
            .filter_map(|l| l.trim().strip_prefix('\'')?.split_once(':'))
            .map(|(n, _)| n.to_owned())
            .collect();
        let arms = completion_arms(&format!("case \"$subcmd\" in{rest}"));
        let subs: std::collections::BTreeSet<_> = cli
            .get_subcommands()
            .map(|c| c.get_name().to_owned())
            .collect();
        assert_eq!(menu, subs, "commands=( menu");
        assert_eq!(
            arms.keys()
                .cloned()
                .collect::<std::collections::BTreeSet<_>>(),
            subs,
            "case arms"
        );

        for sub in cli.get_subcommands() {
            let name = sub.get_name();
            let want = clap_flags(sub);
            let mut have = flags_in(&arms[name]);
            if arms[name].contains("$common") {
                have.extend(common.iter().cloned());
            }
            if let Some(f) = want.difference(&have).next() {
                panic!("{name} {f} missing from the completion file");
            }
            if let Some(f) = have.difference(&want).next() {
                panic!("{name} {f} in the completion file is not a clap flag");
            }
        }

        let values = |helper: &str| -> Vec<String> {
            let body = src
                .split_once(&format!("{helper}() {{"))
                .and_then(|(_, r)| r.split_once("\n}"))
                .unwrap_or_else(|| panic!("{helper} missing"))
                .0;
            body.lines()
                .filter_map(|l| l.trim().strip_prefix('\'')?.split_once(':'))
                .map(|(v, _)| v.to_owned())
                .collect()
        };
        let scopes = [
            Scope::Focused,
            Scope::FocusedAndLayers,
            Scope::Layers,
            Scope::All,
        ];
        assert_eq!(
            values("_woints_scopes"),
            scopes.map(|s| s.as_str().to_owned())
        );
        for v in values("_woints_scopes") {
            assert!(v.parse::<Scope>().is_ok(), "scope {v}");
        }
        assert_eq!(
            values("_woints_actions"),
            Action::ALL.map(|a| a.as_str().to_owned())
        );
        let axes = values("_woints_axes");
        assert_eq!(axes, ["v", "h", "vertical", "horizontal"]);
        for v in axes {
            assert!(v.parse::<Axis>().is_ok(), "axis {v}");
        }
    }
}
