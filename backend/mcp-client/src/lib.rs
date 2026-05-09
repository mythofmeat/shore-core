//! Outbound MCP client for Shore.
//!
//! Spawns and supervises external MCP server subprocesses, performs the
//! initialize handshake, discovers their tools, and routes `tools/call`
//! requests. Holds **no** policy — the daemon-side registry enforces
//! allowlists, the `destructiveHint` rule, and naming.
//!
//! Lifecycle is one supervised task per configured server. Restarts use
//! exponential backoff with the same shape as `backend/daemon/src/supervisor.rs`.

mod client;
mod error;
mod supervisor;

pub use client::{Client, RemoteToolDef};
pub use error::OutboundClientError;
pub use supervisor::{spawn_server, ServerHandle, ServerSpawnSpec};
