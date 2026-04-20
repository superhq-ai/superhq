//! Host-side transport for SuperHQ remote control.
//!
//! Spawns an iroh `Endpoint` that accepts peer connections on the
//! `superhq/remote/1` ALPN, drives the control-stream JSON-RPC loop, and
//! dispatches calls to an application-provided [`RemoteHandler`].

pub mod auth;
pub mod handler;
pub mod server;
pub mod session;

pub use auth::{compute_proof, generate_device_key, now_secs, verify_proof, AuthError};
pub use handler::{RemoteHandler, StubHandler};
pub use server::RemoteServer;

// Re-export the iroh stream types + SecretKey / Endpoint so consumers
// implementing `RemoteHandler` (and persisting the endpoint identity)
// don't need to add iroh as a direct dependency.
pub use iroh::endpoint::{RecvStream, SendStream};
pub use iroh::{Endpoint, EndpointId, SecretKey};
