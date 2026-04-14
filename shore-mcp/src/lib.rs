// Module structure. Everything real is debug-gated in main.rs; this lib.rs
// exists so `cargo test -p shore-mcp --features enabled` can find tests in
// each module file.

#[cfg(debug_assertions)]
pub mod cli;
#[cfg(debug_assertions)]
pub mod gating;
#[cfg(debug_assertions)]
pub mod handler;
#[cfg(debug_assertions)]
pub mod profile;
#[cfg(debug_assertions)]
pub mod server;
#[cfg(debug_assertions)]
pub mod tools;
