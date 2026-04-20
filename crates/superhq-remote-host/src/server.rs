//! `RemoteServer` — thin wrapper around an iroh `Endpoint` + `Router` that
//! accepts peer connections and spawns a session task for each.

use std::sync::Arc;

use anyhow::Result;
use iroh::{
    endpoint::Connection,
    protocol::{AcceptError, ProtocolHandler, Router},
    Endpoint, EndpointId,
};
use superhq_remote_proto::ALPN;
use tracing::{error, info};

use crate::handler::RemoteHandler;

/// Host-side remote-control server. Holds the iroh endpoint, a router
/// registered for the `superhq/remote/1` ALPN, and the handler.
#[derive(Clone)]
pub struct RemoteServer {
    router: Router,
}

impl RemoteServer {
    /// Spawn a new server with the given handler.
    pub async fn spawn<H: RemoteHandler>(handler: H) -> Result<Self> {
        Self::spawn_arc(Arc::new(handler)).await
    }

    /// Spawn a new server with a pre-wrapped `Arc<Handler>`, so the caller
    /// can share the same handler with other parts of the app (UI, state
    /// observers, etc.).
    pub async fn spawn_arc<H: RemoteHandler>(handler: Arc<H>) -> Result<Self> {
        let endpoint = Endpoint::bind().await?;
        Self::spawn_with_endpoint_arc(endpoint, handler).await
    }

    /// Spawn using a pre-built endpoint (for tests that share one).
    pub async fn spawn_with_endpoint<H: RemoteHandler>(
        endpoint: Endpoint,
        handler: H,
    ) -> Result<Self> {
        Self::spawn_with_endpoint_arc(endpoint, Arc::new(handler)).await
    }

    pub async fn spawn_with_endpoint_arc<H: RemoteHandler>(
        endpoint: Endpoint,
        handler: Arc<H>,
    ) -> Result<Self> {
        let proto = RemoteProtocol { handler };
        let router = Router::builder(endpoint).accept(ALPN, proto).spawn();
        Ok(Self { router })
    }

    pub fn endpoint_id(&self) -> EndpointId {
        self.router.endpoint().id()
    }

    pub fn endpoint(&self) -> &Endpoint {
        self.router.endpoint()
    }

    pub async fn shutdown(self) -> Result<()> {
        self.router.shutdown().await?;
        Ok(())
    }
}

/// The `ProtocolHandler` impl registered with the iroh `Router`.
#[derive(Clone)]
struct RemoteProtocol<H: RemoteHandler> {
    handler: Arc<H>,
}

impl<H: RemoteHandler> std::fmt::Debug for RemoteProtocol<H> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteProtocol").finish()
    }
}

impl<H: RemoteHandler> ProtocolHandler for RemoteProtocol<H> {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        info!(remote = %connection.remote_id(), "remote-host: protocol accept");
        let handler = self.handler.clone();
        // Drive the session in a dedicated task so the accept future
        // returns quickly.
        tokio::spawn(async move {
            match crate::session::drive_connection(connection, handler).await {
                Ok(()) => {}
                Err(e) => {
                    // "connection lost" is expected when the peer drops
                    // without calling session.close — downgrade to debug.
                    let s = e.to_string();
                    if s.contains("connection lost") || s.contains("application closed") {
                        tracing::debug!(error = %e, "remote-host: session ended");
                    } else {
                        error!(error = %e, "remote-host: session ended with error");
                    }
                }
            }
        });
        Ok(())
    }
}
