//! Remote control feature: host-side glue.
//!
//! Owns the lifecycle of a `superhq_remote_host::RemoteServer`, connects
//! the app's state (workspaces, tabs, terminals) to the remote protocol
//! via `AppHandler`.
//!
//! State sharing between GPUI (main thread) and the handler's async tasks
//! goes through a single `Arc<RwLock<RemoteStateSnapshot>>`: GPUI pushes
//! updates whenever relevant state changes; the handler reads on each
//! request. No cross-runtime dance.

mod approval;
mod audit;
mod button;
mod commands;
mod handler;
mod pairing;
pub mod pty_bus;
mod snapshot;

pub use approval::{PairingApprovalRequest, PairingApprover};
pub use audit::AuditLog;
pub use commands::{HostCommand, HostCommandDispatcher};
pub use button::{
    render_titlebar_button, ManageDevicesCallback, RemotePopoverState, ToggleEnabledCallback,
};
pub use handler::AppHandler;
pub use pairing::{PairedDevice, PairingStore};
pub use pty_bus::{new_pty_map, PtyBus, PtyMap};
pub use snapshot::{build_agent_infos, build_snapshot, RemoteStateSnapshot};

use std::sync::{Arc, RwLock};

use anyhow::Result;
use superhq_remote_host::{Endpoint, RemoteServer, SecretKey};

use crate::db::Database;

pub struct RemoteAccess {
    server: Option<RemoteServer>,
    tokio_handle: tokio::runtime::Handle,
    state: Arc<RwLock<RemoteStateSnapshot>>,
    db: Arc<Database>,
    /// Broadcast channel for host → client push notifications. Each
    /// connected session subscribes; `push_snapshot` fans out a
    /// `snapshot.invalidated` when the snapshot actually changes.
    notifications: tokio::sync::broadcast::Sender<Arc<String>>,
    /// Hash of the last pushed snapshot — used to debounce
    /// notifications when the render cycle pushes structurally
    /// identical snapshots on rapid re-renders.
    last_snapshot_hash: std::sync::Mutex<Option<u64>>,
}

impl RemoteAccess {
    pub fn new(
        tokio_handle: tokio::runtime::Handle,
        db: Arc<Database>,
    ) -> Self {
        let (notifications, _) = tokio::sync::broadcast::channel(16);
        Self {
            server: None,
            tokio_handle,
            state: Arc::new(RwLock::new(RemoteStateSnapshot::default())),
            db,
            notifications,
            last_snapshot_hash: std::sync::Mutex::new(None),
        }
    }

    /// Handle for the `AppHandler` constructor so it can return a
    /// fresh subscriber per connection.
    pub fn notifications_sender(
        &self,
    ) -> tokio::sync::broadcast::Sender<Arc<String>> {
        self.notifications.clone()
    }

    pub fn is_running(&self) -> bool {
        self.server.is_some()
    }

    pub fn endpoint_id(&self) -> Option<String> {
        self.server.as_ref().map(|s| s.endpoint_id().to_string())
    }

    /// The host's stable id — derived from the persisted secret even
    /// when the server is currently stopped. Returns `None` only on
    /// first launch before any secret has been generated, or if the
    /// DB read fails.
    pub fn host_id(&self) -> Option<String> {
        if let Some(server) = &self.server {
            return Some(server.endpoint_id().to_string());
        }
        match self.db.get_remote_endpoint_secret() {
            Ok(Some(bytes)) => {
                Some(SecretKey::from_bytes(&bytes).public().to_string())
            }
            _ => None,
        }
    }

    /// A clone of the shared state handle — used by `AppHandler` to read
    /// snapshots without touching GPUI entities from async code.
    pub fn state_handle(&self) -> Arc<RwLock<RemoteStateSnapshot>> {
        self.state.clone()
    }

    /// Replace the current snapshot. Cheap — short-held write lock.
    /// Called by GPUI render / state-change code. Also fans out a
    /// `snapshot.invalidated` notification to any connected client,
    /// but only when the snapshot's content actually changed (the
    /// render cycle fires on cosmetic events too).
    pub fn push_snapshot(&self, snapshot: RemoteStateSnapshot) {
        let hash = snapshot_hash(&snapshot);
        let changed = match self.last_snapshot_hash.lock() {
            Ok(mut guard) => {
                let differs = guard.as_ref() != Some(&hash);
                if differs {
                    *guard = Some(hash);
                }
                differs
            }
            Err(_) => true,
        };
        if let Ok(mut guard) = self.state.write() {
            *guard = snapshot;
        }
        if changed {
            if let Some(line) = encode_snapshot_invalidated() {
                let _ = self.notifications.send(Arc::new(line));
            }
        }
    }

    /// Load a persisted endpoint secret key, or generate + save one on
    /// first run. Pinning the key means the iroh NodeId (and therefore
    /// the host id paired devices cache) stays constant across
    /// restarts.
    fn load_or_generate_secret(&self) -> Result<SecretKey> {
        if let Some(bytes) = self.db.get_remote_endpoint_secret()? {
            return Ok(SecretKey::from_bytes(&bytes));
        }
        let mut bytes = [0u8; 32];
        getrandom::getrandom(&mut bytes)
            .map_err(|e| anyhow::anyhow!("generate endpoint secret: {e}"))?;
        self.db.save_remote_endpoint_secret(&bytes)?;
        tracing::info!("remote-control: generated new endpoint secret");
        Ok(SecretKey::from_bytes(&bytes))
    }

    /// Start the server with the given handler. No-op if already running.
    pub fn start(&mut self, handler: Arc<AppHandler>) -> Result<()> {
        if self.server.is_some() {
            return Ok(());
        }
        let secret = self.load_or_generate_secret()?;
        let handle = self.tokio_handle.clone();
        let handler_for_server = handler.clone();
        let server = handle.block_on(async move {
            let endpoint = Endpoint::builder()
                .secret_key(secret)
                .bind()
                .await?;
            RemoteServer::spawn_with_endpoint_arc(endpoint, handler_for_server).await
        })?;
        let id = server.endpoint_id();
        // The handler needs the host NodeId for the auth HMAC transcript.
        handler.set_host_node_id(id);
        eprintln!("remote-control: server started, endpoint_id={}", id);
        tracing::info!(endpoint_id = %id, "remote-control: server started");
        self.server = Some(server);
        Ok(())
    }

    /// Detach the running server and spawn its graceful shutdown in the
    /// background. Returns immediately so the UI can flip to "stopped"
    /// without waiting for iroh to tear the endpoint down (which can
    /// take a noticeable fraction of a second on flaky networks).
    pub fn stop(&mut self) {
        if let Some(server) = self.server.take() {
            self.tokio_handle.spawn(async move {
                if let Err(e) = server.shutdown().await {
                    tracing::warn!(error = %e, "remote-control: shutdown error");
                }
                tracing::info!("remote-control: server stopped");
            });
        }
    }

    /// Rotate the endpoint secret. Stops the server, drops every paired
    /// device (their HMAC transcripts are bound to the old NodeId),
    /// wipes the stored secret, and — when `restart_handler` is provided
    /// — restarts the server with a freshly generated secret. Returns
    /// the new host id on success. Irreversible; the UI must confirm
    /// before calling.
    ///
    /// Blocks until the old endpoint is fully shut down before wiping
    /// secrets — otherwise the new id would be live while the old one
    /// was still accepting control streams, effectively leaving two
    /// endpoints up during the transition.
    pub fn rotate_endpoint_secret(
        &mut self,
        restart_handler: Option<Arc<AppHandler>>,
    ) -> Result<Option<String>> {
        if let Some(server) = self.server.take() {
            let handle = self.tokio_handle.clone();
            // Rotation's safety invariant: the old endpoint must be
            // fully shut down before the new one starts and before any
            // secrets are wiped. If shutdown fails, abort the whole
            // operation rather than proceed while the old endpoint may
            // still be accepting control streams.
            let shutdown_result =
                handle.block_on(async move { server.shutdown().await });
            if let Err(e) = shutdown_result {
                return Err(anyhow::anyhow!(
                    "aborting rotate; endpoint shutdown failed: {e}"
                ));
            }
            tracing::info!("remote-control: server stopped for rotate");
        }
        let removed = PairingStore::new(self.db.clone()).clear_all();
        tracing::info!(removed_devices = removed, "remote-control: rotating host id");
        self.db.delete_remote_endpoint_secret()?;
        if let Some(handler) = restart_handler {
            self.start(handler)?;
            return Ok(self.endpoint_id());
        }
        Ok(None)
    }
}

impl Drop for RemoteAccess {
    fn drop(&mut self) {
        self.stop();
    }
}

fn snapshot_hash(snapshot: &RemoteStateSnapshot) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    // Serialize to JSON once and hash the bytes — cheaper than
    // hand-derived Hash impls that would need to chase nested enums
    // (AgentState, TabKind) to stay in sync with proto changes.
    if let Ok(s) = serde_json::to_string(snapshot_wire(snapshot)) {
        s.hash(&mut h);
    }
    h.finish()
}

fn snapshot_wire(s: &RemoteStateSnapshot) -> &RemoteStateSnapshot {
    s
}

fn encode_snapshot_invalidated() -> Option<String> {
    use superhq_remote_proto::{encode_notification, notifications::SNAPSHOT_INVALIDATED};
    let note = superhq_remote_proto::Notification::new(
        SNAPSHOT_INVALIDATED,
        serde_json::Value::Object(Default::default()),
    );
    encode_notification(&note).ok()
}
