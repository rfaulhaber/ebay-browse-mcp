//! Entry point. Speaks MCP over stdio by default; setting
//! `EBAY_MCP_HTTP_ADDR` switches to the remote transport — Streamable HTTP
//! behind the OAuth gate — for use as a self-hosted Claude.ai connector.

use std::process::ExitCode;
use std::sync::Arc;

use ebay_browse_mcp::config::HttpConfig;
use ebay_browse_mcp::oauth::OauthCore;
use ebay_browse_mcp::{Config, EbayBrowseServer, remote};
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::{StreamableHttpServerConfig, StreamableHttpService};
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
    #[error("could not bind the HTTP listener: {0}")]
    Bind(std::io::Error),
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
    let http = HttpConfig::from_env()?;
    tracing::info!(
        environment = ?config.environment,
        marketplace = %config.default_marketplace,
        "starting eBay Browse MCP server"
    );

    let server = EbayBrowseServer::new(&config)?;
    match http {
        Some(http) => serve_http(server, http).await,
        None => serve_stdio(server).await,
    }
}

async fn serve_stdio(server: EbayBrowseServer) -> Result<(), StartupError> {
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

async fn serve_http(server: EbayBrowseServer, http: HttpConfig) -> Result<(), StartupError> {
    let core = Arc::new(OauthCore::new(
        http.issuer.clone(),
        http.public_url.clone(),
        http.token_key.clone(),
        http.access_key.clone(),
    ));
    // One eBay client shared across sessions: cloning the server clones its
    // `Arc`s, so every connection reuses the same token cache.
    let mcp_service = StreamableHttpService::new(
        move || Ok(server.clone()),
        Arc::new(LocalSessionManager::default()),
        StreamableHttpServerConfig::default().with_allowed_hosts([
            http.public_host.as_str(),
            "localhost",
            "127.0.0.1",
            "::1",
        ]),
    );
    let app = remote::router(core, &http.mcp_path, mcp_service);

    let listener = tokio::net::TcpListener::bind(http.bind)
        .await
        .map_err(StartupError::Bind)?;
    tracing::info!(
        bind = %http.bind,
        public_url = %http.public_url,
        issuer = %http.issuer,
        "serving MCP over streamable HTTP behind OAuth"
    );
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .map_err(|err| StartupError::Serve(err.to_string()))?;
    tracing::info!("eBay Browse MCP server stopped");

    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(error) = tokio::signal::ctrl_c().await {
            tracing::warn!(%error, "could not install the Ctrl-C handler");
            std::future::pending::<()>().await;
        }
    };
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(error) => {
                tracing::warn!(%error, "could not install the SIGTERM handler");
                std::future::pending::<()>().await;
            }
        }
    };
    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }
    tracing::info!("shutdown signal received");
}
