//! `wointsd`: resident daemon owning the socket, Hyprland IPC and the overlay.

mod daemon;
mod doctor;
mod keys;
mod pipeline;
mod scheme;
mod server;
mod session;
mod store;

use std::sync::Arc;
use std::time::Duration;

use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::mpsc;

use woints_core::ports::{OverlayEvent, OverlayKeys};
use woints_core::proto::socket_path;
use woints_hypr::{Event, Hypr};

use crate::daemon::Daemon;
use crate::server::BindError;
use crate::session::Input;

#[tokio::main]
async fn main() -> std::process::ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with_target(false)
        .init();

    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--version" || a == "-V") {
        println!("wointsd {}", env!("CARGO_PKG_VERSION"));
        return std::process::ExitCode::SUCCESS;
    }
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("usage: wointsd [--config PATH] [--print-submap]");
        return std::process::ExitCode::SUCCESS;
    }
    if args.iter().any(|a| a == "--print-submap") {
        return print_submap(&args).await;
    }
    let config_path = match args.iter().position(|a| a == "--config") {
        Some(i) => match args.get(i + 1) {
            Some(p) => Some(std::path::PathBuf::from(p)),
            None => {
                eprintln!("wointsd: --config needs a path");
                return std::process::ExitCode::from(2);
            }
        },
        None => woints_core::config::default_path(),
    };

    #[cfg(feature = "vision")]
    if let Some(p) = &config_path
        && let Ok((l, _)) = woints_core::config::load(p)
        && let Err(e) = woints_vision::check_config(&l.config.vision)
    {
        eprintln!("wointsd: {e}");
        return std::process::ExitCode::from(1);
    }

    let path = socket_path();
    let listener = match server::bind(&path).await {
        Ok(l) => l,
        Err(BindError::AlreadyRunning) => {
            eprintln!("wointsd already running");
            return std::process::ExitCode::from(1);
        }
        Err(e) => {
            eprintln!("wointsd: cannot bind {}: {e}", path.display());
            return std::process::ExitCode::from(1);
        }
    };
    tracing::info!(socket = %path.display(), "listening");

    let hypr = match Hypr::from_env() {
        Ok(h) => Some(h),
        Err(e) => {
            tracing::warn!("{e}; hints will fail until restarted under Hyprland");
            None
        }
    };
    let (daemon, overlay_rx) = Daemon::new(hypr.clone(), config_path);
    daemon.load_fonts().await;
    daemon.refresh_layout().await;
    daemon.setup_keys().await;
    if let Err(e) = daemon.start_overlay().await {
        tracing::error!("overlay unavailable at startup: {e}");
    }

    tokio::spawn(overlay_events(daemon.clone(), overlay_rx));
    if let Some(h) = hypr {
        tokio::spawn(hypr_events(daemon.clone(), h));
    }
    tokio::spawn(scheme::watch(daemon.clone()));
    tokio::spawn(watchdog(daemon.clone()));
    let server = tokio::spawn(server::serve(listener, daemon.clone()));

    let mut term = signal(SignalKind::terminate()).expect("SIGTERM handler");
    let mut int = signal(SignalKind::interrupt()).expect("SIGINT handler");
    let mut hup = signal(SignalKind::hangup()).expect("SIGHUP handler");
    let d = daemon.clone();
    tokio::spawn(async move {
        while hup.recv().await.is_some() {
            tracing::info!("SIGHUP: reloading config");
            let _ = d.reload().await;
        }
    });
    tokio::select! {
        _ = daemon.shutdown.notified() => tracing::info!("quit requested"),
        _ = term.recv() => tracing::info!("SIGTERM"),
        _ = int.recv() => tracing::info!("SIGINT"),
    }
    daemon.apply(Input::Quit).await;
    server.abort();
    if let Err(e) = std::fs::remove_file(&path) {
        tracing::warn!("removing socket: {e}");
    }
    // Give the overlay thread a moment to process the hide before exit.
    tokio::time::sleep(Duration::from_millis(20)).await;
    std::process::ExitCode::SUCCESS
}

async fn overlay_events(daemon: Arc<Daemon>, mut rx: mpsc::UnboundedReceiver<OverlayEvent>) {
    while let Some(ev) = rx.recv().await {
        match ev {
            OverlayEvent::Gone(reason) => daemon.overlay_gone(&reason).await,
            ev => {
                if let Some(ev) = OverlayKeys::map(ev) {
                    daemon.key_event(ev).await;
                }
            }
        }
    }
}

async fn hypr_events(daemon: Arc<Daemon>, hypr: Hypr) {
    loop {
        match hypr.events().await {
            Ok(mut events) => loop {
                match events.next().await {
                    Ok(Some(Event::MonitorAdded { name } | Event::MonitorRemoved { name })) => {
                        tracing::info!(monitor = %name, "monitor hot-plug; cancelling session");
                        daemon.invalidate_monitors().await;
                        daemon.apply(Input::MonitorsChanged).await;
                    }
                    Ok(Some(Event::FocusedMon { .. })) => daemon.invalidate_monitors().await,
                    Ok(Some(Event::ActiveLayout { .. })) => {
                        daemon.refresh_layout().await;
                        daemon.setup_keys().await;
                    }
                    Ok(Some(ev)) => daemon.hypr_event(&ev).await,
                    Ok(None) => break,
                    Err(e) => {
                        tracing::warn!("hyprland event socket: {e}");
                        break;
                    }
                }
            },
            Err(e) => tracing::warn!("hyprland event socket: {e}"),
        }
        daemon.events_lost().await;
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

async fn watchdog(daemon: Arc<Daemon>) {
    let mut tick = tokio::time::interval(Duration::from_millis(500));
    loop {
        tick.tick().await;
        let max_showing =
            Duration::from_millis(u64::from(daemon.settings().theme.timing.max_showing_ms));
        daemon.apply(Input::Tick { max_showing }).await;
    }
}

/// `--print-submap`: print the bind scripts for the current layout without
/// sending anything to Hyprland's `eval`.
async fn print_submap(args: &[String]) -> std::process::ExitCode {
    let fail = |m: String| {
        eprintln!("wointsd: {m}");
        std::process::ExitCode::from(1)
    };
    let path = match args.iter().position(|a| a == "--config") {
        Some(i) => args.get(i + 1).map(std::path::PathBuf::from),
        None => woints_core::config::default_path(),
    };
    let (config, _) = daemon::initial_config(path.as_deref());
    let hypr = match Hypr::from_env() {
        Ok(h) => h,
        Err(e) => return fail(e.to_string()),
    };
    let kbs = match hypr.keyboards().await {
        Ok(k) => k,
        Err(e) => return fail(e.to_string()),
    };
    let Some(kb) = keys::main_keyboard(&kbs) else {
        return fail("no keyboard".into());
    };
    let layout = keys::layout_glyphs(kb);
    let alphabet = config.alphabet(layout.as_deref());
    let input = config.input_config();
    let Some((table, _)) = keys::bind_table(&input.submap, kb, &alphabet) else {
        return fail("keymap unavailable".into());
    };
    println!("# Lua (hyprctl eval)\n{}\n", table.lua_script());
    println!("# legacy\n{}", table.legacy_batch());
    std::process::ExitCode::SUCCESS
}
