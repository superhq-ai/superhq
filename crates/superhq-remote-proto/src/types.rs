//! Shared domain types used across methods and notifications.

use serde::{Deserialize, Serialize};

/// Opaque workspace identifier (matches the host-side `workspace_id` i64).
pub type WorkspaceId = i64;

/// Opaque tab identifier (matches host-side `tab_id` u64).
///
/// Note: `TabId` is only unique within a workspace. A fully-qualified tab
/// reference is `(WorkspaceId, TabId)`. The remote protocol always carries
/// both.
pub type TabId = u64;

/// A workspace as visible to a remote client.
///
/// Includes workspaces that aren't currently loaded in the host app —
/// `is_active: false` means it exists but its sandbox/tabs aren't running
/// right now. A future `workspaces.activate` method would spin one up.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WorkspaceInfo {
    pub workspace_id: WorkspaceId,
    pub label: String,
    /// True when this workspace's sandbox is live in the host right now.
    pub is_active: bool,
    /// Repo name (mount-path basename) when the workspace's mount is a
    /// git repo. `None` for scratch sandboxes or non-git paths.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo_name: Option<String>,
    /// Current branch (from `.git/HEAD`) when mount is a git repo.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// GitHub owner/org of the remote (if it's a github remote).
    /// Clients can render `https://github.com/{owner}.png` as the
    /// workspace avatar without needing an extra round trip.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub github_owner: Option<String>,
}

/// A handle to a binary blob stored in the session's iroh-blobs store.
/// The actual bytes are fetched out-of-band via iroh-blobs.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BlobHandle {
    /// BLAKE3 hash as used by iroh-blobs.
    pub hash: String,
    /// Size in bytes, for progress UIs.
    pub size: u64,
    /// Application-level content hint. Not used by iroh-blobs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime: Option<String>,
}

/// Tab classification.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TabKind {
    Agent,
    Shell,
    HostShell,
}

/// Agent lifecycle state as reported from event hooks.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum AgentState {
    Unknown,
    Running {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool: Option<String>,
    },
    NeedsInput {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },
    Idle,
}

/// A tab as visible to a remote client.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TabInfo {
    pub workspace_id: WorkspaceId,
    pub tab_id: TabId,
    pub label: String,
    pub kind: TabKind,
    pub agent_state: AgentState,
    /// True once the host has a live PTY registered for this tab —
    /// meaning a remote client can call `pty.attach` and expect
    /// bytes. False while an agent tab is still booting its sandbox.
    #[serde(default)]
    pub pty_ready: bool,
    /// Populated when the host failed to bring this tab up (e.g. an
    /// agent whose required secrets aren't configured, or a sandbox
    /// boot error). When `Some`, `pty_ready` will stay false — remote
    /// clients should render the error instead of a waiting spinner.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub setup_error: Option<String>,
}

/// An agent configured on the host. Surfaced to remote clients so the
/// new-tab menu can present the same set of options the desktop shows
/// (Claude Code, Codex, Pi, …) with matching icons + colours.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentInfo {
    pub id: i64,
    pub display_name: String,
    /// Short stable slug (`claude`, `codex`, `pi`). Useful for the
    /// client to look up bundled assets without needing the raw SVG.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slug: Option<String>,
    /// Inline SVG markup for the agent icon, when the host has one.
    /// Small (< 5 KB each) so embedding in `session.hello` is cheap.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon_svg: Option<String>,
    /// Accent colour as hex (`#ff00aa`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
}

/// File change status in a diff view.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FileStatus {
    Added,
    Modified,
    Deleted,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn agent_state_roundtrip() {
        let idle = AgentState::Idle;
        let wire = serde_json::to_value(&idle).unwrap();
        assert_eq!(wire, json!({"state": "idle"}));
        let back: AgentState = serde_json::from_value(wire).unwrap();
        assert_eq!(back, idle);

        let running = AgentState::Running { tool: Some("search".into()) };
        let wire = serde_json::to_value(&running).unwrap();
        assert_eq!(wire, json!({"state": "running", "tool": "search"}));
        let back: AgentState = serde_json::from_value(wire).unwrap();
        assert_eq!(back, running);
    }

    #[test]
    fn tab_kind_roundtrip() {
        let wire = serde_json::to_value(TabKind::Agent).unwrap();
        assert_eq!(wire, json!("agent"));
        let back: TabKind = serde_json::from_value(wire).unwrap();
        assert_eq!(back, TabKind::Agent);
    }

    #[test]
    fn file_status_roundtrip() {
        let wire = serde_json::to_value(FileStatus::Modified).unwrap();
        assert_eq!(wire, json!("modified"));
    }
}
