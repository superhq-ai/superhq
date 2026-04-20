//! Minimal demo host. Spawns a `RemoteServer` with a `StubHandler`, prints
//! its EndpointId, and runs until Ctrl-C. Use alongside the web demo at
//! `crates/superhq-remote-client/demo/` for browser-against-real-host
//! validation.

use anyhow::Result;
use superhq_remote_host::{RemoteServer, StubHandler};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,iroh=warn".into()),
        )
        .init();

    let server = RemoteServer::spawn(StubHandler::default()).await?;
    server.endpoint().online().await;
    println!("================================================================");
    println!("SuperHQ remote demo server");
    println!("EndpointId: {}", server.endpoint_id());
    println!("================================================================");
    println!("Press Ctrl+C to stop.");

    tokio::signal::ctrl_c().await?;
    println!("\nShutting down...");
    server.shutdown().await?;
    Ok(())
}
