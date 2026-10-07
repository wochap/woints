//! Unix socket server: JSON lines, one task per connection.

use std::io::ErrorKind;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

use woints_core::proto::{Request, Response, parse_request};

use crate::daemon::Daemon;

#[derive(Debug)]
pub enum BindError {
    AlreadyRunning,
    Io(std::io::Error),
}

impl std::error::Error for BindError {}

impl std::fmt::Display for BindError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BindError::AlreadyRunning => f.write_str("wointsd already running"),
            BindError::Io(e) => write!(f, "{e}"),
        }
    }
}

/// Bind the socket, refusing if a live daemon answers `status` and removing
/// a stale socket file otherwise.
pub async fn bind(path: &Path) -> Result<UnixListener, BindError> {
    if path.exists() {
        match UnixStream::connect(path).await {
            Ok(s) => {
                if probe_status(s).await {
                    return Err(BindError::AlreadyRunning);
                }
                tracing::warn!(path = %path.display(), "socket accepts but does not answer; replacing");
                std::fs::remove_file(path).map_err(BindError::Io)?;
            }
            Err(e) if matches!(e.kind(), ErrorKind::ConnectionRefused | ErrorKind::NotFound) => {
                tracing::info!(path = %path.display(), "removing stale socket");
                std::fs::remove_file(path).map_err(BindError::Io)?;
            }
            Err(e) => return Err(BindError::Io(e)),
        }
    }
    UnixListener::bind(path).map_err(BindError::Io)
}

async fn probe_status(s: UnixStream) -> bool {
    let fut = async {
        let (r, mut w) = s.into_split();
        w.write_all(b"{\"cmd\":\"status\"}\n").await.ok()?;
        let mut line = String::new();
        BufReader::new(r).read_line(&mut line).await.ok()?;
        serde_json::from_str::<Response>(&line).ok().map(|r| r.ok)
    };
    matches!(
        tokio::time::timeout(Duration::from_millis(500), fut).await,
        Ok(Some(true))
    )
}

pub async fn serve(listener: UnixListener, daemon: Arc<Daemon>) {
    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                let d = daemon.clone();
                tokio::spawn(async move {
                    if let Err(e) = connection(stream, d).await {
                        tracing::debug!("connection closed: {e}");
                    }
                });
            }
            Err(e) => {
                tracing::error!("accept: {e}");
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }
}

async fn connection(stream: UnixStream, daemon: Arc<Daemon>) -> std::io::Result<()> {
    let (r, mut w) = stream.into_split();
    let mut lines = BufReader::new(r).lines();
    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let resp = match parse_request(&line) {
            Ok(req) => {
                tracing::debug!(?req, "request");
                let quit = req == Request::Quit;
                let resp = daemon.handle(req).await;
                if quit {
                    w.write_all(resp.to_line().as_bytes()).await?;
                    w.flush().await?;
                    return Ok(());
                }
                resp
            }
            Err(e) => Response::err(e),
        };
        w.write_all(resp.to_line().as_bytes()).await?;
    }
    Ok(())
}
