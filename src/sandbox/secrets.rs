use crate::db::RequiredSecretEntry;
use crate::db::Database;
use crate::oauth;
use anyhow::Result;
use shuru_sdk::SecretConfig;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

/// Default host patterns for well-known API key env vars.
/// Used as fallback when the RequiredSecret doesn't specify hosts
/// and the DB secret has no hosts stored.
pub fn default_hosts(env_var: &str) -> Vec<String> {
    match env_var {
        "ANTHROPIC_API_KEY" => vec!["api.anthropic.com".into()],
        "OPENAI_API_KEY" => vec![
            "api.openai.com".into(),
            "chatgpt.com".into(),
        ],
        "OPENROUTER_API_KEY" => vec!["openrouter.ai".into()],
        _ => vec![],
    }
}

/// Check which required secrets are missing from the vault.
pub fn check_missing(db: &Database, required: &[RequiredSecretEntry]) -> Vec<RequiredSecretEntry> {
    // Required secrets: all must be present
    let mut missing: Vec<RequiredSecretEntry> = required
        .iter()
        .filter(|e| !e.is_optional() && !db.has_secret(e.env_var()).unwrap_or(false))
        .cloned()
        .collect();

    // Optional secrets: at least one must be present (if any exist)
    let optional: Vec<&RequiredSecretEntry> = required.iter().filter(|e| e.is_optional()).collect();
    if !optional.is_empty() {
        let has_any = optional.iter().any(|e| db.has_secret(e.env_var()).unwrap_or(false));
        if !has_any {
            // Report all optional secrets as missing so user can pick one
            missing.extend(optional.iter().map(|e| (*e).clone()));
        }
    }

    missing
}

pub struct ResolvedSecrets {
    pub secrets: HashMap<String, SecretConfig>,
}

/// Build the secrets map for `SandboxConfig.secrets`.
/// Decrypts each required secret and constructs `SecretConfig` with direct values.
///
/// `gateway_env_vars` contains env var names handled by an auth gateway — these
/// are skipped for both MITM proxy setup and OAuth token injection (the gateway
/// handles auth on the host side).
pub fn build_secrets_map(
    db: &Database,
    required: &[RequiredSecretEntry],
    gateway_env_vars: &HashSet<&str>,
) -> Result<ResolvedSecrets> {
    let mut secrets = HashMap::new();

    for entry in required {
        let env_var: &str = entry.env_var();
        let full = entry.as_full();

        // Skip secrets handled by the auth gateway — no MITM proxy or OAuth
        // token injection needed. The gateway looks up credentials directly.
        if gateway_env_vars.contains(env_var) {
            continue;
        }

        if let Some(value) = db.get_secret_value(env_var)? {
            // Resolve hosts: RequiredSecret.hosts > DB hosts > default_hosts
            let hosts: Vec<String> = if let Some(h) = full.hosts {
                h
            } else {
                let db_hosts = db.get_secret_hosts(env_var)?;
                if db_hosts.is_empty() {
                    default_hosts(env_var)
                } else {
                    db_hosts
                }
            };

            secrets.insert(
                env_var.to_string(),
                SecretConfig {
                    from: env_var.to_string(),
                    hosts,
                    value: Some(value),
                },
            );
        }
    }

    Ok(ResolvedSecrets { secrets })
}

/// Refresh any OAuth tokens that are near expiry. Call this before building the secrets map.
pub async fn refresh_oauth_tokens(db: &Arc<Database>, required: &[RequiredSecretEntry]) -> Result<()> {
    for entry in required {
        let env_var: &str = entry.env_var();
        if let Err(e) = oauth::refresh_if_needed(db, env_var).await {
            eprintln!("OAuth token refresh failed for {env_var}: {e}");
        }
    }
    Ok(())
}
