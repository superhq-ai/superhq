use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Workspace status values.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceStatus {
    Booting,
    Running,
    Stopped,
    Archived,
}

impl WorkspaceStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Booting => "booting",
            Self::Running => "running",
            Self::Stopped => "stopped",
            Self::Archived => "archived",
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s {
            "booting" => Self::Booting,
            "running" => Self::Running,
            "stopped" => Self::Stopped,
            "archived" => Self::Archived,
            _ => Self::Stopped,
        }
    }
}

/// Cached git status for a workspace with a mounted git repo.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GitStatus {
    pub ahead: u32,
    pub behind: u32,
    pub staged: u32,
    pub unstaged: u32,
    pub untracked: u32,
}

/// Cached diff stats for a workspace.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DiffStats {
    pub additions: u32,
    pub deletions: u32,
    pub files: Vec<DiffFileStat>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiffFileStat {
    pub path: String,
    pub additions: u32,
    pub deletions: u32,
}

/// A port forwarding rule: guest exposes a port to the host.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortMapping {
    pub guest_port: u16,
    pub host_port: u16,
}

/// A host port exposed to the sandbox via host.shuru.internal.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExposeHostPort {
    /// Port on the host machine (localhost:{host_port}).
    pub host_port: u16,
    /// Port visible inside the sandbox (host.shuru.internal:{guest_port}).
    pub guest_port: u16,
}

/// A workspace row from the database.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct Workspace {
    pub id: i64,
    pub name: String,
    pub status: WorkspaceStatus,
    pub cloned_from_id: Option<i64>,
    pub initial_prompt: Option<String>,
    pub tab_order: i32,
    pub last_opened_at: Option<String>,
    pub is_unread: bool,

    // Mount
    pub mount_path: Option<String>,
    pub mount_read_only: bool,
    pub is_git_repo: bool,
    pub branch_name: Option<String>,
    pub base_branch: Option<String>,

    // Git (cached JSON)
    pub git_status: Option<GitStatus>,
    pub diff_stats: Option<DiffStats>,
    pub pr_number: Option<i32>,
    pub pr_url: Option<String>,

    // Sandbox config
    pub sandbox_cpus: i32,
    pub sandbox_memory_mb: i64,
    pub sandbox_disk_mb: i64,
    pub allowed_hosts: Option<Vec<String>>,
    pub secrets_config: Option<String>,

    // Port forwarding (guest -> host)
    pub port_mappings: Vec<PortMapping>,

    // Host ports exposed to sandbox (host -> guest)
    pub expose_host_ports: Vec<ExposeHostPort>,

    // Sandbox runtime
    pub sandbox_instance_dir: Option<String>,
    pub sandbox_checkpoint_name: Option<String>,

    pub created_at: String,
    pub stopped_at: Option<String>,
    pub deleting_at: Option<String>,
}

/// A secret required by an agent, with optional OAuth claim extraction.
///
/// Deserialized from JSON — supports both plain strings (`"OPENAI_API_KEY"`)
/// and full objects with hosts/oauth_claims.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RequiredSecretEntry {
    /// Plain env var name: `"OPENAI_API_KEY"` — shorthand.
    Plain(String),
    /// Full specification with optional hosts and OAuth claims.
    Full(RequiredSecret),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequiredSecret {
    pub env_var: String,
    pub label: Option<String>,
    pub hosts: Option<Vec<String>>,
    /// When auth_method is "oauth", extract these JWT claims from the id_token
    /// and inject them as env vars into the sandbox.
    /// Key = env var name in sandbox, Value = JWT claim name.
    /// e.g. `{"CHATGPT_ACCOUNT_ID": "chatgpt_account_id"}`
    pub oauth_claims: Option<HashMap<String, String>>,
}

impl RequiredSecretEntry {
    /// Get the env var name regardless of variant.
    pub fn env_var(&self) -> &str {
        match self {
            Self::Plain(s) => s,
            Self::Full(r) => &r.env_var,
        }
    }

    /// Get the full RequiredSecret, promoting plain strings to defaults.
    pub fn as_full(&self) -> RequiredSecret {
        match self {
            Self::Plain(s) => RequiredSecret {
                env_var: s.clone(),
                label: None,
                hosts: None,
                oauth_claims: None,
            },
            Self::Full(r) => r.clone(),
        }
    }
}

/// An agent CLI definition from the database.
/// Static config (required_secrets) comes from AgentConfig at runtime.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct Agent {
    pub id: i64,
    pub name: String,
    pub display_name: String,
    pub command: String,
    pub icon: Option<String>,
    pub color: Option<String>,
    pub is_builtin: bool,
    pub tab_order: i32,
    pub required_secrets: Vec<RequiredSecretEntry>,
}

impl Agent {
    /// Merge static config from the embedded AgentConfig.
    pub fn with_config(mut self, configs: &[crate::agents::AgentConfig]) -> Self {
        if let Some(cfg) = configs.iter().find(|c| c.name == self.name) {
            self.required_secrets = cfg.secrets.clone();
        }
        self
    }
}

/// A stored secret (metadata only — decrypted value is fetched on demand).
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct Secret {
    pub id: i64,
    pub env_var: String,
    pub label: String,
    pub hosts: Vec<String>,
    pub auth_method: String,
    pub oauth_expires_at: Option<String>,
}

/// A terminal tab within a workspace (DB row).
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct TerminalTab {
    pub id: i64,
    pub workspace_id: i64,
    pub label: String,
    pub agent_id: Option<i64>,
    pub tab_order: i32,
    pub created_at: String,
}

/// A saved checkpointed tab loaded from the DB.
#[derive(Debug, Clone)]
pub struct SavedTab {
    pub id: i64,
    pub label: String,
    pub agent_id: Option<i64>,
    pub checkpoint_name: Option<String>,
}

/// App settings (singleton row).
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct Settings {
    pub last_active_workspace_id: Option<i64>,
    pub theme: String,
    pub terminal_font_family: String,
    pub terminal_font_size: i32,
    pub sidebar_width: i32,
    pub review_panel_width: i32,
    pub confirm_on_quit: bool,
    pub default_agent_id: Option<i64>,
    pub sandbox_cpus: i32,
    pub sandbox_memory_mb: i64,
    pub sandbox_disk_mb: i64,
    pub allowed_hosts: Option<Vec<String>>,
}

/// A sandbox snapshot for rewind.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct Snapshot {
    pub id: i64,
    pub workspace_id: i64,
    pub name: String,
    pub checkpoint_name: String,
    pub auto_generated: bool,
    pub trigger: Option<String>,
    pub created_at: String,
}
