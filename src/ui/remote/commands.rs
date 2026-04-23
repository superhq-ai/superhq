//! Cross-runtime command channel for remote RPCs that need to drive
//! the GPUI main thread (workspace.activate, tabs.create).
//!
//! Mirrors the shape of `approval.rs` — flume queue of request enums,
//! each carrying a `tokio::sync::oneshot` Sender for the response.
//! `AppHandler` holds the sender and awaits a response with a bounded
//! timeout; `AppView` owns the receiver and dispatches into
//! `TerminalPanel` on each incoming command.

use std::time::Duration;

use superhq_remote_proto::{
    methods::{TabCloseMode, TabCreateSpec, TabsCreateResult, WorkspaceActivateResult},
    types::{TabId, WorkspaceId},
    RpcError,
};
use tokio::sync::oneshot;

/// Workspace activation / tab creation can take a moment — spinning
/// up a sandbox, downloading an image, auto-launching an agent. 60 s
/// keeps the client from hanging forever if the GPUI side is busy,
/// while being generous enough for a slow sandbox start.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(60);

pub enum HostCommand {
    ActivateWorkspace {
        workspace_id: WorkspaceId,
        response: oneshot::Sender<Result<WorkspaceActivateResult, RpcError>>,
    },
    CreateTab {
        workspace_id: WorkspaceId,
        spec: TabCreateSpec,
        response: oneshot::Sender<Result<TabsCreateResult, RpcError>>,
    },
    CloseTab {
        workspace_id: WorkspaceId,
        tab_id: TabId,
        mode: TabCloseMode,
        response: oneshot::Sender<Result<(), RpcError>>,
    },
    WriteAttachment {
        workspace_id: WorkspaceId,
        tab_id: TabId,
        name: String,
        bytes: Vec<u8>,
        response: oneshot::Sender<Result<String, RpcError>>,
    },
}

#[derive(Clone)]
pub struct HostCommandDispatcher {
    tx: flume::Sender<HostCommand>,
}

impl HostCommandDispatcher {
    pub fn new() -> (Self, flume::Receiver<HostCommand>) {
        let (tx, rx) = flume::unbounded();
        (Self { tx }, rx)
    }

    pub async fn activate_workspace(
        &self,
        workspace_id: WorkspaceId,
    ) -> Result<WorkspaceActivateResult, RpcError> {
        let (resp_tx, resp_rx) = oneshot::channel();
        let cmd = HostCommand::ActivateWorkspace {
            workspace_id,
            response: resp_tx,
        };
        send_and_await(&self.tx, cmd, resp_rx, "workspace.activate").await
    }

    pub async fn create_tab(
        &self,
        workspace_id: WorkspaceId,
        spec: TabCreateSpec,
    ) -> Result<TabsCreateResult, RpcError> {
        let (resp_tx, resp_rx) = oneshot::channel();
        let cmd = HostCommand::CreateTab {
            workspace_id,
            spec,
            response: resp_tx,
        };
        send_and_await(&self.tx, cmd, resp_rx, "tabs.create").await
    }

    pub async fn close_tab(
        &self,
        workspace_id: WorkspaceId,
        tab_id: TabId,
        mode: TabCloseMode,
    ) -> Result<(), RpcError> {
        let (resp_tx, resp_rx) = oneshot::channel();
        let cmd = HostCommand::CloseTab {
            workspace_id,
            tab_id,
            mode,
            response: resp_tx,
        };
        send_and_await(&self.tx, cmd, resp_rx, "tabs.close").await
    }

    pub async fn write_attachment(
        &self,
        workspace_id: WorkspaceId,
        tab_id: TabId,
        name: String,
        bytes: Vec<u8>,
    ) -> Result<String, RpcError> {
        let (resp_tx, resp_rx) = oneshot::channel();
        let cmd = HostCommand::WriteAttachment {
            workspace_id,
            tab_id,
            name,
            bytes,
            response: resp_tx,
        };
        send_and_await(&self.tx, cmd, resp_rx, "attachment.write").await
    }
}

async fn send_and_await<T>(
    tx: &flume::Sender<HostCommand>,
    cmd: HostCommand,
    rx: oneshot::Receiver<Result<T, RpcError>>,
    method: &str,
) -> Result<T, RpcError> {
    tx.send_async(cmd).await.map_err(|_| {
        RpcError::internal(format!("{method}: host UI is not available"))
    })?;
    match tokio::time::timeout(COMMAND_TIMEOUT, rx).await {
        Ok(Ok(result)) => result,
        Ok(Err(_)) => Err(RpcError::internal(format!(
            "{method}: host cancelled the command"
        ))),
        Err(_) => Err(RpcError::internal(format!(
            "{method}: timed out waiting for the host"
        ))),
    }
}
