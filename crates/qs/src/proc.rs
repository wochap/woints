//! Child processes with a deadline. The only place the daemon forks.

use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use tokio::process::Command;

/// Children spawned since start; tests assert hot paths leave it unchanged.
static SPAWNS: AtomicU64 = AtomicU64::new(0);

pub fn spawn_count() -> u64 {
    SPAWNS.load(Ordering::SeqCst)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunError {
    /// Not installed or not executable.
    Spawn(String),
    /// Killed after the deadline.
    Timeout,
    /// Non-zero exit; stderr (or stdout) trimmed.
    Failed(String),
}

impl std::fmt::Display for RunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RunError::Spawn(e) => write!(f, "spawn: {e}"),
            RunError::Timeout => f.write_str("timed out"),
            RunError::Failed(e) => f.write_str(e),
        }
    }
}

/// Run `program args…` and return stdout. The child is killed when
/// `timeout` passes.
pub async fn run(program: &str, args: &[&str], timeout: Duration) -> Result<String, RunError> {
    SPAWNS.fetch_add(1, Ordering::SeqCst);
    let child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| RunError::Spawn(format!("{program}: {e}")))?;
    let out = match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(r) => r.map_err(|e| RunError::Failed(e.to_string()))?,
        // Dropping the future drops the child, which kills it.
        Err(_) => return Err(RunError::Timeout),
    };
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    if out.status.success() {
        Ok(stdout)
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        let msg = if err.trim().is_empty() {
            stdout.trim()
        } else {
            err.trim()
        };
        Err(RunError::Failed(format!("{program}: {msg}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn output_and_count() {
        let before = spawn_count();
        let out = run("sh", &["-c", "echo hi"], Duration::from_secs(5))
            .await
            .unwrap();
        assert_eq!(out, "hi\n");
        assert!(spawn_count() > before);
    }

    #[tokio::test]
    async fn timeout_kills() {
        let t = std::time::Instant::now();
        let r = run("sleep", &["5"], Duration::from_millis(50)).await;
        assert_eq!(r, Err(RunError::Timeout));
        assert!(t.elapsed() < Duration::from_secs(2));
    }

    #[tokio::test]
    async fn failures() {
        assert!(matches!(
            run("/nonexistent/qs", &[], Duration::from_secs(1)).await,
            Err(RunError::Spawn(_))
        ));
        assert_eq!(
            run(
                "sh",
                &["-c", "echo nope >&2; exit 3"],
                Duration::from_secs(5)
            )
            .await,
            Err(RunError::Failed("sh: nope".into()))
        );
    }
}
