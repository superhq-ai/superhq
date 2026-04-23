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
    /// Handle `session.hello`.
    ///
    /// `challenge` is the 32-byte nonce the server issued via the
    /// preceding `session.challenge` call on this connection, or
    /// `None` if the client skipped that step. A server that
    /// requires auth should reject `None` with `AUTH_INVALID`.
    /// The nonce has already been consumed from session state;
    /// handlers receive it at-most-once here.
    async fn session_hello(
        &self,
        params: SessionHelloParams,
        challenge: Option<[u8; 32]>,
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
    /// given (workspace_id, tab_id) tab. `device_id` identifies the
    /// calling session so the handler can aggregate per-client sizes.
    async fn pty_attach(
        &self,
        params: PtyAttachParams,
        device_id: Option<String>,
    ) -> Result<PtyAttachResult, RpcError>;

    /// Handle `pty.detach` — tear down a PTY stream (or mark its tab as
    /// detached from this session).
    async fn pty_detach(
        &self,
        params: PtyDetachParams,
        device_id: Option<String>,
    ) -> Result<(), RpcError>;

    /// Handle `pty.resize`.
    async fn pty_resize(
        &self,
        params: PtyResizeParams,
        device_id: Option<String>,
    ) -> Result<(), RpcError>;

    /// Handle a newly opened attachment upload stream. Client writes
    /// `size` bytes after the `stream.init` ack; handler saves them,
    /// writes a JSON `AttachmentResult { path }` line, and closes.
    /// Default impl returns unimplemented.
    async fn attachment_stream(
        &self,
        workspace_id: WorkspaceId,
        tab_id: TabId,
        name: String,
        mime: Option<String>,
        size: u64,
        device_id: Option<String>,
        send: SendStream,
        recv: RecvStream,
    ) -> Result<(), RpcError> {
        let _ = (
            workspace_id,
            tab_id,
            name,
            mime,
            size,
            device_id,
            send,
            recv,
        );
        Err(RpcError::new(
            error_code::INTERNAL_ERROR,
            "attachment.stream not implemented by this handler",
        ))
    }

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
        device_id: Option<String>,
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

    /// Whether `device_id` is still a paired, authorized device. Called
    /// before every post-auth RPC and every data-stream init so a session
    /// that authenticated earlier can be neutralized the moment its
    /// device is revoked. Default impl returns `true`, matching the
    /// existing "auth at hello, trust forever" behavior.
    async fn is_device_authorized(&self, device_id: &str) -> bool {
        let _ = device_id;
        true
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
        _challenge: Option<[u8; 32]>,
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
            allow_host_shell: false,
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

    async fn pty_attach(
        &self,
        _params: PtyAttachParams,
        _device_id: Option<String>,
    ) -> Result<PtyAttachResult, RpcError> {
        // Stub: pretend any tab is attachable, 80x24.
        Ok(PtyAttachResult {
            cols: 80,
            rows: 24,
            initial_buffer: None,
        })
    }

    async fn pty_detach(
        &self,
        _params: PtyDetachParams,
        _device_id: Option<String>,
    ) -> Result<(), RpcError> {
        Ok(())
    }

    async fn pty_resize(
        &self,
        _params: PtyResizeParams,
        _device_id: Option<String>,
    ) -> Result<(), RpcError> {
        Ok(())
    }

    async fn pty_stream(
        &self,
        _workspace_id: WorkspaceId,
        _tab_id: TabId,
        _cols: u16,
        _rows: u16,
        _device_id: Option<String>,
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
