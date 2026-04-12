use crate::db::{Agent, Database};
use crate::sandbox::agent_config;
use crate::sandbox::auth_gateway::AuthGateway;
use shuru_sdk::{AsyncSandbox, ExposeHostMapping, MountConfig, SandboxConfig};
use std::collections::HashMap;
use std::sync::Arc;

/// Builds a SandboxConfig from workspace settings.
pub fn build_config(
    db: &Database,
    ws_id: i64,
    mount_path: Option<&str>,
    secrets_map: HashMap<String, shuru_sdk::SecretConfig>,
    boot_from: Option<&str>,
    gateway_spec: Option<&crate::agents::AuthGatewaySpec>,
    auth_gateway: Option<&AuthGateway>,
) -> SandboxConfig {
    let sb_settings = db.get_settings().ok();
    let mut config = SandboxConfig::default();
    config.allow_net = true;
    config.env.insert("HOME".into(), "/root".into());
    config.storage = shuru_sdk::StorageMode::Cas { cas_dir: None };
    config.cpus = sb_settings.as_ref().map(|s| s.sandbox_cpus as usize).unwrap_or(2);
    config.memory_mb = sb_settings.as_ref().map(|s| s.sandbox_memory_mb as u64).unwrap_or(8192);
    config.disk_size_mb = sb_settings.as_ref().map(|s| s.sandbox_disk_mb as u64).unwrap_or(16384);
    config.secrets = secrets_map;

    if let Some(path) = mount_path {
        config.mounts.push(MountConfig {
            host_path: path.to_string(),
            guest_path: "/workspace".to_string(),
            read_only: true,
        });
    }

    if let (Some(spec), Some(gw)) = (gateway_spec, auth_gateway) {
        config.expose_host.push(ExposeHostMapping {
            host_port: gw.host_port,
            guest_port: spec.guest_port,
        });
    }

    if let Ok(port_mappings) = db.get_port_mappings(ws_id) {
        for pm in &port_mappings {
            config.ports.push(shuru_proto::PortMapping {
                host_port: pm.host_port,
                guest_port: pm.guest_port,
            });
        }
    }

    if let Ok(expose_ports) = db.get_expose_host_ports(ws_id) {
        for ep in &expose_ports {
            config.expose_host.push(ExposeHostMapping {
                host_port: ep.host_port,
                guest_port: ep.guest_port,
            });
        }
    }

    if let Some(checkpoint) = boot_from {
        config.from = Some(checkpoint.to_string());
    }

    config
}

/// Boot a sandbox with retry (handles port bind races on fork).
pub async fn boot_with_retry(
    config: SandboxConfig,
    retries: u32,
) -> Result<Arc<AsyncSandbox>, String> {
    let mut last_err = String::new();
    for attempt in 0..retries {
        if attempt > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
        let cfg = config.clone();
        match AsyncSandbox::boot(cfg).await {
            Ok(s) => return Ok(Arc::new(s)),
            Err(e) => last_err = format!("{e}"),
        }
    }
    Err(last_err)
}

/// Post-boot setup: create /workspace, copy gitignore, write auth config.
pub async fn post_boot_setup(
    sandbox: &Arc<AsyncSandbox>,
    mount_path: Option<&str>,
    agent: Option<&Agent>,
    gateway_env: &HashMap<String, String>,
) {
    // Create /workspace for scratch sandboxes
    if mount_path.is_none() {
        let sb = sandbox.clone();
        let _ = sb.mkdir("/workspace", true).await;
    }

    // Copy host's global gitignore
    if let Some(content) = read_host_global_gitignore() {
        let sb = sandbox.clone();
        let _ = sb.mkdir("/root/.config/git", true).await;
        let _ = sb.write_file("/root/.config/git/ignore", &content).await;
    }

    // Write auth config files
    if let Some(agent) = agent {
        agent_config::run_auth_setup(sandbox, agent, gateway_env).await;
    }
}

fn read_host_global_gitignore() -> Option<Vec<u8>> {
    let home = std::env::var("HOME").ok()?;
    let path = format!("{}/.config/git/ignore", home);
    std::fs::read(&path).ok()
}

/// Build the agent environment for open_shell.
pub fn build_agent_env(gateway_env: &HashMap<String, String>) -> HashMap<String, String> {
    let mut env: HashMap<String, String> = gateway_env
        .iter()
        .filter(|(k, _)| !k.starts_with("_GATEWAY_"))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    env.insert(
        "NODE_EXTRA_CA_CERTS".into(),
        "/usr/local/share/ca-certificates/shuru-proxy.crt".into(),
    );
    env.insert(
        "SSL_CERT_FILE".into(),
        "/etc/ssl/certs/ca-certificates.crt".into(),
    );
    env
}
