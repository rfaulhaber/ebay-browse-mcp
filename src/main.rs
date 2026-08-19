//! Entry point for the stdio MCP server.

use std::process::ExitCode;

use ebay_browse_mcp::{Config, EbayBrowseServer};
use rmcp::{ServiceExt, transport::stdio};
use tracing_subscriber::EnvFilter;

#[derive(Debug, thiserror::Error)]
enum StartupError {
    #[error("{0}")]
    Config(#[from] ebay_browse_mcp::config::ConfigError),
    #[error(
        "could not initialise the HTTP client: {0}. If this mentions CA certificates, \
         the process has no system trust store — set SSL_CERT_FILE to a CA bundle in \
         the environment the MCP client launches this server from"
    )]
    Http(#[from] reqwest::Error),
    #[error("could not start the MCP server: {0}")]
    Serve(String),
}

/// Returning [`ExitCode`] rather than `Result` so failures are reported through
/// the configured logger. A `Result`-returning `main` prints the error's
/// `Debug` form, which would show `Config(MissingVar("EBAY_CLIENT_ID"))` in
/// place of the message explaining how to fix it.
#[tokio::main]
async fn main() -> ExitCode {
    // stdout carries the JSON-RPC stream, so every diagnostic must go to
    // stderr. The crate denies `print_stdout`/`print_stderr` outright, which
    // makes that structural rather than a matter of discipline.
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_env("EBAY_MCP_LOG").unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();

    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            tracing::error!("{error}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<(), StartupError> {
    let config = Config::from_env()?;
    tracing::info!(
        environment = ?config.environment,
        marketplace = %config.default_marketplace,
        "starting eBay Browse MCP server"
    );

    let server = EbayBrowseServer::new(&config)?;
    let running = server
        .serve(stdio())
        .await
        .map_err(|err| StartupError::Serve(err.to_string()))?;

    // Resolves when the client disconnects or closes the transport.
    let reason = running
        .waiting()
        .await
        .map_err(|err| StartupError::Serve(err.to_string()))?;
    tracing::info!(?reason, "eBay Browse MCP server stopped");

    Ok(())
}
