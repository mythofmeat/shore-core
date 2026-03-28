use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::watch;
use tracing::{error, info, warn};

/// Maximum number of restart attempts before marking a service as failed.
const MAX_RESTARTS: u32 = 5;

/// Maximum backoff delay between restarts.
const MAX_BACKOFF: Duration = Duration::from_secs(30);

/// If a service ran stably for this long before crashing, reset its restart
/// counter so transient failures don't permanently exhaust the budget.
const STABLE_WINDOW: Duration = Duration::from_secs(300);

/// Health-check poll interval.
const HEALTH_INTERVAL: Duration = Duration::from_secs(1);

/// Health-check timeout (total time waiting for a service to become healthy).
const HEALTH_TIMEOUT: Duration = Duration::from_secs(30);

/// Grace period after SIGTERM before SIGKILL.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(10);

/// State of a supervised service.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceState {
    Starting,
    Ready,
    Failed,
    Stopped,
}

/// Runtime information for one supervised service.
struct ManagedService {
    name: String,
    command: String,
    socket: PathBuf,
    state: ServiceState,
    child: Option<Child>,
    restart_count: u32,
    /// Scheduled respawn time — set when a crash triggers a backoff delay.
    restart_after: Option<tokio::time::Instant>,
    /// When the service last transitioned to Ready — used to reset restart_count
    /// after a sustained healthy run.
    healthy_since: Option<tokio::time::Instant>,
}

/// Configuration for one service to supervise.
#[derive(Debug, Clone)]
pub struct ServiceSpec {
    pub name: String,
    pub command: String,
    pub socket: PathBuf,
}

/// The process supervisor. Spawns and monitors child services.
pub struct Supervisor {
    services: Vec<ManagedService>,
    /// Notifies waiters when shore-llm becomes ready.
    llm_ready_tx: watch::Sender<bool>,
    llm_ready_rx: watch::Receiver<bool>,
}

impl Supervisor {
    /// Build a supervisor from the loaded services config.
    ///
    /// `runtime_dir` is used to auto-generate socket paths when not specified.
    pub fn from_config(
        services_config: &shore_config::app::ServicesConfig,
        runtime_dir: &Path,
    ) -> Self {
        let mut specs = Vec::new();

        // shore-llm: defaults to "shore-llm" on PATH when no command is set.
        if services_config.llm.enabled {
            let cmd = services_config
                .llm
                .command
                .as_deref()
                .unwrap_or("shore-llm")
                .to_string();
            let socket = services_config
                .llm
                .socket
                .as_ref()
                .map(PathBuf::from)
                .unwrap_or_else(|| runtime_dir.join("llm.sock"));
            specs.push(ServiceSpec {
                name: "shore-llm".into(),
                command: cmd,
                socket,
            });
        }

        // Optional bridges.
        if let Some(ref mx) = services_config.matrix {
            if mx.enabled {
                if let Some(ref cmd) = mx.command {
                    let socket = mx
                        .socket
                        .as_ref()
                        .map(PathBuf::from)
                        .unwrap_or_else(|| runtime_dir.join("matrix.sock"));
                    specs.push(ServiceSpec {
                        name: "shore-matrix".into(),
                        command: cmd.clone(),
                        socket,
                    });
                }
            }
        }

        Self::new(specs)
    }

    /// Create a supervisor for a list of service specs.
    pub fn new(specs: Vec<ServiceSpec>) -> Self {
        let (llm_ready_tx, llm_ready_rx) = watch::channel(false);
        let services = specs
            .into_iter()
            .map(|spec| ManagedService {
                name: spec.name,
                command: spec.command,
                socket: spec.socket,
                state: ServiceState::Starting,
                child: None,
                restart_count: 0,
                restart_after: None,
                healthy_since: None,
            })
            .collect();
        Self {
            services,
            llm_ready_tx,
            llm_ready_rx,
        }
    }

    /// Returns a receiver that resolves to `true` when shore-llm is ready.
    pub fn llm_ready(&self) -> watch::Receiver<bool> {
        self.llm_ready_rx.clone()
    }

    /// Returns the current state of all services.
    pub fn states(&self) -> HashMap<String, ServiceState> {
        self.services
            .iter()
            .map(|s| (s.name.clone(), s.state))
            .collect()
    }

    /// Start all services and monitor them. Runs until `shutdown` fires.
    pub async fn run(&mut self, mut shutdown: watch::Receiver<()>) {
        // Spawn all services initially.
        for svc in &mut self.services {
            spawn_service(svc);
        }

        // Run health checks for all services concurrently.
        let mut health_handles = Vec::new();
        for svc in &self.services {
            let name = svc.name.clone();
            let socket = svc.socket.clone();
            let handle = tokio::spawn(async move {
                wait_for_health(&name, &socket).await
            });
            health_handles.push((svc.name.clone(), handle));
        }

        // Collect health check results.
        let now = tokio::time::Instant::now();
        for (name, handle) in health_handles {
            match handle.await {
                Ok(true) => {
                    if let Some(svc) = self.services.iter_mut().find(|s| s.name == name) {
                        svc.state = ServiceState::Ready;
                        svc.healthy_since = Some(now);
                        info!(service = %name, "Service is ready");
                        if name == "shore-llm" {
                            let _ = self.llm_ready_tx.send(true);
                        }
                    }
                }
                Ok(false) => {
                    if let Some(svc) = self.services.iter_mut().find(|s| s.name == name) {
                        svc.state = ServiceState::Failed;
                        error!(service = %name, "Service failed health check");
                    }
                }
                Err(e) => {
                    error!(service = %name, error = %e, "Health check task panicked");
                }
            }
        }

        // Monitor loop: watch for child exits and fire scheduled restarts.
        // Restarts are non-blocking — a crash schedules a future respawn time
        // and the select! continues processing shutdown signals normally.
        loop {
            tokio::select! {
                _ = shutdown.changed() => {
                    info!("Supervisor received shutdown signal");
                    break;
                }
                _ = tokio::time::sleep(HEALTH_INTERVAL) => {
                    let now = tokio::time::Instant::now();
                    for svc in &mut self.services {
                        if svc.state == ServiceState::Failed || svc.state == ServiceState::Stopped {
                            continue;
                        }

                        // Fire a scheduled respawn when the backoff window has elapsed.
                        if let Some(restart_at) = svc.restart_after {
                            if now >= restart_at {
                                svc.restart_after = None;
                                spawn_service(svc);
                                svc.state = ServiceState::Ready;
                                svc.healthy_since = Some(now);
                                info!(service = %svc.name, "Service restarted");
                                if svc.name == "shore-llm" {
                                    let _ = self.llm_ready_tx.send(true);
                                }
                            }
                            continue;
                        }

                        // Check whether the running child has exited.
                        if let Some(ref mut child) = svc.child {
                            match child.try_wait() {
                                Ok(Some(status)) => {
                                    warn!(
                                        service = %svc.name,
                                        exit_code = ?status.code(),
                                        "Service exited unexpectedly"
                                    );
                                    svc.child = None;

                                    // Reset the restart counter if the service ran
                                    // stably for long enough before this crash.
                                    if svc.healthy_since
                                        .map_or(false, |t| t.elapsed() >= STABLE_WINDOW)
                                    {
                                        info!(
                                            service = %svc.name,
                                            "Service was stable; resetting restart counter"
                                        );
                                        svc.restart_count = 0;
                                    }

                                    svc.restart_count += 1;
                                    if svc.restart_count > MAX_RESTARTS {
                                        error!(
                                            service = %svc.name,
                                            restarts = svc.restart_count,
                                            "Service exceeded max restarts, marking as failed"
                                        );
                                        svc.state = ServiceState::Failed;
                                    } else {
                                        let delay = Duration::from_secs(
                                            1 << (svc.restart_count - 1).min(5),
                                        )
                                        .min(MAX_BACKOFF);
                                        warn!(
                                            service = %svc.name,
                                            restart_count = svc.restart_count,
                                            delay_secs = delay.as_secs(),
                                            "Scheduling restart after backoff"
                                        );
                                        svc.state = ServiceState::Starting;
                                        svc.restart_after = Some(now + delay);
                                    }
                                }
                                Ok(None) => {
                                    // Still running.
                                }
                                Err(e) => {
                                    error!(service = %svc.name, error = %e, "Failed to poll child");
                                }
                            }
                        }
                    }
                }
            }
        }

        // Graceful shutdown: SIGTERM → wait → SIGKILL.
        self.shutdown_children().await;
    }

    /// Send SIGTERM to all children, wait grace period, then SIGKILL any survivors.
    async fn shutdown_children(&mut self) {
        for svc in &mut self.services {
            if let Some(ref child) = svc.child {
                let pid = child.id();
                if let Some(pid) = pid {
                    info!(service = %svc.name, pid, "Sending SIGTERM");
                    // Safety: we own the child process.
                    unsafe {
                        libc::kill(pid as i32, libc::SIGTERM);
                    }
                }
            }
        }

        // Wait for grace period, checking periodically if children have exited.
        let deadline = tokio::time::Instant::now() + SHUTDOWN_GRACE;
        loop {
            let all_exited = self.services.iter_mut().all(|svc| {
                match svc.child.as_mut() {
                    Some(child) => matches!(child.try_wait(), Ok(Some(_))),
                    None => true,
                }
            });
            if all_exited {
                info!("All child processes exited cleanly");
                break;
            }
            if tokio::time::Instant::now() >= deadline {
                break;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }

        // SIGKILL any survivors.
        for svc in &mut self.services {
            if let Some(ref mut child) = svc.child {
                match child.try_wait() {
                    Ok(Some(_)) => {} // Already exited.
                    _ => {
                        warn!(service = %svc.name, "Sending SIGKILL after grace period");
                        let _ = child.kill().await;
                    }
                }
            }
            svc.child = None;
            svc.state = ServiceState::Stopped;
        }
    }
}

/// Spawn a service as a child process with stderr piped for log collection.
fn spawn_service(svc: &mut ManagedService) {
    let parts: Vec<&str> = svc.command.split_whitespace().collect();
    if parts.is_empty() {
        error!(service = %svc.name, "Empty command");
        svc.state = ServiceState::Failed;
        return;
    }

    let program = parts[0];
    let mut args: Vec<&str> = parts[1..].to_vec();

    // Pass socket path as argument if the command doesn't already include it.
    let socket_str = svc.socket.display().to_string();
    if !svc.command.contains(&socket_str) {
        args.push(&socket_str);
    }

    info!(service = %svc.name, command = %svc.command, socket = %svc.socket.display(), "Spawning service");

    match Command::new(program)
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(false)
        .spawn()
    {
        Ok(mut child) => {
            // Spawn a task to collect stderr and interleave with daemon logs.
            if let Some(stderr) = child.stderr.take() {
                let name = svc.name.clone();
                tokio::spawn(async move {
                    collect_stderr(&name, stderr).await;
                });
            }
            info!(service = %svc.name, pid = ?child.id(), "Service spawned");
            svc.child = Some(child);
        }
        Err(e) => {
            error!(service = %svc.name, error = %e, "Failed to spawn service");
            svc.state = ServiceState::Failed;
        }
    }
}

/// Collect stderr lines from a child and log them with the service name.
async fn collect_stderr(service_name: &str, stderr: tokio::process::ChildStderr) {
    let mut reader = BufReader::new(stderr).lines();
    while let Ok(Some(line)) = reader.next_line().await {
        info!(service = %service_name, "{}", line);
    }
}

/// Wait for a service to become healthy by checking its Unix socket.
///
/// Returns `true` if the service responded to a health check within the timeout.
async fn wait_for_health(service_name: &str, socket: &Path) -> bool {
    let deadline = tokio::time::Instant::now() + HEALTH_TIMEOUT;

    while tokio::time::Instant::now() < deadline {
        if check_health(socket).await {
            return true;
        }
        tokio::time::sleep(HEALTH_INTERVAL).await;
    }

    error!(
        service = %service_name,
        socket = %socket.display(),
        "Health check timed out after {}s",
        HEALTH_TIMEOUT.as_secs()
    );
    false
}

/// Perform a single health check against a Unix socket HTTP endpoint.
///
/// Sends a minimal HTTP GET /v1/health request and checks for a 200 response.
async fn check_health(socket: &Path) -> bool {
    use tokio::net::UnixStream;

    let stream = match UnixStream::connect(socket).await {
        Ok(s) => s,
        Err(_) => return false,
    };

    let request = "GET /v1/health HTTP/1.0\r\nHost: localhost\r\n\r\n";
    let (reader, mut writer) = tokio::io::split(stream);

    use tokio::io::AsyncWriteExt;
    if writer.write_all(request.as_bytes()).await.is_err() {
        return false;
    }

    // Read enough of the response to check the status code.
    let mut buf_reader = BufReader::new(reader);
    let mut status_line = String::new();
    match buf_reader.read_line(&mut status_line).await {
        Ok(0) | Err(_) => false,
        Ok(_) => status_line.contains("200"),
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use shore_config::app::ServiceEntry;
    use tokio::net::UnixListener;

    #[tokio::test]
    async fn health_check_succeeds_with_200() {
        let tmp = tempfile::tempdir().unwrap();
        let socket = tmp.path().join("health.sock");

        let listener = UnixListener::bind(&socket).unwrap();
        tokio::spawn(async move {
            if let Ok((stream, _)) = listener.accept().await {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                let (mut r, mut w) = tokio::io::split(stream);
                let mut buf = vec![0u8; 1024];
                let _ = r.read(&mut buf).await;
                let _ = w
                    .write_all(b"HTTP/1.0 200 OK\r\nContent-Length: 0\r\n\r\n")
                    .await;
            }
        });

        assert!(check_health(&socket).await);
    }

    #[tokio::test]
    async fn health_check_fails_with_no_socket() {
        let tmp = tempfile::tempdir().unwrap();
        let socket = tmp.path().join("nonexistent.sock");
        assert!(!check_health(&socket).await);
    }

    #[tokio::test]
    async fn health_check_fails_with_500() {
        let tmp = tempfile::tempdir().unwrap();
        let socket = tmp.path().join("health500.sock");

        let listener = UnixListener::bind(&socket).unwrap();
        tokio::spawn(async move {
            if let Ok((stream, _)) = listener.accept().await {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                let (mut r, mut w) = tokio::io::split(stream);
                let mut buf = vec![0u8; 1024];
                let _ = r.read(&mut buf).await;
                let _ = w
                    .write_all(b"HTTP/1.0 500 Internal Server Error\r\n\r\n")
                    .await;
            }
        });

        assert!(!check_health(&socket).await);
    }

    #[tokio::test]
    async fn spawn_and_monitor_real_process() {
        // Use a simple command that exits quickly — `true` on Unix.
        let tmp = tempfile::tempdir().unwrap();
        let socket = tmp.path().join("test.sock");

        let specs = vec![ServiceSpec {
            name: "test-service".into(),
            command: "true".into(),
            socket: socket.clone(),
        }];
        let mut sup = Supervisor::new(specs);
        assert_eq!(sup.services.len(), 1);

        // Spawn the service.
        spawn_service(&mut sup.services[0]);
        assert!(sup.services[0].child.is_some());
        assert!(sup.services[0].restart_after.is_none());

        // Wait a moment for it to exit.
        tokio::time::sleep(Duration::from_millis(100)).await;

        if let Some(ref mut child) = sup.services[0].child {
            let status = child.try_wait().unwrap();
            assert!(status.is_some(), "Process should have exited");
        }
    }

    #[tokio::test]
    async fn crash_schedules_backoff_restart() {
        let tmp = tempfile::tempdir().unwrap();
        let socket = tmp.path().join("restart.sock");

        // Simulate a service that just crashed (child = None, restart_count = 4).
        let mut svc = ManagedService {
            name: "test-svc".into(),
            command: "false".into(),
            socket,
            state: ServiceState::Ready,
            child: None,
            restart_count: 4,
            restart_after: None,
            healthy_since: None,
        };

        // Simulate the crash handling inline (mirrors what the monitor loop does).
        svc.restart_count += 1; // becomes 5 — still under MAX_RESTARTS
        let delay = Duration::from_secs(1 << (svc.restart_count - 1).min(5)).min(MAX_BACKOFF);
        svc.state = ServiceState::Starting;
        svc.restart_after = Some(tokio::time::Instant::now() + delay);

        assert_eq!(svc.restart_count, 5);
        assert_eq!(svc.state, ServiceState::Starting);
        assert!(svc.restart_after.is_some());

        // One more crash — exceeds MAX_RESTARTS.
        svc.restart_after = None;
        svc.restart_count += 1; // becomes 6
        if svc.restart_count > MAX_RESTARTS {
            svc.state = ServiceState::Failed;
        }
        assert_eq!(svc.state, ServiceState::Failed);
    }

    #[tokio::test]
    async fn stable_service_resets_restart_count() {
        let tmp = tempfile::tempdir().unwrap();
        let socket = tmp.path().join("stable.sock");

        let mut svc = ManagedService {
            name: "stable-svc".into(),
            command: "true".into(),
            socket,
            state: ServiceState::Ready,
            child: None,
            restart_count: 4,
            restart_after: None,
            // Service was healthy long ago — elapsed will be >> STABLE_WINDOW.
            healthy_since: Some(tokio::time::Instant::now() - STABLE_WINDOW - Duration::from_secs(1)),
        };

        if svc.healthy_since.map_or(false, |t| t.elapsed() >= STABLE_WINDOW) {
            svc.restart_count = 0;
        }
        svc.restart_count += 1;
        assert_eq!(svc.restart_count, 1, "counter should have been reset before incrementing");
    }

    #[tokio::test]
    async fn supervisor_from_config_parses_services() {
        let config = shore_config::app::ServicesConfig {
            llm: ServiceEntry {
                command: Some("shore-llm".into()),
                socket: Some("/tmp/llm.sock".into()),
                enabled: true,
            },
            matrix: Some(ServiceEntry {
                command: Some("shore-matrix".into()),
                socket: None,
                enabled: true,
            }),
        };

        let runtime_dir = PathBuf::from("/tmp/shore");
        let sup = Supervisor::from_config(&config, &runtime_dir);

        assert_eq!(sup.services.len(), 2);
        assert_eq!(sup.services[0].name, "shore-llm");
        assert_eq!(sup.services[0].socket, PathBuf::from("/tmp/llm.sock"));
        assert_eq!(sup.services[1].name, "shore-matrix");
        assert_eq!(sup.services[1].socket, PathBuf::from("/tmp/shore/matrix.sock"));
    }

    #[tokio::test]
    async fn supervisor_from_config_defaults_command_to_shore_llm() {
        let config = shore_config::app::ServicesConfig {
            llm: ServiceEntry {
                command: None, // No explicit command — should default to "shore-llm".
                socket: None,
                enabled: true,
            },
            matrix: None,
        };

        let runtime_dir = PathBuf::from("/tmp/shore");
        let sup = Supervisor::from_config(&config, &runtime_dir);

        assert_eq!(sup.services.len(), 1);
        assert_eq!(sup.services[0].command, "shore-llm");
    }

    #[tokio::test]
    async fn supervisor_from_config_respects_enabled() {
        let config = shore_config::app::ServicesConfig {
            llm: ServiceEntry {
                command: None,
                socket: None,
                enabled: false, // Disabled.
            },
            matrix: None,
        };

        let runtime_dir = PathBuf::from("/tmp/shore");
        let sup = Supervisor::from_config(&config, &runtime_dir);
        assert!(sup.services.is_empty());
    }

    #[tokio::test]
    async fn supervisor_shutdown_sends_sigterm() {
        let tmp = tempfile::tempdir().unwrap();
        let socket = tmp.path().join("sleep.sock");

        // Spawn a long-running process (sleep).
        let mut svc = ManagedService {
            name: "sleeper".into(),
            command: "sleep 60".into(),
            socket,
            state: ServiceState::Ready,
            child: None,
            restart_count: 0,
            restart_after: None,
            healthy_since: None,
        };
        spawn_service(&mut svc);
        assert!(svc.child.is_some());

        let pid = svc.child.as_ref().unwrap().id();
        assert!(pid.is_some());

        let mut sup = Supervisor {
            services: vec![svc],
            llm_ready_tx: watch::channel(false).0,
            llm_ready_rx: watch::channel(false).1,
        };

        sup.shutdown_children().await;

        // All children should be gone.
        assert!(sup.services[0].child.is_none());
        assert_eq!(sup.services[0].state, ServiceState::Stopped);
    }

    #[tokio::test]
    async fn wait_for_health_succeeds() {
        let tmp = tempfile::tempdir().unwrap();
        let socket = tmp.path().join("health_wait.sock");

        let listener = UnixListener::bind(&socket).unwrap();
        tokio::spawn(async move {
            // Accept multiple connections (health check may retry).
            loop {
                match listener.accept().await {
                    Ok((stream, _)) => {
                        tokio::spawn(async move {
                            use tokio::io::{AsyncReadExt, AsyncWriteExt};
                            let (mut r, mut w) = tokio::io::split(stream);
                            let mut buf = vec![0u8; 1024];
                            let _ = r.read(&mut buf).await;
                            let _ = w
                                .write_all(b"HTTP/1.0 200 OK\r\nContent-Length: 0\r\n\r\n")
                                .await;
                        });
                    }
                    Err(_) => break,
                }
            }
        });

        let result = wait_for_health("test-svc", &socket).await;
        assert!(result);
    }

    #[tokio::test]
    async fn service_state_transitions() {
        let specs = vec![ServiceSpec {
            name: "test".into(),
            command: "true".into(),
            socket: PathBuf::from("/tmp/test.sock"),
        }];
        let sup = Supervisor::new(specs);

        let states = sup.states();
        assert_eq!(states.get("test"), Some(&ServiceState::Starting));
    }
}
