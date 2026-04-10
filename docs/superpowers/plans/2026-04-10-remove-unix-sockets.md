# Remove Unix Sockets — TCP-Only Transport

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Remove all Unix socket transport code and make TCP the sole transport for daemon-client communication.

**Architecture:** The daemon currently always listens on a Unix socket and optionally on TCP. This plan removes the Unix socket listener entirely, makes TCP always-on (default `127.0.0.1:7320`), and simplifies instance discovery to use TCP addresses as the canonical identity. Multiple daemons coexist by using different ports.

**Tech Stack:** Rust, tokio, serde, clap

---

## File Map

| Action | File | Responsibility |
|--------|------|----------------|
| Modify | `shore-config/src/app.rs` | Remove `DaemonConfig.socket_path`, flatten `TcpConfig` into `DaemonConfig` |
| Modify | `shore-daemon/src/server/mod.rs` | Remove `UnixListener`, make `TcpListener` the sole listener |
| Modify | `shore-daemon/src/server/registry.rs` | Replace `socket_path` with `addr` in `InstanceInfo` |
| Modify | `shore-daemon/src/main.rs` | Remove socket path logic, always bind TCP |
| Modify | `shore-client/src/connection.rs` | Remove `ServerAddr::Unix`, `is_unix_path()`, Unix stream code |
| Modify | `shore-client/src/discovery.rs` | Remove `default_socket_path()`, `addr_from_socket()`, Unix fallback |
| Modify | `shore-client/src/conn_manager.rs` | Remove `is_unix_path` branching in `resolve_addr` |
| Modify | `shore-client/src/client_config.rs` | Update doc comments |
| Modify | `shore-client/src/lib.rs` | Update tests |
| Modify | `shore-client/Cargo.toml` | Remove `[target.'cfg(unix)'.dependencies] libc` |
| Modify | `shore-cli/src/cli.rs` | Rename `--socket` to `--addr`, update help text |
| Modify | `shore-cli/src/run.rs` | Simplify `resolve_addr` to always produce `ServerAddr::Tcp` |
| Modify | `shore-tui/src/main.rs` | Rename `--socket` to `--addr` |
| Modify | `shore-tui/src/connection.rs` | Pass through renamed field |
| Modify | `shore-matrix/src/main.rs` | Rename `--socket` to `--addr` |
| Modify | `shore-matrix/src/connection.rs` | Pass through renamed field |
| Modify | `examples/config.toml` | Update `[daemon]` section |
| Modify | `examples/client.toml` | Remove Unix socket example |
| Modify | `docs/ARCHITECTURE.md` | Update discovery docs |
| Modify | `docs/DECISIONS.md` | Record decision |

---

### Task 1: Config — Replace `socket_path` with TCP-first `DaemonConfig`

**Files:**
- Modify: `shore-config/src/app.rs:48-55` (DaemonConfig)
- Modify: `shore-config/src/app.rs:412-443` (ConnectionsConfig, TcpConfig)

- [ ] **Step 1: Update `DaemonConfig` to hold TCP fields directly**

Replace the current `DaemonConfig`:

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct DaemonConfig {
    /// TCP address to listen on (default: "127.0.0.1:7320").
    #[serde(default = "default_daemon_addr")]
    pub addr: String,

    /// Allowed client hosts. Empty list means allow all.
    #[serde(default)]
    pub allowed_hosts: Vec<String>,
}

serde_default!(default_daemon_addr -> String { "127.0.0.1:7320".to_string() });

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            addr: default_daemon_addr(),
            allowed_hosts: vec![],
        }
    }
}
```

- [ ] **Step 2: Remove `TcpConfig` and the `tcp` field from `ConnectionsConfig`**

Remove:
```rust
pub struct TcpConfig { ... }
```

And remove the `tcp` field from `ConnectionsConfig`:
```rust
pub struct ConnectionsConfig {
    // tcp: Option<TcpConfig>,  ← REMOVE
    pub matrix: Option<MatrixConfig>,
    pub telegram: Option<TelegramConfig>,
    pub discord: Option<DiscordConfig>,
}
```

- [ ] **Step 3: Run `cargo check -p shore-config`**

This will fail with downstream errors — that's expected. Confirm only the expected errors (missing `TcpConfig`, changed `DaemonConfig` fields).

- [ ] **Step 4: Commit**

```bash
git add shore-config/src/app.rs
git commit -m "refactor(config): replace socket_path + TcpConfig with DaemonConfig.addr"
```

---

### Task 2: Registry — Replace `socket_path` with `addr`

**Files:**
- Modify: `shore-daemon/src/server/registry.rs`

- [ ] **Step 1: Update `InstanceInfo` struct**

Replace the struct:

```rust
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct InstanceInfo {
    pub id: String,
    pub pid: u32,
    /// TCP address where this instance listens (e.g. "127.0.0.1:7320").
    pub addr: String,
    pub started_at: String,
    /// Resolved data directory so CLI commands can find the ledger.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_dir: Option<String>,
}
```

- [ ] **Step 2: Update test helper `sample_instance`**

```rust
fn sample_instance(id: &str) -> InstanceInfo {
    InstanceInfo {
        id: id.into(),
        pid: std::process::id(),
        addr: format!("127.0.0.1:73{}", id.chars().last().unwrap_or('0') as u8 % 10),
        started_at: "2026-01-01T00:00:00Z".into(),
        data_dir: None,
    }
}
```

- [ ] **Step 3: Update test assertions that reference `socket_path`**

The `register_replaces_stale_entry` test checks `socket_path` — change to `addr`:

```rust
#[test]
fn register_replaces_stale_entry() {
    let (_tmp, reg) = test_registry();
    reg.register(sample_instance("test-4")).unwrap();

    let mut updated = sample_instance("test-4");
    updated.addr = "127.0.0.1:9999".into();
    reg.register(updated.clone()).unwrap();

    let entries = reg.list().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].addr, "127.0.0.1:9999");
}
```

- [ ] **Step 4: Run `cargo test -p shore-daemon -- registry`**

Expected: all registry tests pass.

- [ ] **Step 5: Commit**

```bash
git add shore-daemon/src/server/registry.rs
git commit -m "refactor(registry): replace socket_path + tcp_addr with single addr field"
```

---

### Task 3: Server — Remove Unix listener, TCP-only

**Files:**
- Modify: `shore-daemon/src/server/mod.rs`

- [ ] **Step 1: Update imports — remove `UnixListener`**

Remove:
```rust
use tokio::net::{TcpListener, UnixListener};
```
Replace with:
```rust
use tokio::net::TcpListener;
```

Also remove the `use shore_config::app::TcpConfig;` import since `TcpConfig` no longer exists.

- [ ] **Step 2: Simplify `ServerConfig`**

```rust
pub struct ServerConfig {
    /// TCP address to bind (e.g. "127.0.0.1:7320").
    pub addr: String,
    /// Allowed client hosts. Empty = allow all.
    pub allowed_hosts: Vec<String>,
    pub server_name: String,
}
```

- [ ] **Step 3: Rewrite `Server::run()` — TCP only**

Replace the entire `run()` method body. Remove the Unix socket binding, the `tokio::select!` with two listener branches, and the socket cleanup. The new body:

```rust
pub async fn run(&self, shutdown: tokio::sync::watch::Receiver<()>) -> std::io::Result<()> {
    let listener = TcpListener::bind(&self.config.addr).await?;
    info!(addr = %self.config.addr, "TCP listening");

    let allowed_hosts = self.config.allowed_hosts.clone();

    loop {
        tokio::select! {
            result = listener.accept() => {
                match result {
                    Ok((stream, addr)) => {
                        if !allowed_hosts.is_empty() {
                            let peer_ip = addr.ip().to_string();
                            if !allowed_hosts.iter().any(|h| h == &peer_ip) {
                                warn!(%addr, "TCP connection rejected: not in allowed_hosts");
                                drop(stream);
                                continue;
                            }
                        }
                        info!(%addr, "Client connected");
                        let (reader, writer) = stream.into_split();
                        self.spawn_client(reader, writer, shutdown.clone());
                    }
                    Err(e) => error!(error = %e, "Accept error"),
                }
            }

            _ = {
                let mut rx = shutdown.clone();
                async move { rx.changed().await }
            } => {
                info!("Server shutting down");
                self.broadcast(ServerMessage::Shutdown(Shutdown {}));
                break;
            }
        }
    }

    Ok(())
}
```

- [ ] **Step 4: Update tests — remove `socket_path` from `ServerConfig`**

In `spawn_tcp_server` test helper:
```rust
fn spawn_tcp_server(
    port: u16,
    allowed_hosts: Vec<String>,
) -> (
    tokio::task::JoinHandle<std::io::Result<()>>,
    tokio::sync::watch::Sender<()>,
) {
    let config = ServerConfig {
        addr: format!("127.0.0.1:{port}"),
        allowed_hosts,
        server_name: "test-acl-server".into(),
    };
    let server = Server::new(config);
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(());
    let handle = tokio::spawn(async move { server.run(shutdown_rx).await });
    (handle, shutdown_tx)
}
```

Remove the `TempDir` parameter and `use tempfile::TempDir;` import from the test module (if only used for socket path). Update the three ACL test functions to not create a `TempDir`:

```rust
#[tokio::test]
async fn tcp_acl_empty_allows_all() {
    let port = available_port();
    let (_handle, shutdown_tx) = spawn_tcp_server(port, vec![]);
    assert!(tcp_handshake_succeeds(port).await, "Empty allowed_hosts should allow all");
    let _ = shutdown_tx.send(());
}

#[tokio::test]
async fn tcp_acl_allows_matching_ip() {
    let port = available_port();
    let (_handle, shutdown_tx) = spawn_tcp_server(port, vec!["127.0.0.1".into()]);
    assert!(tcp_handshake_succeeds(port).await, "Matching IP should be allowed");
    let _ = shutdown_tx.send(());
}

#[tokio::test]
async fn tcp_acl_rejects_non_matching_ip() {
    let port = available_port();
    let (_handle, shutdown_tx) = spawn_tcp_server(port, vec!["10.0.0.1".into()]);
    assert!(!tcp_handshake_succeeds(port).await, "Non-matching IP should be rejected");
    let _ = shutdown_tx.send(());
}
```

- [ ] **Step 5: Run `cargo test -p shore-daemon`**

Expected: all server tests pass. The handler tests (handshake, routing, broadcast) use `duplex` streams and are transport-agnostic — they should pass without changes.

- [ ] **Step 6: Commit**

```bash
git add shore-daemon/src/server/mod.rs
git commit -m "refactor(server): remove Unix socket listener, TCP-only"
```

---

### Task 4: Client connection — Remove `ServerAddr::Unix`

**Files:**
- Modify: `shore-client/src/connection.rs`
- Modify: `shore-client/Cargo.toml`

- [ ] **Step 1: Remove `ServerAddr::Unix` variant and Unix stream code**

Replace `ServerAddr`:
```rust
/// Address to connect to — a TCP host:port.
#[derive(Debug, Clone)]
pub struct ServerAddr(pub String);
```

Update `SWPConnection::open`:
```rust
async fn open(addr: &ServerAddr) -> Result<Self> {
    debug!(addr = %addr.0, "connecting via tcp");
    let stream = TcpStream::connect(&addr.0).await.map_err(|e| {
        error!(addr = %addr.0, error = %e, "tcp connect failed");
        ClientError::Connect(format!("tcp:{}: {e}", addr.0))
    })?;
    let (r, w) = stream.into_split();
    debug!(addr = %addr.0, "tcp connected");
    Ok(Self {
        reader: BufReader::new(Box::new(tokio::io::join(r, tokio::io::sink()))),
        writer: BufWriter::new(Box::new(tokio::io::join(tokio::io::empty(), w))),
    })
}
```

- [ ] **Step 2: Remove `is_unix_path()` function and `UnixStream` import**

Remove:
```rust
#[cfg(unix)]
use tokio::net::UnixStream;
```

Remove:
```rust
pub fn is_unix_path(s: &str) -> bool { ... }
```

- [ ] **Step 3: Remove `libc` dependency from `shore-client/Cargo.toml`**

Remove the entire section:
```toml
[target.'cfg(unix)'.dependencies]
libc = "0.2"
```

- [ ] **Step 4: Run `cargo check -p shore-client`**

This will fail with downstream compile errors from `discovery.rs` and `conn_manager.rs` referencing the removed items — that's expected, those are fixed in the next tasks.

- [ ] **Step 5: Commit**

```bash
git add shore-client/src/connection.rs shore-client/Cargo.toml
git commit -m "refactor(client): remove ServerAddr::Unix, TCP-only connection"
```

---

### Task 5: Client discovery — TCP-only resolution

**Files:**
- Modify: `shore-client/src/discovery.rs`

- [ ] **Step 1: Update `InstanceEntry` to use `addr` instead of `socket_path`**

```rust
#[derive(Deserialize, Debug, Clone)]
pub struct InstanceEntry {
    #[serde(default)]
    pub id: Option<String>,
    /// TCP address where the daemon listens (e.g. "127.0.0.1:7320").
    pub addr: String,
    #[serde(default)]
    pub pid: Option<u32>,
    #[serde(default)]
    pub data_dir: Option<String>,
}
```

- [ ] **Step 2: Simplify `discover()` — return `ServerAddr` directly**

```rust
pub fn discover(config_path: Option<&str>) -> Result<ServerAddr> {
    let entries = read_instances()?;

    let entry = match config_path {
        Some(wanted) => entries
            .iter()
            .find(|e| e.id.as_deref() == Some(wanted))
            .ok_or_else(|| ClientError::Discovery(format!("no daemon found for id: {wanted}")))?,
        None => entries
            .first()
            .ok_or_else(|| ClientError::Discovery("instances file is empty".into()))?,
    };

    Ok(ServerAddr(entry.addr.clone()))
}
```

- [ ] **Step 3: Remove `addr_from_socket()` and `default_socket_path()`**

Delete both functions entirely.

- [ ] **Step 4: Update `discover_or_default()` — fall back to `127.0.0.1:7320`**

```rust
/// Default TCP address when no instances file or client.toml is found.
pub const DEFAULT_ADDR: &str = "127.0.0.1:7320";

pub fn discover_or_default(config_path: Option<&str>) -> ServerAddr {
    // 1. Check client.toml for a default_address.
    if let Some(cfg) = crate::client_config::load_client_config() {
        if let Some(addr) = &cfg.default_address {
            debug!(addr = %addr, "using address from client.toml");
            return ServerAddr(addr.clone());
        }
    }

    // 2. Instance discovery (optionally filtered by --config ID).
    match discover(config_path) {
        Ok(addr) => {
            debug!(addr = ?addr, "resolved daemon via instance discovery");
            addr
        }
        Err(e) => {
            warn!(error = %e, fallback = DEFAULT_ADDR, "instance discovery failed, using default address");
            ServerAddr(DEFAULT_ADDR.to_string())
        }
    }
}
```

- [ ] **Step 5: Update tests**

Replace all `socket_path` references in tests:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_alive_no_pid_assumes_alive() {
        let entry = InstanceEntry {
            id: None,
            addr: "127.0.0.1:7320".into(),
            pid: None,
            data_dir: None,
        };
        assert!(entry_alive(&entry));
    }

    #[test]
    fn entry_alive_current_process() {
        let entry = InstanceEntry {
            id: None,
            addr: "127.0.0.1:7320".into(),
            pid: Some(std::process::id()),
            data_dir: None,
        };
        assert!(entry_alive(&entry));
    }

    #[test]
    fn entry_alive_bogus_pid() {
        let entry = InstanceEntry {
            id: None,
            addr: "127.0.0.1:7320".into(),
            pid: Some(u32::MAX - 1),
            data_dir: None,
        };
        assert!(!entry_alive(&entry));
    }

    #[test]
    fn instance_entry_full_fields() {
        let json = r#"[{
            "id": "default",
            "addr": "127.0.0.1:7320",
            "pid": 12345,
            "data_dir": "/home/user/data"
        }]"#;
        let entries: Vec<InstanceEntry> = serde_json::from_str(json).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].id.as_deref(), Some("default"));
        assert_eq!(entries[0].addr, "127.0.0.1:7320");
        assert_eq!(entries[0].pid, Some(12345));
        assert_eq!(entries[0].data_dir.as_deref(), Some("/home/user/data"));
    }

    #[test]
    fn instance_entry_minimal_fields_use_defaults() {
        let json = r#"[{"addr": "127.0.0.1:7320"}]"#;
        let entries: Vec<InstanceEntry> = serde_json::from_str(json).unwrap();
        assert_eq!(entries.len(), 1);
        assert!(entries[0].id.is_none());
        assert!(entries[0].pid.is_none());
        assert_eq!(entries[0].addr, "127.0.0.1:7320");
    }
}
```

Remove the `addr_from_socket_*` tests entirely.

- [ ] **Step 6: Run `cargo check -p shore-client`**

Expected: compiles (conn_manager.rs will fail separately — next task).

- [ ] **Step 7: Commit**

```bash
git add shore-client/src/discovery.rs
git commit -m "refactor(discovery): TCP-only instance resolution, remove Unix fallback"
```

---

### Task 6: Client conn_manager — Simplify `resolve_addr`

**Files:**
- Modify: `shore-client/src/conn_manager.rs`

- [ ] **Step 1: Simplify `resolve_addr`**

```rust
fn resolve_addr(addr: &Option<String>, config: &Option<String>) -> ServerAddr {
    if let Some(addr) = addr {
        return ServerAddr(addr.clone());
    }
    discover_or_default(config.as_deref())
}
```

- [ ] **Step 2: Rename `socket` parameter to `addr` in `spawn_connection` and `connection_loop`**

In `spawn_connection`:
```rust
pub fn spawn_connection(
    addr: Option<String>,
    config: Option<String>,
    client_id: &str,
    app_name: &str,
    character: Option<String>,
) -> (mpsc::Sender<ConnCommand>, mpsc::Receiver<ConnEvent>) {
```

And in `connection_loop`:
```rust
async fn connection_loop(
    addr: Option<String>,
    config: Option<String>,
    ...
```

Update the call to `resolve_addr` inside `connection_loop` accordingly (rename `socket` → `addr`).

- [ ] **Step 3: Update tests**

```rust
#[test]
fn test_resolve_addr_explicit_tcp() {
    let addr = resolve_addr(&Some("127.0.0.1:9090".into()), &None);
    assert_eq!(addr.0, "127.0.0.1:9090");
}
```

Remove `test_resolve_addr_explicit_unix`.

- [ ] **Step 4: Run `cargo test -p shore-client`**

Expected: all client tests pass. Some tests in `lib.rs` will need updating (next task).

- [ ] **Step 5: Commit**

```bash
git add shore-client/src/conn_manager.rs
git commit -m "refactor(conn_manager): simplify to TCP-only address resolution"
```

---

### Task 7: Client lib tests and client_config — Update

**Files:**
- Modify: `shore-client/src/lib.rs`
- Modify: `shore-client/src/client_config.rs`

- [ ] **Step 1: Remove `is_unix_path_detection` and `client_config_parses_unix_address` tests from `lib.rs`**

Delete the two test functions entirely.

- [ ] **Step 2: Update `client_config.rs` doc comment**

Change:
```rust
/// Default server address (Unix socket path or `host:port`).
```
To:
```rust
/// Default server address (`host:port`).
```

- [ ] **Step 3: Run `cargo test -p shore-client`**

Expected: all tests pass.

- [ ] **Step 4: Commit**

```bash
git add shore-client/src/lib.rs shore-client/src/client_config.rs
git commit -m "refactor(client): remove Unix socket tests, update docs"
```

---

### Task 8: Daemon main — TCP-only startup

**Files:**
- Modify: `shore-daemon/src/main.rs:49-91`

- [ ] **Step 1: Replace socket path + TCP config resolution with simple addr resolution**

Replace lines 49–91 (from `let instance_id` through `registry.register`) with:

```rust
let instance_id = uuid::Uuid::new_v4().to_string();

// Resolve listen address: config → SHORE_ADDR env → default.
let addr = std::env::var("SHORE_ADDR")
    .unwrap_or_else(|_| loaded.app.daemon.addr.clone());

let server_config = ServerConfig {
    addr: addr.clone(),
    allowed_hosts: loaded.app.daemon.allowed_hosts.clone(),
    server_name: "shore-daemon".into(),
};

// Register instance.
let registry = Registry::default_path();
let instance_info = InstanceInfo {
    id: instance_id.clone(),
    pid: std::process::id(),
    addr,
    started_at: epoch_timestamp(),
    data_dir: Some(loaded.dirs.data.display().to_string()),
};
registry.register(instance_info)?;
info!(instance_id = %instance_id, "Registered daemon instance");
```

- [ ] **Step 2: Remove unused imports**

Remove: `use std::path::PathBuf;` if no longer used (check — it's still used for `config_path`).
Remove: `use shore_daemon::server::ServerConfig;` is already imported, just ensure `TcpConfig` is not referenced.

- [ ] **Step 3: Run `cargo build -p shore-daemon`**

Expected: compiles successfully.

- [ ] **Step 4: Commit**

```bash
git add shore-daemon/src/main.rs
git commit -m "refactor(daemon): TCP-only startup, remove socket path logic"
```

---

### Task 9: CLI — Rename `--socket` to `--addr`

**Files:**
- Modify: `shore-cli/src/cli.rs`
- Modify: `shore-cli/src/run.rs`

- [ ] **Step 1: Rename field in `Cli` struct**

```rust
/// TCP address of the daemon (overrides discovery)
#[arg(long, global = true)]
pub addr: Option<String>,
```

- [ ] **Step 2: Update `resolve_addr` in `run.rs`**

```rust
fn resolve_addr(cli: &Cli) -> ServerAddr {
    if let Some(addr) = &cli.addr {
        return ServerAddr(addr.clone());
    }
    shore_client::discover_or_default(cli.config.as_deref())
}
```

- [ ] **Step 3: Update `handle_matrix_command` to pass `--addr` instead of `--socket`**

```rust
if let Some(ref addr) = cli.addr {
    cmd.arg("--addr").arg(addr);
}
```

- [ ] **Step 4: Update CLI test `test_cli` helper and `parse_global_socket_flag` test**

Rename `socket` → `addr` in test helper:
```rust
fn test_cli(command: CliCommand) -> Cli {
    Cli {
        addr: None,
        config: None,
        character: None,
        no_color: false,
        command,
    }
}
```

Rename test:
```rust
#[test]
fn parse_global_addr_flag() {
    let cli = parse(&["--addr", "127.0.0.1:7320", "status"]);
    assert_eq!(cli.addr.as_deref(), Some("127.0.0.1:7320"));
    assert!(matches!(cli.command, CliCommand::Status { .. }));
}
```

- [ ] **Step 5: Run `cargo test -p shore-cli`**

Expected: all tests pass.

- [ ] **Step 6: Commit**

```bash
git add shore-cli/src/cli.rs shore-cli/src/run.rs
git commit -m "refactor(cli): rename --socket to --addr"
```

---

### Task 10: TUI and Matrix — Rename `--socket` to `--addr`

**Files:**
- Modify: `shore-tui/src/main.rs:32-34`
- Modify: `shore-tui/src/connection.rs`
- Modify: `shore-matrix/src/main.rs:55-57`
- Modify: `shore-matrix/src/connection.rs`

- [ ] **Step 1: Update TUI `Cli` struct**

```rust
/// TCP address of the daemon
#[arg(long)]
addr: Option<String>,
```

Update the call site in `main.rs` where `cli.socket` is used → `cli.addr`.

- [ ] **Step 2: Update TUI `connection.rs`**

Rename parameter from `socket` to `addr`:
```rust
pub fn spawn_connection(
    addr: Option<String>,
    config: Option<String>,
    character: Option<String>,
) -> ( ... ) {
    shore_client::spawn_connection(addr, config, "tui", "shore-tui", character)
}
```

- [ ] **Step 3: Update Matrix `Cli` struct**

```rust
/// Daemon address (host:port)
#[arg(long)]
addr: Option<String>,
```

Update all `args.socket` → `args.addr` references in `main.rs`.

- [ ] **Step 4: Update Matrix `connection.rs`**

Rename parameter from `socket` to `addr`:
```rust
pub fn spawn_connection(
    addr: Option<String>,
    config: Option<String>,
) -> ( ... ) {
    shore_client::spawn_connection(addr, config, "bridge", "shore-matrix", None)
}
```

- [ ] **Step 5: Run `cargo build --workspace`**

Expected: full workspace compiles.

- [ ] **Step 6: Commit**

```bash
git add shore-tui/src/main.rs shore-tui/src/connection.rs shore-matrix/src/main.rs shore-matrix/src/connection.rs
git commit -m "refactor(tui,matrix): rename --socket to --addr"
```

---

### Task 11: Example configs and docs

**Files:**
- Modify: `examples/config.toml`
- Modify: `examples/client.toml`
- Modify: `docs/ARCHITECTURE.md`
- Modify: `docs/DECISIONS.md`

- [ ] **Step 1: Update `examples/config.toml`**

Replace the `[daemon]` section:
```toml
# [daemon]
# addr = "127.0.0.1:7320"     # Default listen address
# allowed_hosts = []            # Empty = allow all; list IPs to restrict
```

Remove the `[connections.tcp]` section entirely.

- [ ] **Step 2: Update `examples/client.toml`**

```toml
# Shore V2 — Example Client Configuration
#
# Place this file at $XDG_CONFIG_HOME/shore/client.toml
# (typically ~/.config/shore/client.toml).
#
# This is read by shore (CLI), shore-tui, and any other SWP client.
# It is intentionally separate from the daemon's config.toml.

# Default server address. Set this on remote machines to point at
# the daemon's TCP listener (host:port).
#
# The --addr CLI flag always overrides this value.
#
# default_address = "192.168.1.50:7320"
```

- [ ] **Step 3: Update `docs/ARCHITECTURE.md`**

Update the discovery resolution order (around line 724) from:
```
1. --socket CLI flag
2. client.toml default_address
3. Instance discovery (instances.json)
4. Default Unix socket
```
To:
```
1. --addr CLI flag
2. client.toml default_address
3. Instance discovery (instances.json)
4. Default: 127.0.0.1:7320
```

Update the intro text (around line 14) to remove "Unix sockets or TCP" → "TCP".

- [ ] **Step 4: Update `docs/DECISIONS.md`**

Add a new decision entry:
```markdown
### Unix sockets removed — TCP-only transport

**Date:** 2026-04-10

**Decision:** Remove Unix socket support entirely. TCP is the sole transport.

**Rationale:** Unix sockets added complexity (socket path management, stale file cleanup, dual-listener code) with no real benefit over TCP on localhost. For remote clients, identifying an instance by Unix socket path on another machine is meaningless. TCP was already a core feature, so making it the only transport simplifies the codebase and makes instance identity uniform (`host:port`).

**Default:** `127.0.0.1:7320` (localhost-only). `allowed_hosts` whitelist for remote access.

**Trade-offs:** Marginally higher per-message overhead vs Unix sockets on localhost (negligible for JSON-Lines messages). Lost the ability to enforce filesystem-level permissions on the socket file — mitigated by `allowed_hosts` ACL and localhost-only default.
```

Also update the existing discovery resolution order entry (around line 274).

- [ ] **Step 5: Commit**

```bash
git add examples/config.toml examples/client.toml docs/ARCHITECTURE.md docs/DECISIONS.md
git commit -m "docs: update for TCP-only transport"
```

---

### Task 12: Full verification

- [ ] **Step 1: Run full workspace build**

```bash
cargo build --workspace --release
```

Expected: clean build, no warnings.

- [ ] **Step 2: Run full test suite**

```bash
cargo test --workspace
```

Expected: all tests pass.

- [ ] **Step 3: Run type check**

```bash
cargo check --workspace
```

Expected: no errors.

- [ ] **Step 4: Grep for any remaining Unix socket references**

Search for leftover references that should have been removed:

```bash
rg -i "unix.?sock|\.sock|socket_path|UnixListener|UnixStream|is_unix_path" --type rust
```

Ignore any hits in comments explaining the removal or in unrelated code (like `kitty_diag.rs` terminal I/O).

- [ ] **Step 5: Live test — start daemon and connect**

```bash
cargo run --bin shore-daemon &
sleep 1
cargo run --bin shore -- status
```

Confirm: daemon binds TCP on `127.0.0.1:7320`, CLI connects and returns status.

- [ ] **Step 6: Live test — custom port**

```bash
SHORE_ADDR=127.0.0.1:8888 cargo run --bin shore-daemon &
sleep 1
cargo run --bin shore -- --addr 127.0.0.1:8888 status
```

Confirm: daemon binds on custom port, CLI connects via `--addr`.
