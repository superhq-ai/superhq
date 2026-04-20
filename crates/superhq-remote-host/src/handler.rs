//! The trait the application provides to answer remote RPC calls.
//!
//! A stub implementation is provided for testing.

use async_trait::async_trait;
use iroh::endpoint::{RecvStream, SendStream};
use superhq_remote_proto::{
    error_code,
    methods::{
        HostInfo, PairingRequestParams, PairingRequestResult, PtyAttachParams,
        PtyAttachResult, PtyDetachParams, PtyResizeParams, SessionHelloParams,
        SessionHelloResult, TabsCloseParams, TabsCreateParams, TabsCreateResult,
        TabsListResult, WorkspaceActivateParams, WorkspaceActivateResult,
        WorkspacesListResult,
    },
    types::{TabId, WorkspaceId},
    RpcError,
};

/// Application-side answerer for remote control RPC.
///
/// Each connected client gets its own handler reference (or shares one
/// behind an `Arc`, up to the caller). Methods return `Result<T, RpcError>`
/// where `RpcError` will be serialized back to the peer verbatim.
#[async_trait]
pub trait RemoteHandler: Send + Sync + 'static {
    /// Handle `session.hello` — negotiate protocol version and return the
    /// initial workspaces + tabs snapshot. Implementations that require
    /// auth should check `params.auth` and return an `AUTH_REQUIRED` or
    /// `AUTH_INVALID` error if missing/wrong.
    async fn session_hello(
        &self,
        params: SessionHelloParams,
    ) -> Result<SessionHelloResult, RpcError>;

    /// Handle `pairing.request` — issue credentials to a new device.
    /// Implementations decide approval policy (TOTP, local dialog,
    /// env-gated auto-approve, etc.).
    async fn pairing_request(
        &self,
        params: PairingRequestParams,
    ) -> Result<PairingRequestResult, RpcError>;

    /// Handle `workspaces.list`.
    async fn workspaces_list(&self) -> Result<WorkspacesListResult, RpcError>;

    /// Handle `workspace.activate` — spin up a stopped workspace's
    /// sandbox, restore any checkpointed tabs, and return the
    /// refreshed workspace + its tabs. Default impl returns a
    /// `method_not_found`-equivalent so lightweight handlers (tests,
    /// stubs) opt in explicitly.
    async fn workspace_activate(
        &self,
        params: WorkspaceActivateParams,
    ) -> Result<WorkspaceActivateResult, RpcError> {
        let _ = params;
        Err(RpcError::new(
            error_code::INTERNAL_ERROR,
            "workspace.activate not implemented by this handler",
        ))
    }

    /// Handle `tabs.list`. Returns tabs across all workspaces.
    async fn tabs_list(&self) -> Result<TabsListResult, RpcError>;

    /// Handle `tabs.create` — open a new tab in the given workspace.
    /// `agent_id` is optional; the host picks a default when absent.
    /// Default impl returns unimplemented so stubs don't silently
    /// accept creations they can't service.
    async fn tabs_create(
        &self,
        params: TabsCreateParams,
    ) -> Result<TabsCreateResult, RpcError> {
        let _ = params;
        Err(RpcError::new(
            error_code::INTERNAL_ERROR,
            "tabs.create not implemented by this handler",
        ))
    }

    /// Handle `tabs.close` — close a tab. `mode: Checkpoint` snapshots
    /// an agent sandbox and leaves a stopped row behind; `mode: Force`
    /// tears the tab down. Hosts without sandbox-capable tabs can
    /// simply treat both modes as a forced close.
    async fn tabs_close(&self, params: TabsCloseParams) -> Result<(), RpcError> {
        let _ = params;
        Err(RpcError::new(
            error_code::INTERNAL_ERROR,
            "tabs.close not implemented by this handler",
        ))
    }

    /// Handle `pty.attach` — host prepares to accept a PTY stream for the
    /// given (workspace_id, tab_id) tab.
    async fn pty_attach(&self, params: PtyAttachParams) -> Result<PtyAttachResult, RpcError>;

    /// Handle `pty.detach` — tear down a PTY stream (or mark its tab as
    /// detached from this session).
    async fn pty_detach(&self, params: PtyDetachParams) -> Result<(), RpcError>;

    /// Handle `pty.resize`.
    async fn pty_resize(&self, params: PtyResizeParams) -> Result<(), RpcError>;

    /// Handle a newly opened PTY data stream. The handler owns the stream
    /// from this point and is responsible for pumping bytes in both
    /// directions (recv → PTY stdin, PTY stdout → send). Returns when the
    /// stream closes.
    async fn pty_stream(
        &self,
        workspace_id: WorkspaceId,
        tab_id: TabId,
        cols: u16,
        rows: u16,
        send: SendStream,
        recv: RecvStream,
    ) -> Result<(), RpcError>;

    /// Called once for every control-stream JSON-RPC request after it
    /// resolves (success or error). Default impl is a no-op; hosts that
    /// want to log remote activity override this to write an audit
    /// entry. `device_id` is `Some` after `session.hello` establishes
    /// the session identity, `None` for pre-auth methods like
    /// `pairing.request` and `session.hello` itself.
    async fn audit_rpc(&self, method: &str, ok: bool, device_id: Option<String>) {
        let _ = (method, ok, device_id);
    }

    /// Called once per connection after `session.hello` succeeds.
    /// Returns a broadcast receiver the session's control-stream
    /// writer will drain, forwarding each message verbatim as a
    /// JSON-RPC notification line. Default is `None` — hosts that
    /// never push notifications don't need to override.
    ///
    /// The bytes are expected to be a full JSON-RPC notification
    /// envelope (i.e. the output of `encode_notification`).
    fn subscribe_notifications(
        &self,
    ) -> Option<tokio::sync::broadcast::Receiver<std::sync::Arc<String>>> {
        None
    }
}

/// A stub handler that returns empty / nonsense for everything. Used in
/// integration tests to validate the transport without a real app behind it.
/// `pty_stream` implements a byte echo so round-trip tests can verify the
/// full pipeline.
pub struct StubHandler {
    pub host_info: HostInfo,
}

impl Default for StubHandler {
    fn default() -> Self {
        Self {
            host_info: HostInfo {
                app_version: env!("CARGO_PKG_VERSION").to_string(),
                os: std::env::consts::OS.to_string(),
                hostname: hostname().unwrap_or_else(|| "unknown".into()),
            },
        }
    }
}

#[async_trait]
impl RemoteHandler for StubHandler {
    async fn session_hello(
        &self,
        params: SessionHelloParams,
    ) -> Result<SessionHelloResult, RpcError> {
        use superhq_remote_proto::PROTOCOL_VERSION;
        let accepted = params.protocol_version.min(PROTOCOL_VERSION);
        Ok(SessionHelloResult {
            protocol_version: accepted,
            session_id: uuid::Uuid::new_v4().to_string(),
            resume_token: uuid::Uuid::new_v4().to_string(),
            host_info: self.host_info.clone(),
            workspaces: Vec::new(),
            tabs: Vec::new(),
            agents: Vec::new(),
        })
    }

    async fn pairing_request(
        &self,
        _params: PairingRequestParams,
    ) -> Result<PairingRequestResult, RpcError> {
        Err(RpcError::new(
            superhq_remote_proto::error_code::PAIRING_REJECTED,
            "stub handler: pairing disabled",
        ))
    }

    async fn workspaces_list(&self) -> Result<WorkspacesListResult, RpcError> {
        Ok(Vec::new())
    }

    async fn tabs_list(&self) -> Result<TabsListResult, RpcError> {
        Ok(Vec::new())
    }

    async fn pty_attach(&self, _params: PtyAttachParams) -> Result<PtyAttachResult, RpcError> {
        // Stub: pretend any tab is attachable, 80x24.
        Ok(PtyAttachResult {
            cols: 80,
            rows: 24,
            initial_buffer: None,
        })
    }

    async fn pty_detach(&self, _params: PtyDetachParams) -> Result<(), RpcError> {
        Ok(())
    }

    async fn pty_resize(&self, _params: PtyResizeParams) -> Result<(), RpcError> {
        Ok(())
    }

    async fn pty_stream(
        &self,
        _workspace_id: WorkspaceId,
        _tab_id: TabId,
        _cols: u16,
        _rows: u16,
        mut send: SendStream,
        mut recv: RecvStream,
    ) -> Result<(), RpcError> {
        // Echo: every chunk received on `recv` is written back to `send`.
        let mut buf = [0u8; 4096];
        loop {
            let n = match recv.read(&mut buf).await {
                Ok(Some(n)) => n,
                Ok(None) => break,
                Err(_) => break,
            };
            if n == 0 {
                break;
            }
            if send.write_all(&buf[..n]).await.is_err() {
                break;
            }
        }
        let _ = send.finish();
        Ok(())
    }
}

fn hostname() -> Option<String> {
    // tokio-free, blocking; fine for one-time host startup.
    std::process::Command::new("hostname")
        .output()
        .ok()
        .and_then(|o| {
            let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if s.is_empty() { None } else { Some(s) }
        })
}
