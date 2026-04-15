use super::{home, secret_entry, write_config, AgentConfig, AuthGatewaySpec, InstallStep};
use crate::db::Database;
use shuru_sdk::AsyncSandbox;
use std::collections::HashMap;

const OPENAI_GATEWAY_PORT: u16 = 9100;
const OPENROUTER_GATEWAY_PORT: u16 = 9102;

#[derive(Clone, Copy, PartialEq, Eq)]
enum CodexProvider {
    OpenAI,
    OpenRouter,
}

fn resolve_provider(db: &Database) -> CodexProvider {
    if db.has_secret("OPENROUTER_API_KEY").unwrap_or(false) {
        CodexProvider::OpenRouter
    } else {
        CodexProvider::OpenAI
    }
}

pub fn auth_gateway_spec(db: &Database) -> Option<AuthGatewaySpec> {
    let provider = resolve_provider(db);
    Some(match provider {
        CodexProvider::OpenAI => AuthGatewaySpec {
            secret_env_var: "OPENAI_API_KEY",
            upstream_base: "https://api.openai.com",
            guest_port: OPENAI_GATEWAY_PORT,
            base_url_env: None,
        },
        CodexProvider::OpenRouter => AuthGatewaySpec {
            secret_env_var: "OPENROUTER_API_KEY",
            upstream_base: "https://openrouter.ai/api/v1",
            guest_port: OPENROUTER_GATEWAY_PORT,
            base_url_env: None,
        },
    })
}

pub fn config() -> AgentConfig {
    AgentConfig {
        name: "codex",
        display_name: "Codex",
        command: "/usr/local/bin/codex",
        icon: Some("icons/agents/codex.svg"),
        color: Some("#7A9DFF"),
        tab_order: 1,
        install_steps: vec![
            InstallStep::Group {
                label: "Installing Codex",
                skip_if: Some("/usr/local/bin/codex --version"),
                steps: vec![
                    InstallStep::Download {
                        label: "Downloading Codex",
                        url: "https://github.com/openai/codex/releases/latest/download/codex-aarch64-unknown-linux-musl.tar.gz",
                        path: "/usr/local/bin",
                        extract: true,
                        // Flat tarball — binary sits at the archive root.
                        strip_components: 0,
                        skip_if: None,
                    },
                    InstallStep::Rename {
                        label: "Installing binary",
                        from: "/usr/local/bin/codex-aarch64-unknown-linux-musl",
                        to: "/usr/local/bin/codex",
                        skip_if: None,
                    },
                    InstallStep::Chmod {
                        label: "Setting permissions",
                        path: "/usr/local/bin/codex",
                        mode: 0o755,
                        skip_if: None,
                    },
                ],
            },
            InstallStep::Cmd {
                label: "Setting up environment",
                command: "ln -sf /bin/true /usr/bin/bwrap",
                skip_if: None,
            },
            InstallStep::Cmd {
                label: "Verifying installation",
                command: "/usr/local/bin/codex --version",
                skip_if: None,
            },
        ],
        secrets: vec![
            secret_entry("OPENROUTER_API_KEY", "OpenRouter API Key", &[], &[], true),
            secret_entry("OPENAI_API_KEY", "OpenAI API Key", &[], &[], true),
        ],
        auth_gateway: None,
    }
}

pub async fn auth_setup(sandbox: &AsyncSandbox, vars: &HashMap<String, String>) {
    let home = home(vars);
    let provider = match vars.get("_CODEX_PROVIDER").map(|s| s.as_str()) {
        Some("openrouter") => CodexProvider::OpenRouter,
        _ => CodexProvider::OpenAI,
    };

    let base_url = vars
        .get("_GATEWAY_BASE_URL")
        .map(|s| s.as_str())
        .unwrap_or("");

    if provider == CodexProvider::OpenAI {
        let auth = serde_json::json!({
            "auth_mode": "apikey",
            "OPENAI_API_KEY": super::GATEWAY_DUMMY_KEY,
        });
        let auth_path = format!("{home}/.codex/auth.json");
        write_config(sandbox, &auth_path, auth.to_string().as_bytes()).await;
    }

    let config = match provider {
        CodexProvider::OpenAI => {
            let base = base_url.trim_end_matches("/v1");
            include_str!("../../assets/agents/codex-config.toml")
                .replace("{{OPENAI_BASE_URL}}", base)
        }
        CodexProvider::OpenRouter => format!(
            r#"model = "openai/gpt-5.4"
model_provider = "openrouter"
sandbox_mode = "danger-full-access"
approval_policy = "never"

[features]
codex_hooks = true

[model_providers.openrouter]
name = "OpenRouter"
base_url = "{base_url}"
env_key = "OPENROUTER_API_KEY"
wire_api = "responses"
supports_websockets = false

[projects."/workspace"]
trust_level = "trusted"
"#,
        ),
    };

    let config_path = format!("{home}/.codex/config.toml");
    write_config(sandbox, &config_path, config.as_bytes()).await;

    // Lifecycle hooks (same schema as Claude Code)
    let hooks = serde_json::json!({
        "hooks": {
            "SessionStart": [{ "hooks": [{ "type": "command", "command": "/usr/local/bin/superhq-claude-hook session_start" }] }],
            "UserPromptSubmit": [{ "hooks": [{ "type": "command", "command": "/usr/local/bin/superhq-claude-hook prompt_submit" }] }],
            "PreToolUse": [{ "hooks": [{ "type": "command", "command": "/usr/local/bin/superhq-claude-hook pre_tool_use" }] }],
            "Stop": [{ "hooks": [{ "type": "command", "command": "/usr/local/bin/superhq-claude-hook stop" }] }],
        }
    });
    let hooks_path = format!("{home}/.codex/hooks.json");
    write_config(sandbox, &hooks_path, hooks.to_string().as_bytes()).await;
}
