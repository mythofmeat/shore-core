use std::path::PathBuf;
use std::time::{Duration, Instant};

use tokio::process::Command;
use tokio::sync::watch;
use tracing::{error, info, warn};

const MAX_CONSECUTIVE_FAILURES: u32 = 5;
const STABLE_RUNTIME_THRESHOLD: Duration = Duration::from_mins(5);
const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);
const MATRIX_LOG_ENV: &str = "SHORE_MATRIX_RUST_LOG";
const DEFAULT_MATRIX_LOG_FILTER: &str = "warn,shore_matrix=info,matrix_sdk_crypto::backups=error";

/// Handle to the supervisor's background task; `shutdown()` joins it.
pub struct MatrixSupervisor {
    handle: tokio::task::JoinHandle<()>,
}

/// Spawn a supervisor that manages the lifecycle of `shore-matrix` as a
/// child process. Returns `None` when no binary can be located on PATH or
/// next to the running daemon — the supervisor then becomes a no-op and
/// the daemon continues without the Matrix bridge.
pub fn spawn(shutdown_rx: watch::Receiver<()>) -> Option<MatrixSupervisor> {
    let binary = locate_binary()?;
    let handle = tokio::spawn(async move {
        let mut rx = shutdown_rx;
        supervise(binary, &mut rx).await;
    });
    Some(MatrixSupervisor { handle })
}

impl MatrixSupervisor {
    /// Wait up to `timeout` for the supervisor task to exit after a
    /// shutdown signal has been sent on the watch channel.
    pub async fn shutdown(self, timeout: Duration) {
        let _ = tokio::time::timeout(timeout, self.handle).await;
    }
}

fn locate_binary() -> Option<PathBuf> {
    if let Ok(p) = which::which("shore-matrix") {
        return Some(p);
    }
    if let Some(sibling) = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("shore-matrix")))
    {
        if sibling.is_file() {
            return Some(sibling);
        }
    }
    warn!(
        "shore-matrix binary not found on PATH or next to shore-daemon; \
         Matrix bridge disabled. Install shore-matrix or add it to PATH."
    );
    None
}

async fn supervise(binary: PathBuf, shutdown_rx: &mut watch::Receiver<()>) {
    let mut failures: u32 = 0;

    loop {
        if shutdown_rx.has_changed().unwrap_or(true) {
            return;
        }

        info!(binary = %binary.display(), "spawning shore-matrix");
        let started_at = Instant::now();
        let mut command = Command::new(&binary);
        command
            .kill_on_drop(true)
            .env("RUST_LOG", matrix_log_filter());
        let mut child = match command.spawn() {
            Ok(c) => c,
            Err(e) => {
                error!(error = %e, "failed to spawn shore-matrix");
                failures = failures.saturating_add(1);
                if failures >= MAX_CONSECUTIVE_FAILURES {
                    error!(
                        failures,
                        "shore-matrix failed to spawn {MAX_CONSECUTIVE_FAILURES} \
                         times consecutively; giving up. Daemon continuing \
                         without Matrix bridge."
                    );
                    return;
                }
                if !sleep_or_shutdown(backoff(failures), shutdown_rx).await {
                    return;
                }
                continue;
            }
        };

        tokio::select! {
            status = child.wait() => {
                let runtime = started_at.elapsed();
                if runtime >= STABLE_RUNTIME_THRESHOLD {
                    failures = 0;
                }
                failures = failures.saturating_add(1);
                warn!(
                    ?status,
                    runtime_secs = runtime.as_secs(),
                    failures,
                    "shore-matrix exited; will restart with backoff"
                );
                if failures >= MAX_CONSECUTIVE_FAILURES {
                    error!(
                        failures,
                        "shore-matrix crashed {MAX_CONSECUTIVE_FAILURES} times \
                         consecutively; giving up. Daemon continuing without \
                         Matrix bridge."
                    );
                    return;
                }
                if !sleep_or_shutdown(backoff(failures), shutdown_rx).await {
                    return;
                }
            }
            _ = shutdown_rx.changed() => {
                info!("shutdown requested; stopping shore-matrix");
                graceful_shutdown(&mut child, SHUTDOWN_GRACE).await;
                return;
            }
        }
    }
}

/// Send SIGTERM to `child` and wait up to `grace` for it to exit. If the
/// child is still alive after the grace period, escalate to SIGKILL.
///
/// We route SIGTERM through `libc::kill` because tokio's `Child::start_kill`
/// sends SIGKILL on Unix, which gives shore-matrix no chance to tear down
/// its own tuwunel subprocess or flush Matrix SDK state.
async fn graceful_shutdown(child: &mut tokio::process::Child, grace: Duration) {
    if let Some(pid) = child.id() {
        let Ok(pid_t) = libc::pid_t::try_from(pid) else {
            warn!(
                pid,
                "shore-matrix pid does not fit platform pid_t; escalating"
            );
            let _ = child.start_kill();
            let _ = child.wait().await;
            return;
        };
        // SAFETY: `libc::kill` is a standard syscall; passing a valid pid
        // retrieved from the still-running child is sound.
        let rc = unsafe { libc::kill(pid_t, libc::SIGTERM) };
        if rc != 0 {
            let err = std::io::Error::last_os_error();
            warn!(pid, error = %err, "SIGTERM to shore-matrix failed; escalating");
        }
    } else {
        // No pid means the child has already been reaped.
        return;
    }

    if tokio::time::timeout(grace, child.wait()).await.is_err() {
        warn!(
            grace_secs = grace.as_secs(),
            "shore-matrix did not exit within grace period; sending SIGKILL"
        );
        let _ = child.start_kill();
        let _ = child.wait().await;
    }
}

/// Exponential backoff: 1s, 2s, 4s, 8s, 16s, capped at 32s. `failures`
/// is the 1-indexed consecutive failure count.
fn backoff(failures: u32) -> Duration {
    let shift = failures.saturating_sub(1).min(5);
    Duration::from_secs(1u64 << shift)
}

fn matrix_log_filter() -> String {
    matrix_log_filter_from(std::env::var(MATRIX_LOG_ENV).ok())
}

fn matrix_log_filter_from(value: Option<String>) -> String {
    value
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_MATRIX_LOG_FILTER.to_string())
}

/// Sleep for `dur` unless a shutdown signal arrives first. Returns
/// `true` if the sleep completed normally, `false` if interrupted by
/// shutdown.
async fn sleep_or_shutdown(dur: Duration, rx: &mut watch::Receiver<()>) -> bool {
    tokio::select! {
        () = tokio::time::sleep(dur) => true,
        _ = rx.changed() => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_progression() {
        assert_eq!(backoff(1), Duration::from_secs(1));
        assert_eq!(backoff(2), Duration::from_secs(2));
        assert_eq!(backoff(3), Duration::from_secs(4));
        assert_eq!(backoff(4), Duration::from_secs(8));
        assert_eq!(backoff(5), Duration::from_secs(16));
        assert_eq!(backoff(6), Duration::from_secs(32));
        // Saturates at the 32-second cap beyond the 5-retry window.
        assert_eq!(backoff(7), Duration::from_secs(32));
        assert_eq!(backoff(100), Duration::from_secs(32));
    }

    #[test]
    fn backoff_zero_failures_is_one_second() {
        // Not a real code path, but guards against a `saturating_sub`
        // regression turning 0 into the cap.
        assert_eq!(backoff(0), Duration::from_secs(1));
    }

    #[test]
    fn matrix_log_filter_uses_quiet_default() {
        assert_eq!(
            matrix_log_filter_from(None),
            "warn,shore_matrix=info,matrix_sdk_crypto::backups=error"
        );
        assert_eq!(
            matrix_log_filter_from(Some("   ".into())),
            "warn,shore_matrix=info,matrix_sdk_crypto::backups=error"
        );
    }

    #[test]
    fn matrix_log_filter_accepts_override() {
        assert_eq!(
            matrix_log_filter_from(Some("shore_matrix=debug".into())),
            "shore_matrix=debug"
        );
    }
}
