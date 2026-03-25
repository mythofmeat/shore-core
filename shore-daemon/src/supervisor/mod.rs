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
        services_config: &crate::config::app::ServicesConfig,
        runtime_dir: &Path,
    ) -> Self {
        let mut specs = Vec::new();

        // shore-llm is always required if it has a command.
        if services_config.llm.enabled {
            if let Some(ref cmd) = services_config.llm.command {
                let socket = services_config
                    .llm
                    .socket
                    .as_ref()
                    .map(PathBuf::from)
                    .unwrap_or_else(|| runtime_dir.join("llm.sock"));
                specs.push(ServiceSpec {
                    name: "shore-llm".into(),
                    command: cmd.clone(),
                    socket,
                });
            }
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
        for (name, handle) in health_handles {
            match handle.await {
                Ok(true) => {
                    if let Some(svc) = self.services.iter_mut().find(|s| s.name == name) {
                        svc.state = ServiceState::Ready;
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

        // Monitor loop: watch for child exits and restart as needed.
        loop {
            tokio::select! {
                _ = shutdown.changed() => {
                    info!("Supervisor received shutdown signal");
                    break;
                }
                _ = tokio::time::sleep(Duration::from_secs(1)) => {
                    for svc in &mut self.services {
                        if svc.state == ServiceState::Failed || svc.state == ServiceState::Stopped {
                            continue;
                        }
                        if let Some(ref mut child) = svc.child {
                            match child.try_wait() {
                                Ok(Some(status)) => {
                                    warn!(
                                        service = %svc.name,
                                        exit_code = ?status.code(),
                                        "Service exited unexpectedly"
                                    );
                                    svc.child = None;
                                    handle_service_restart(svc).await;
                                    if svc.state == ServiceState::Ready && svc.name == "shore-llm" {
                                        // Still ready after restart — already signaled.
                                    }
                                }
                                Ok(None) => {
                                    // Still running, good.
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

/// Handle restart logic with exponential backoff.
async fn handle_service_restart(svc: &mut ManagedService) {
    svc.restart_count += 1;

    if svc.restart_count > MAX_RESTARTS {
        error!(
            service = %svc.name,
            restarts = svc.restart_count,
            "Service exceeded max restarts, marking as failed"
        );
        svc.state = ServiceState::Failed;
        return;
    }

    // Exponential backoff: 1s, 2s, 4s, 8s, 16s, 30s (capped).
    let delay = Duration::from_secs(1 << (svc.restart_count - 1).min(5))
        .min(MAX_BACKOFF);

    warn!(
        service = %svc.name,
        restart_count = svc.restart_count,
        delay_secs = delay.as_secs(),
        "Restarting service after backoff"
    );

    tokio::time::sleep(delay).await;
    spawn_service(svc);

    // Health-check the restarted service.
    if wait_for_health(&svc.name, &svc.socket).await {
        svc.state = ServiceState::Ready;
        info!(service = %svc.name, "Service recovered after restart");
    } else {
        warn!(service = %svc.name, "Service failed health check after restart");
        // Will be retried on next monitor cycle if child exits again.
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::app::ServiceEntry;
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

        // Wait a moment for it to exit.
        tokio::time::sleep(Duration::from_millis(100)).await;

        if let Some(ref mut child) = sup.services[0].child {
            let status = child.try_wait().unwrap();
            assert!(status.is_some(), "Process should have exited");
        }
    }

    #[tokio::test]
    async fn restart_count_increments_and_caps() {
        let tmp = tempfile::tempdir().unwrap();
        let socket = tmp.path().join("restart.sock");

        let mut svc = ManagedService {
            name: "test-svc".into(),
            command: "false".into(), // Exits with code 1 immediately.
            socket,
            state: ServiceState::Ready,
            child: None,
            restart_count: 4, // One away from max.
        };

        // First restart should work (restart_count becomes 5).
        handle_service_restart(&mut svc).await;
        assert_eq!(svc.restart_count, 5);

        // Next restart should mark as failed (restart_count becomes 6 > MAX_RESTARTS).
        handle_service_restart(&mut svc).await;
        assert_eq!(svc.restart_count, 6);
        assert_eq!(svc.state, ServiceState::Failed);
    }

    #[tokio::test]
    async fn supervisor_from_config_parses_services() {
        let config = crate::config::app::ServicesConfig {
            llm: ServiceEntry {
                command: Some("node shore-llm/dist/index.js".into()),
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
    async fn supervisor_from_config_respects_enabled() {
        let config = crate::config::app::ServicesConfig {
            llm: ServiceEntry {
                command: Some("node shore-llm/dist/index.js".into()),
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
