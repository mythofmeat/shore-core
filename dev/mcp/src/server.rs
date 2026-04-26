use clap::Parser;
use rmcp::ServiceExt;

use crate::cli::Cli;
use crate::handler::ShoreMcpHandler;
use crate::profile;

pub async fn run() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let resolved = profile::resolve_profile(cli.clone())?;
    let profile_is_test = resolved.is_test();

    tracing::info!(
        kind = ?resolved.kind,
        profile_is_test,
        allow_main_writes = cli.allow_main_writes,
        "resolved shore-mcp profile"
    );

    let conn = profile::attach(&resolved, &cli).await?;
    let handler = ShoreMcpHandler::new(conn, &cli, profile_is_test);

    // rmcp 1.4 stdio API:
    //   `rmcp::transport::stdio()` returns `(tokio::io::Stdin, tokio::io::Stdout)`.
    //   `ServiceExt::serve` accepts `IntoTransport`, which auto-converts that tuple.
    //   `serve()` returns `RunningService` after MCP initialize succeeds; `.waiting()`
    //   blocks until the client closes the connection.
    let service = handler
        .serve(rmcp::transport::stdio())
        .await
        .map_err(|e| anyhow::anyhow!("rmcp stdio service init failed: {e}"))?;

    service
        .waiting()
        .await
        .map_err(|e| anyhow::anyhow!("rmcp stdio service join failed: {e}"))?;

    // Keep the ephemeral tempdir alive for the lifetime of the server.
    drop(resolved);

    Ok(())
}
