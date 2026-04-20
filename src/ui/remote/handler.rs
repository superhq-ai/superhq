//! Real `RemoteHandler` implementation.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use base64::{engine::general_purpose::STANDARD, Engine};
use superhq_remote_host::{
    generate_device_key, now_secs, verify_proof, AuthError, EndpointId, RecvStream,
    RemoteHandler, SendStream,
};
use superhq_remote_proto::{
    error_code,
    methods::{
        HostInfo, PairingRequestParams, PairingRequestResult, PtyAttachParams,
        PtyAttachResult, PtyDetachParams, PtyResizeParams, SessionAuth,
        SessionHelloParams, SessionHelloResult, TabsCloseParams, TabsCreateParams,
        TabsCreateResult, TabsListResult, WorkspaceActivateParams,
        WorkspaceActivateResult, WorkspacesListResult,
    },
    types::{AgentInfo, TabId, WorkspaceId},
    RpcError, PROTOCOL_VERSION,
};
use tokio::sync::broadcast::error::RecvError;

use super::approval::PairingApprover;
use super::audit::AuditLog;
use super::commands::HostCommandDispatcher;
use super::pairing::{PairedDevice, PairingStore};
use super::pty_bus::{PtyBus, PtyMap};
use super::snapshot::RemoteStateSnapshot;

pub struct AppHandler {
    host_info: HostInfo,
    state: Arc<RwLock<RemoteStateSnapshot>>,
    pty_map: PtyMap,
    pairings: Arc<PairingStore>,
    audit: AuditLog,
    approver: PairingApprover,
    commands: HostCommandDispatcher,
    /// Broadcast of already-encoded JSON-RPC notifications. Each
    /// connected session subscribes and forwards items on its control
    /// stream. See `RemoteAccess::broadcast_snapshot_invalidated`.
    notifications: tokio::sync::broadcast::Sender<Arc<String>>,
    /// Agents list lazily populated from the host DB. We fill it at
    /// construction time via a callback rather than passing a
    /// `Database` ref so the remote module stays decoupled from the
    /// `db` module.
    agents: Arc<RwLock<Vec<AgentInfo>>>,
    /// The host's own NodeId, used in the HMAC transcript. Set after the
    /// RemoteServer is up — see `RemoteAccess::start`.
    host_node_id: Arc<RwLock<Option<String>>>,
    /// If true, `session.hello` requires valid auth. If false (V1 default),
    /// unauthed hellos are accepted (for migration / demo convenience).
    require_auth: bool,
    /// If true, pairing requests are auto-approved without UI. Dev/demo only.
    auto_approve_pairings: bool,
    /// Per-device most-recent-accepted timestamp. Blocks replay: the
    /// same HMAC proof cannot authenticate twice because the second
    /// attempt's timestamp is not strictly greater than the first's.
    /// In-memory only — on process restart the guard resets, which
    /// still leaves a one-replay-per-restart window but closes the
    /// previous 5-minute skew-window replay gap.
    replay_guard: Arc<RwLock<HashMap<String, u64>>>,
}

impl AppHandler {
    pub fn new(
        state: Arc<RwLock<RemoteStateSnapshot>>,
        pty_map: PtyMap,
        pairings: Arc<PairingStore>,
        audit: AuditLog,
        approver: PairingApprover,
        commands: HostCommandDispatcher,
        notifications: tokio::sync::broadcast::Sender<Arc<String>>,
    ) -> Self {
        // Auth is required by default. The only opt-out is an explicit
        // env override — previously the default was `false`, which let
        // any peer that reached the iroh endpoint skip pairing entirely.
        let require_auth = std::env::var("SUPERHQ_REMOTE_REQUIRE_AUTH")
            .map(|v| !(v == "0" || v.eq_ignore_ascii_case("false")))
            .unwrap_or(true);
        let auto_approve_pairings = std::env::var("SUPERHQ_REMOTE_AUTO_APPROVE_PAIRINGS")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);
        Self {
            host_info: HostInfo {
                app_version: env!("CARGO_PKG_VERSION").to_string(),
                os: std::env::consts::OS.to_string(),
                hostname: hostname().unwrap_or_else(|| "unknown".into()),
            },
            state,
            pty_map,
            pairings,
            audit,
            approver,
            commands,
            notifications,
            agents: Arc::new(RwLock::new(Vec::new())),
            host_node_id: Arc::new(RwLock::new(None)),
            require_auth,
            auto_approve_pairings,
            replay_guard: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub fn set_host_node_id(&self, id: EndpointId) {
        if let Ok(mut guard) = self.host_node_id.write() {
            *guard = Some(id.to_string());
        }
    }

    /// Replace the cached agents list. Called from GPUI render so the
    /// list tracks DB changes (new agent added, rename, etc).
    pub fn set_agents(&self, agents: Vec<AgentInfo>) {
        if let Ok(mut guard) = self.agents.write() {
            *guard = agents;
        }
    }

    fn read_agents(&self) -> Vec<AgentInfo> {
        self.agents
            .read()
            .map(|g| g.clone())
            .unwrap_or_default()
    }

    /// Absolute path of the audit log — exposed so the Settings UI can
    /// display it / open it in an external editor.
    pub fn audit_log_path(&self) -> std::path::PathBuf {
        self.audit.path()
    }

    /// Mint a fresh credential and persist the paired device. Shared
    /// by the approval-dialog and env-var auto-approve paths.
    fn issue_credentials(&self, device_label: String) -> PairingRequestResult {
        let device_id = format!("dev_{}", uuid::Uuid::new_v4().simple());
        let device_key = generate_device_key();
        let device_key_b64 = STANDARD.encode(device_key);

        let record = PairedDevice {
            device_id: device_id.clone(),
            device_label: device_label.clone(),
            device_key_b64: device_key_b64.clone(),
            created_at: now_secs(),
        };
        self.pairings.insert(record);

        tracing::info!(
            device_id = %device_id,
            label = %device_label,
            "remote-control: paired new device"
        );

        PairingRequestResult {
            device_id,
            device_key: device_key_b64,
        }
    }

    fn host_id(&self) -> Option<String> {
        self.host_node_id.read().ok().and_then(|g| g.clone())
    }

    fn read_state(&self) -> RemoteStateSnapshot {
        self.state
            .read()
            .map(|g| g.clone())
            .unwrap_or_default()
    }

    fn find_bus(&self, workspace_id: WorkspaceId, tab_id: TabId) -> Option<PtyBus> {
        self.pty_map
            .read()
            .ok()
            .and_then(|m| m.get(&(workspace_id, tab_id)).cloned())
    }

    /// Check auth: returns Ok(()) if acceptable, Err(RpcError) otherwise.
    fn check_auth(&self, auth: Option<&SessionAuth>) -> Result<(), RpcError> {
        let Some(auth) = auth else {
            if self.require_auth {
                return Err(RpcError::new(
                    error_code::AUTH_REQUIRED,
                    "session.hello requires auth (pair first via pairing.request)",
                ));
            }
            return Ok(());
        };
        let Some(host_id) = self.host_id() else {
            return Err(RpcError::new(
                error_code::AUTH_INVALID,
                "host node id not yet available",
            ));
        };
        let device = self.pairings.get(&auth.device_id).ok_or_else(|| {
            RpcError::new(
                error_code::AUTH_INVALID,
                format!("unknown device_id: {}", auth.device_id),
            )
        })?;
        let key_bytes = STANDARD
            .decode(device.device_key_b64.as_bytes())
            .map_err(|_| {
                RpcError::new(error_code::AUTH_INVALID, "stored device key malformed")
            })?;
        verify_proof(
            &key_bytes,
            &host_id,
            &auth.device_id,
            auth.timestamp,
            &auth.proof,
            now_secs(),
        )
        .map_err(auth_err_to_rpc)?;
        // Replay protection: require strictly increasing timestamps per
        // device. Captured proofs within the 5-minute skew window used
        // to be reusable; this closes that gap. Check-and-set atomically
        // under the write lock so racing hellos can't both be accepted.
        {
            let mut g = self.replay_guard.write().map_err(|_| {
                RpcError::new(
                    error_code::INTERNAL_ERROR,
                    "replay guard lock poisoned",
                )
            })?;
            let last = g.get(&auth.device_id).copied().unwrap_or(0);
            if auth.timestamp <= last {
                return Err(RpcError::new(
                    error_code::AUTH_INVALID,
                    "replay rejected: timestamp not greater than last accepted",
                ));
            }
            g.insert(auth.device_id.clone(), auth.timestamp);
        }
        // Auth passed — bump last_seen_at for the paired-devices UI later.
        self.pairings.touch(&auth.device_id, now_secs());
        Ok(())
    }
}

fn auth_err_to_rpc(e: AuthError) -> RpcError {
    RpcError::new(error_code::AUTH_INVALID, e.to_string())
}

#[async_trait]
impl RemoteHandler for AppHandler {
    async fn session_hello(
        &self,
        params: SessionHelloParams,
    ) -> Result<SessionHelloResult, RpcError> {
        self.check_auth(params.auth.as_ref())?;
        let accepted = params.protocol_version.min(PROTOCOL_VERSION);
        let snap = self.read_state();
        Ok(SessionHelloResult {
            protocol_version: accepted,
            session_id: uuid::Uuid::new_v4().to_string(),
            resume_token: uuid::Uuid::new_v4().to_string(),
            host_info: self.host_info.clone(),
            workspaces: snap.workspaces,
            tabs: snap.tabs,
            agents: self.read_agents(),
        })
    }

    async fn pairing_request(
        &self,
        params: PairingRequestParams,
    ) -> Result<PairingRequestResult, RpcError> {
        if params.totp_code.is_some() {
            return Err(RpcError::new(
                error_code::PAIRING_REJECTED,
                "TOTP pairing not yet implemented (V1)",
            ));
        }

        // Dev escape hatch — skip the approval dialog when the env var
        // is set. Everyone else waits on the UI.
        if !self.auto_approve_pairings {
            let approved = self
                .approver
                .request_approval(params.device_label.clone())
                .await;
            if !approved {
                return Err(RpcError::new(
                    error_code::PAIRING_REJECTED,
                    "pairing rejected by host",
                ));
            }
        }

        Ok(self.issue_credentials(params.device_label))
    }

    async fn workspaces_list(&self) -> Result<WorkspacesListResult, RpcError> {
        Ok(self.read_state().workspaces)
    }

    async fn workspace_activate(
        &self,
        params: WorkspaceActivateParams,
    ) -> Result<WorkspaceActivateResult, RpcError> {
        self.commands.activate_workspace(params.workspace_id).await
    }

    async fn tabs_list(&self) -> Result<TabsListResult, RpcError> {
        Ok(self.read_state().tabs)
    }

    async fn tabs_create(
        &self,
        params: TabsCreateParams,
    ) -> Result<TabsCreateResult, RpcError> {
        self.commands
            .create_tab(params.workspace_id, params.spec)
            .await
    }

    async fn tabs_close(&self, params: TabsCloseParams) -> Result<(), RpcError> {
        self.commands
            .close_tab(params.workspace_id, params.tab_id, params.mode)
            .await
    }

    async fn pty_attach(
        &self,
        params: PtyAttachParams,
    ) -> Result<PtyAttachResult, RpcError> {
        let Some(bus) = self.find_bus(params.workspace_id, params.tab_id) else {
            return Err(RpcError::new(
                error_code::NOT_FOUND,
                format!(
                    "no terminal for workspace={} tab={}",
                    params.workspace_id, params.tab_id
                ),
            ));
        };
        let (cols, rows) = bus.current_dimensions();
        Ok(PtyAttachResult {
            cols,
            rows,
            initial_buffer: None,
        })
    }

    async fn pty_detach(&self, _params: PtyDetachParams) -> Result<(), RpcError> {
        Ok(())
    }

    async fn pty_resize(&self, params: PtyResizeParams) -> Result<(), RpcError> {
        let Some(bus) = self.find_bus(params.workspace_id, params.tab_id) else {
            return Err(RpcError::new(
                error_code::NOT_FOUND,
                "pty.resize: tab not found",
            ));
        };
        bus.resize(params.cols, params.rows);
        Ok(())
    }

    async fn pty_stream(
        &self,
        workspace_id: WorkspaceId,
        tab_id: TabId,
        _cols: u16,
        _rows: u16,
        mut send: SendStream,
        mut recv: RecvStream,
    ) -> Result<(), RpcError> {
        let Some(bus) = self.find_bus(workspace_id, tab_id) else {
            return Err(RpcError::new(
                error_code::NOT_FOUND,
                "pty.stream: tab not found",
            ));
        };

        let (scrollback, mut subscriber) = bus.snapshot_and_subscribe();
        let writer = bus.writer.clone();

        let mut send_closed = false;
        let output_task = async {
            if !scrollback.is_empty() {
                if send.write_all(&scrollback).await.is_err() {
                    send_closed = true;
                    let _ = send.finish();
                    return;
                }
            }
            loop {
                match subscriber.recv().await {
                    Ok(chunk) => {
                        if send.write_all(&chunk).await.is_err() {
                            break;
                        }
                    }
                    Err(RecvError::Lagged(_)) => continue,
                    Err(RecvError::Closed) => break,
                }
            }
            send_closed = true;
            let _ = send.finish();
        };

        let input_task = async {
            let mut buf = [0u8; 4096];
            loop {
                match recv.read(&mut buf).await {
                    Ok(Some(0)) | Ok(None) => break,
                    Ok(Some(n)) => {
                        if writer.send_input(&buf[..n]).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        };

        tokio::join!(output_task, input_task);
        let _ = send_closed;
        Ok(())
    }

    async fn audit_rpc(&self, method: &str, ok: bool, device_id: Option<String>) {
        self.audit.log(method, ok, device_id.as_deref());
    }

    fn subscribe_notifications(
        &self,
    ) -> Option<tokio::sync::broadcast::Receiver<Arc<String>>> {
        Some(self.notifications.subscribe())
    }
}

fn hostname() -> Option<String> {
    std::process::Command::new("hostname")
        .output()
        .ok()
        .and_then(|o| {
            let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if s.is_empty() { None } else { Some(s) }
        })
}

