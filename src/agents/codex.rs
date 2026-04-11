use super::{home, secret_entry, write_config, AgentConfig, AuthGatewaySpec, InstallStep};
use shuru_sdk::AsyncSandbox;
use std::collections::HashMap;

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
        secrets: vec![secret_entry(
            "OPENAI_API_KEY",
            "OpenAI API Key",
            &[],
            &[],
            false,
        )],
        auth_gateway: Some(AuthGatewaySpec {
            secret_env_var: "OPENAI_API_KEY",
            upstream_base: "https://api.openai.com",
            guest_port: 9100,
            base_url_env: None, // written to config.toml instead
        }),
    }
}

pub async fn auth_setup(sandbox: &AsyncSandbox, vars: &HashMap<String, String>) {
    let home = home(vars);

    // Auth credentials
    let auth = serde_json::json!({
        "auth_mode": "apikey",
        "OPENAI_API_KEY": super::GATEWAY_DUMMY_KEY,
    });
    let auth_path = format!("{home}/.codex/auth.json");
    write_config(sandbox, &auth_path, auth.to_string().as_bytes()).await;

    // Resolve gateway base URL if set
    let base_url = vars
        .get("_GATEWAY_BASE_URL")
        .map(|s| s.as_str())
        .unwrap_or("");

    // config.toml: disable sandbox (already in VM), pin model, auto-approve,
    // trust /workspace, and point at the auth gateway.
    let base = base_url.trim_end_matches("/v1");
    let config = include_str!("../../assets/agents/codex-config.toml")
        .replace("{{OPENAI_BASE_URL}}", base);

    let config_path = format!("{home}/.codex/config.toml");
    write_config(sandbox, &config_path, config.as_bytes()).await;

    // Lifecycle hooks (same schema as Claude Code, but async not supported)
    let hooks = serde_json::json!({
        "hooks": {
            "SessionStart": [{ "hooks": [{ "type": "command", "command": "superhq-claude-hook session_start" }] }],
            "UserPromptSubmit": [{ "hooks": [{ "type": "command", "command": "superhq-claude-hook prompt_submit" }] }],
            "PreToolUse": [{ "hooks": [{ "type": "command", "command": "superhq-claude-hook pre_tool_use" }] }],
            "Stop": [{ "hooks": [{ "type": "command", "command": "superhq-claude-hook stop" }] }],
        }
    });
    // Write to both user-level and project-level config
    let hooks_bytes = hooks.to_string();
    write_config(sandbox, &format!("{home}/.codex/hooks.json"), hooks_bytes.as_bytes()).await;
    write_config(sandbox, "/workspace/.codex/hooks.json", hooks_bytes.as_bytes()).await;
}
