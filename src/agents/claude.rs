use super::{home, secret_entry, write_config, AgentConfig, InstallStep, NODE_INSTALL_STEP};
use shuru_sdk::AsyncSandbox;
use std::collections::HashMap;

pub fn config() -> AgentConfig {
    AgentConfig {
        name: "claude",
        display_name: "Claude Code",
        command: "/usr/local/bin/claude",
        icon: Some("icons/agents/claude.svg"),
        color: Some("#D97757"),
        tab_order: 0,
        install_steps: vec![
            NODE_INSTALL_STEP,
            InstallStep::Cmd {
                label: "Installing Claude Code",
                command: "/usr/local/bin/npm install -g @anthropic-ai/claude-code",
                skip_if: None,
            },
            InstallStep::Cmd {
                label: "Verifying installation",
                command: "/usr/local/bin/claude --version",
                skip_if: None,
            },
        ],
        secrets: vec![secret_entry(
            "ANTHROPIC_API_KEY",
            "Anthropic API Key",
            &["api.anthropic.com"],
            &[],
            false,
        )],
        auth_gateway: None,
    }
}

pub async fn auth_setup(sandbox: &AsyncSandbox, vars: &HashMap<String, String>) {
    let home = home(vars);

    // Inject lifecycle hooks so Claude reports state back via superhq-hook
    let settings = serde_json::json!({
        "hooks": {
            "PreToolUse": [{ "hooks": [{ "type": "command", "command": "superhq-claude-hook pre_tool_use", "async": true }] }],
            "Notification": [{ "hooks": [{ "type": "command", "command": "superhq-claude-hook notification", "async": true }] }],
            "Stop": [{ "hooks": [{ "type": "command", "command": "superhq-claude-hook stop", "async": true }] }],
            "SessionStart": [{ "hooks": [{ "type": "command", "command": "superhq-claude-hook session_start", "async": true }] }],
            "SessionEnd": [{ "hooks": [{ "type": "command", "command": "superhq-claude-hook session_end", "async": true }] }],
            "UserPromptSubmit": [{ "hooks": [{ "type": "command", "command": "superhq-claude-hook prompt_submit", "async": true }] }],
        }
    });

    let settings_path = format!("{home}/.claude/settings.json");
    write_config(sandbox, &settings_path, settings.to_string().as_bytes()).await;
}
