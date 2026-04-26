// shore-mcp is a debug/testing-only binary. In release builds `debug_assertions`
// is off by default, so the binary becomes a stub that refuses to run. Set a
// custom profile with `debug-assertions = true` if you really want a release build.

#[cfg(not(debug_assertions))]
fn main() {
    eprintln!(
        "shore-mcp is only available in debug builds. \
         Rebuild with `cargo build -p shore-mcp` (default dev profile) \
         or a custom profile with `debug-assertions = true`."
    );
    std::process::exit(1);
}

#[cfg(debug_assertions)]
mod cli;
#[cfg(debug_assertions)]
mod gating;
#[cfg(debug_assertions)]
mod handler;
#[cfg(debug_assertions)]
mod profile;
#[cfg(debug_assertions)]
mod server;
#[cfg(debug_assertions)]
mod tools;

#[cfg(debug_assertions)]
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("shore_mcp=info")),
        )
        .with_writer(std::io::stderr) // stdout is reserved for JSON-RPC
        .init();

    server::run().await
}
