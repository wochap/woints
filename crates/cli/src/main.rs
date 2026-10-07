//! `woints`: thin client of the `wointsd` JSON-lines socket.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::process::ExitCode;
use std::time::Duration;

use clap::{Parser, Subcommand};
use serde_json::Value;

use woints_core::model::{ActionKind, ElementId, Scope};
use woints_core::proto::{
    ActReply, BenchReply, Request, Response, SnapshotReply, StatusReply, socket_path,
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
        #[arg(long, default_value = "focused")]
        scope: Scope,
        /// activate | focus | left | right | middle | double | hover
        #[arg(long)]
        action: Option<ActionKind>,
    },
    /// Hide hints.
    Cancel,
    /// Daemon state, outputs and last timings.
    Status,
    /// Stop the daemon.
    Quit,
    /// Targets and elements as JSON, without showing the overlay.
    Snapshot {
        #[arg(long, default_value = "focused")]
        scope: Scope,
    },
    /// Act on an element of a snapshot without showing the overlay.
    Act {
        #[arg(long)]
        snapshot: u64,
        #[arg(long)]
        id: u32,
        /// activate | focus | left | right | middle | double | hover
        #[arg(long, default_value = "activate")]
        action: ActionKind,
    },
    /// Run the pipeline N times (no overlay) and print p50/p95 per stage.
    Bench {
        #[arg(short = 'n', long, default_value_t = 10)]
        iterations: u32,
        #[arg(long, default_value = "focused")]
        scope: Scope,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let (req, timeout) = match cli.cmd {
        Cmd::Hints { scope, action } => (Request::Hints { scope, action }, 5),
        Cmd::Cancel => (Request::Cancel, 5),
        Cmd::Status => (Request::Status, 5),
        Cmd::Quit => (Request::Quit, 5),
        Cmd::Snapshot { scope } => (Request::Snapshot { scope }, 5),
        Cmd::Bench { iterations, scope } => (Request::Bench { iterations, scope }, 300),
        Cmd::Act {
            snapshot,
            id,
            action,
        } => (
            Request::Act {
                snapshot_id: snapshot,
                id: ElementId(id),
                action,
            },
            10,
        ),
    };
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
            }
        }
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
