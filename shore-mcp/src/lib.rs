// Module structure. Everything real is debug-gated in main.rs; this lib.rs
// exists so `cargo test -p shore-mcp --features enabled` can find tests in
// each module file.
//
// `handler` and `server` additionally require the `enabled` feature because
// they depend on `rmcp` (optional dep). Without `enabled`, `cargo check
// --workspace` skips them and only compiles the feature-agnostic modules.

#[cfg(debug_assertions)]
pub mod cli;
#[cfg(debug_assertions)]
pub mod gating;
#[cfg(all(debug_assertions, feature = "enabled"))]
pub mod handler;
#[cfg(debug_assertions)]
pub mod profile;
#[cfg(all(debug_assertions, feature = "enabled"))]
pub mod server;
#[cfg(all(debug_assertions, feature = "enabled"))]
pub mod tools;
