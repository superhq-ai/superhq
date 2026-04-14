use super::{home, secret_entry, write_config, AgentConfig, AuthGatewaySpec, InstallStep};
use shuru_sdk::AsyncSandbox;
use std::collections::HashMap;

pub fn config() -> AgentConfig {
    AgentConfig {
        name: "pi",
        display_name: "Pi",
        command: "/usr/local/bin/pi",
        icon: Some("icons/agents/pi.svg"),
        color: Some("#FFFFFF"),
        tab_order: 3,
        install_steps: vec![
            InstallStep::Group {
                label: "Downloading Pi",
                skip_if: Some("/usr/local/bin/pi --version"),
                steps: vec![
                    InstallStep::Download {
                        label: "Downloading Pi",
                        url: "https://github.com/badlogic/pi-mono/releases/latest/download/pi-linux-arm64.tar.gz",
                        path: "/usr/local/bin",
                        extract: true,
                        skip_if: None,
                    },
                    InstallStep::Chmod {
                        label: "Setting permissions",
                        path: "/usr/local/bin/pi",
                        mode: 0o755,
                        skip_if: None,
                    },
                ],
            },
            InstallStep::Cmd {
                label: "Verifying installation",
                command: "/usr/local/bin/pi --version",
                skip_if: None,
            },
        ],
        secrets: vec![
            secret_entry(
                "ANTHROPIC_API_KEY",
                "Anthropic API Key",
                &["api.anthropic.com"],
                &[],
                true,
            ),
            secret_entry(
                "OPENAI_API_KEY",
                "OpenAI API Key",
                &[],
                &[],
                true,
            ),
        ],
        auth_gateway: Some(AuthGatewaySpec {
            secret_env_var: "OPENAI_API_KEY",
            upstream_base: "https://api.openai.com",
            guest_port: 9101,
            base_url_env: None, // written to models.json instead
        }),
    }
}

pub async fn auth_setup(sandbox: &AsyncSandbox, vars: &HashMap<String, String>) {
    let home = home(vars);
    let gateway_url = vars.get("_GATEWAY_BASE_URL").map(|s| s.as_str()).unwrap_or("");
    // Only write models.json if the gateway is available (user has OpenAI key)
    if gateway_url.is_empty() {
        return;
    }

    // Strip /v1 suffix — Pi's baseUrl should be the root
    let base_url = gateway_url.trim_end_matches("/v1");

    let is_oauth = vars.get("_GATEWAY_AUTH_METHOD").map(|s| s == "oauth").unwrap_or(false);

    let (api_type, api_key, models) = if is_oauth {
        // OAuth: use openai-codex-responses (handles instructions field,
        // chatgpt-account-id header). apiKey is a stub JWT with just the
        // accountId — the gateway swaps it for the real OAuth token.
        let jwt = vars.get("_GATEWAY_STUB_JWT")
            .cloned()
            .unwrap_or_else(|| super::GATEWAY_DUMMY_KEY.to_string());
        (
            "openai-codex-responses",
            jwt,
            serde_json::json!([
                { "id": "gpt-5.4", "reasoning": true },
                { "id": "gpt-5.4-mini", "reasoning": true },
                { "id": "gpt-5.3-codex", "reasoning": true },
                { "id": "gpt-5.3-codex-spark", "reasoning": true },
                { "id": "gpt-5.2-codex", "reasoning": true },
                { "id": "gpt-5.2", "reasoning": true },
                { "id": "gpt-5.1-codex-max", "reasoning": true },
                { "id": "gpt-5.1-codex-mini", "reasoning": true },
                { "id": "gpt-5.1", "reasoning": true },
            ]),
        )
    } else {
        (
            "openai-responses",
            super::GATEWAY_DUMMY_KEY.to_string(),
            serde_json::json!([
                { "id": "gpt-5.4", "reasoning": true },
                { "id": "gpt-5.2-codex", "reasoning": true },
                { "id": "gpt-5.1-codex", "reasoning": true },
                { "id": "gpt-5.1-codex-max", "reasoning": true },
                { "id": "gpt-5-chat-latest" },
                { "id": "gpt-5.3-codex-spark", "reasoning": true },
            ]),
        )
    };

    let config = serde_json::json!({
        "providers": {
            "openai-gateway": {
                "baseUrl": format!("{}/v1", base_url),
                "api": api_type,
                "apiKey": api_key,
                "models": models,
            }
        }
    });

    let config_path = format!("{home}/.pi/agent/models.json");
    write_config(sandbox, &config_path, config.to_string().as_bytes()).await;

    // Lifecycle hooks via Pi extension API
    let extension = r#"import { execSync } from "child_process";
function emit(args) {
  try { execSync(`superhq-hook ${args}`, { stdio: "ignore", timeout: 5000 }); } catch {}
}
export default function (pi) {
  pi.on("session_start", async () => { emit("session_start"); });
  pi.on("session_shutdown", async () => { emit("session_end"); });
  pi.on("agent_start", async () => { emit("running"); });
  pi.on("agent_end", async () => { emit("idle"); });
  pi.on("tool_execution_start", async (event) => {
    const tool = event.tool?.name || "";
    emit(`running --tool "${tool}"`);
  });
}
"#;
    let ext_path = format!("{home}/.pi/agent/extensions/superhq-hooks.ts");
    write_config(sandbox, &ext_path, extension.as_bytes()).await;
}
