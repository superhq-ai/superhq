mod claude;
mod codex;
mod pi;

use crate::db::{Database, RequiredSecret, RequiredSecretEntry};
use shuru_sdk::AsyncSandbox;
use std::collections::HashMap;
use std::sync::Arc;

/// Dummy API key injected into agent config files. The auth gateway on the host
/// swaps this for the real credential before forwarding upstream.
pub const GATEWAY_DUMMY_KEY: &str = "sk-shuru-gateway";

/// Auth gateway spec — tells the boot flow to run a reverse proxy on the host
/// that swaps a dummy API key for the real credential before forwarding upstream.
#[derive(Clone, Copy)]
pub struct AuthGatewaySpec {
    /// Which secret env var the gateway authenticates with (e.g. "OPENAI_API_KEY").
    pub secret_env_var: &'static str,
    /// Upstream URL to forward requests to (e.g. "https://api.openai.com").
    pub upstream_base: &'static str,
    /// Fixed guest port for the expose_host mapping.
    pub guest_port: u16,
    /// Env var to set in the sandbox pointing to the gateway base URL.
    /// None if the agent reads it from its own config file instead.
    pub base_url_env: Option<&'static str>,
}

/// A single install step.
pub enum InstallStep {
    /// Run a shell command.
    Cmd {
        label: &'static str,
        command: &'static str,
        /// If this command succeeds (exit 0), skip this step.
        skip_if: Option<&'static str>,
    },
    /// Download a URL into the sandbox (with progress reporting).
    Download {
        label: &'static str,
        url: &'static str,
        path: &'static str,
        /// If true, treat as .tar.gz and extract to `path`.
        extract: bool,
        /// When extracting, strip N leading path components. Mirrors
        /// `tar --strip-components=N`. Use `1` for tarballs wrapped in a
        /// single top-level directory (e.g. `node-v22…/bin/node`), `0`
        /// for flat tarballs (a single binary at the archive root).
        strip_components: u32,
        /// If this command succeeds (exit 0), skip this step.
        skip_if: Option<&'static str>,
    },
    /// Rename a file inside the sandbox.
    Rename {
        label: &'static str,
        from: &'static str,
        to: &'static str,
        skip_if: Option<&'static str>,
    },
    /// Set file permissions.
    Chmod {
        label: &'static str,
        path: &'static str,
        mode: u32,
        skip_if: Option<&'static str>,
    },
    /// Multiple operations shown as one step in the UI.
    Group {
        label: &'static str,
        steps: Vec<InstallStep>,
        skip_if: Option<&'static str>,
    },
}

impl InstallStep {
    pub fn label(&self) -> &'static str {
        match self {
            InstallStep::Cmd { label, .. }
            | InstallStep::Download { label, .. }
            | InstallStep::Rename { label, .. }
            | InstallStep::Chmod { label, .. }
            | InstallStep::Group { label, .. } => label,
        }
    }

    pub fn skip_if(&self) -> Option<&'static str> {
        match self {
            InstallStep::Cmd { skip_if, .. }
            | InstallStep::Download { skip_if, .. }
            | InstallStep::Rename { skip_if, .. }
            | InstallStep::Chmod { skip_if, .. }
            | InstallStep::Group { skip_if, .. } => *skip_if,
        }
    }
}

/// Static config for a built-in agent.
pub struct AgentConfig {
    pub name: &'static str,
    pub display_name: &'static str,
    pub command: &'static str,
    pub icon: Option<&'static str>,
    pub color: Option<&'static str>,
    pub tab_order: i32,
    pub install_steps: Vec<InstallStep>,
    pub secrets: Vec<RequiredSecretEntry>,
    /// If set, this agent uses an auth gateway instead of the MITM proxy.
    pub auth_gateway: Option<AuthGatewaySpec>,
}

/// Shared install step: Node.js via pre-built tarball (fast, ~5s).
const NODE_INSTALL_STEP: InstallStep = InstallStep::Download {
    label: "Downloading Node.js",
    url: "https://nodejs.org/dist/v22.16.0/node-v22.16.0-linux-arm64.tar.gz",
    path: "/usr/local",
    extract: true,
    // Strip `node-v22.16.0-linux-arm64/` so contents land in /usr/local/{bin,lib,…}.
    strip_components: 1,
    skip_if: Some("/usr/local/bin/node --version"),
};

/// All built-in agent configs.
pub fn builtin_agents() -> Vec<AgentConfig> {
    vec![
        pi::config(),
        claude::config(),
        codex::config(),
    ]
}

/// Resolve the auth gateway spec for an agent, allowing agent-specific
/// provider selection instead of a single static gateway.
pub fn auth_gateway_spec_for_agent(
    agent_name: &str,
    db: &Database,
    fallback: Option<&AgentConfig>,
) -> Option<AuthGatewaySpec> {
    match agent_name {
        "codex" => codex::auth_gateway_spec(db),
        "pi" => pi::auth_gateway_spec(db),
        _ => fallback.and_then(|cfg| cfg.auth_gateway),
    }
}

/// Run agent-specific auth setup. Dispatches to the agent module's
/// `auth_setup` function if one exists.
///
/// `vars` contains proxy placeholder env vars, extra_env (OAuth claims),
/// and script_secrets (id_tokens). Auth files are written with
/// `sandbox.write_file()` — real secrets flow through the proxy as
/// placeholders and never hit the filesystem.
pub async fn run_auth_setup(
    agent_name: &str,
    sandbox: &Arc<AsyncSandbox>,
    vars: &HashMap<String, String>,
) {
    match agent_name {
        "claude" => claude::auth_setup(sandbox, vars).await,
        "codex" => codex::auth_setup(sandbox, vars).await,
        "pi" => pi::auth_setup(sandbox, vars).await,
        _ => {}
    }
}

/// Helper: get HOME from vars.
fn home(vars: &HashMap<String, String>) -> &str {
    vars.get("HOME").map(|s| s.as_str()).unwrap_or("/root")
}

/// Helper: ensure parent dir exists and write a file.
async fn write_config(sandbox: &AsyncSandbox, path: &str, content: &[u8]) {
    if let Some(parent) = path.rsplit_once('/').map(|(p, _)| p) {
        let _ = sandbox
            .exec_in("sh", &format!("mkdir -p '{parent}'"))
            .await;
    }
    if let Err(e) = sandbox.write_file(path, content).await {
        eprintln!("[auth_setup] failed to write {path}: {e}");
    }
}

/// Helper: build a RequiredSecretEntry.
fn secret_entry(
    env_var: &str,
    label: &str,
    hosts: &[&str],
    oauth_claims: &[(&str, &str)],
    optional: bool,
) -> RequiredSecretEntry {
    RequiredSecretEntry::Full(RequiredSecret {
        env_var: env_var.into(),
        label: Some(label.into()),
        hosts: Some(hosts.iter().map(|s| s.to_string()).collect()),
        oauth_claims: if oauth_claims.is_empty() {
            None
        } else {
            Some(
                oauth_claims
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect(),
            )
        },
        optional,
    })
}
