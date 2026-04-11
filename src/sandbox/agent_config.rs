//! Agent auth setup orchestrator.
//!
//! Reads proxy placeholder env vars from the sandbox, merges with
//! gateway env vars, then dispatches to the agent's Rust module
//! for config file writes.

use crate::agents;
use crate::db::Agent;
use shuru_sdk::AsyncSandbox;
use std::collections::HashMap;
use std::sync::Arc;

pub async fn run_auth_setup(
    sandbox: &Arc<AsyncSandbox>,
    agent: &Agent,
    gateway_env: &HashMap<String, String>,
) {
    // Read proxy placeholder env vars from the sandbox
    let sandbox_env = match sandbox.exec_in("sh", "printenv").await {
        Ok(r) => r
            .stdout
            .lines()
            .filter_map(|line| line.split_once('=').map(|(k, v)| (k.to_string(), v.to_string())))
            .collect::<HashMap<_, _>>(),
        Err(e) => {
            eprintln!("[auth_setup] {} failed to read env: {e}", agent.name);
            HashMap::new()
        }
    };

    let mut vars = sandbox_env;
    vars.extend(gateway_env.iter().map(|(k, v)| (k.clone(), v.clone())));

    // Write lifecycle hook scripts
    write_hook_scripts(sandbox).await;

    // Dispatch to agent-specific auth setup
    agents::run_auth_setup(&agent.name, sandbox, &vars).await;

    // Build profile.d for interactive shell env vars
    let mut profile_lines = String::new();

    // Proxy CA cert (only if MITM is active)
    if let Ok(r) = sandbox.exec_in("sh", "test -f /usr/local/share/ca-certificates/shuru-proxy.crt && echo yes").await {
        if r.stdout.trim() == "yes" {
            write_export(&mut profile_lines, "NODE_EXTRA_CA_CERTS", "/usr/local/share/ca-certificates/shuru-proxy.crt");
            write_export(&mut profile_lines, "SSL_CERT_FILE", "/etc/ssl/certs/ca-certificates.crt");
        }
    }

    // Gateway env vars (skip internal _GATEWAY_* keys used only by auth_setup)
    for (k, v) in gateway_env {
        if !k.starts_with("_GATEWAY_") {
            write_export(&mut profile_lines, k, v);
        }
    }

    if let Err(e) = sandbox
        .write_file("/etc/profile.d/shuru-env.sh", profile_lines.as_bytes())
        .await
    {
        eprintln!("[auth_setup] {} failed to write profile.d: {e}", agent.name);
    }
}

fn write_export(lines: &mut String, k: &str, v: &str) {
    lines.push_str(&format!("export {}='{}'\n", k, v.replace('\'', "'\\''")));
}

/// Write the generic lifecycle hook script and Claude-specific wrapper.
async fn write_hook_scripts(sandbox: &AsyncSandbox) {
    // Create events directory
    let _ = sandbox.exec_in("sh", "mkdir -p /root/.superhq").await;

    // Generic hook script: appends a JSONL line to events.jsonl
    let superhq_hook = r#"#!/bin/bash
EVENT="$1"; shift
TOOL="" TITLE="" MESSAGE=""
while [ $# -gt 0 ]; do
  case "$1" in
    --tool)    TOOL="$2"; shift 2 ;;
    --title)   TITLE="$2"; shift 2 ;;
    --message) MESSAGE="$2"; shift 2 ;;
    *)         shift ;;
  esac
done
TS=$(date +%s)
LINE="{\"event\":\"${EVENT}\",\"ts\":${TS}"
[ -n "$TOOL" ]    && LINE="${LINE},\"tool\":\"${TOOL}\""
[ -n "$TITLE" ]   && LINE="${LINE},\"title\":\"${TITLE}\""
[ -n "$MESSAGE" ] && LINE="${LINE},\"message\":\"${MESSAGE}\""
LINE="${LINE}}"
echo "$LINE" >> /root/.superhq/events.jsonl
"#;

    // Claude-specific wrapper: reads JSON from stdin and forwards to superhq-hook
    let claude_hook = r#"#!/bin/bash
HOOK="$1"
INPUT=$(cat)
case "$HOOK" in
  pre_tool_use)
    TOOL=$(echo "$INPUT" | jq -r '.tool_name // empty' 2>/dev/null)
    exec superhq-hook running --tool "$TOOL"
    ;;
  notification)
    TITLE=$(echo "$INPUT" | jq -r '.title // empty' 2>/dev/null)
    MSG=$(echo "$INPUT" | jq -r '.message // empty' 2>/dev/null)
    exec superhq-hook needs_input --title "$TITLE" --message "$MSG"
    ;;
  stop)
    exec superhq-hook idle
    ;;
  session_start)
    exec superhq-hook session_start
    ;;
  session_end)
    exec superhq-hook session_end
    ;;
  prompt_submit)
    exec superhq-hook running
    ;;
esac
"#;

    if let Err(e) = sandbox
        .write_file("/usr/local/bin/superhq-hook", superhq_hook.as_bytes())
        .await
    {
        eprintln!("[hook_scripts] failed to write superhq-hook: {e}");
        return;
    }
    let _ = sandbox
        .exec_in("sh", "chmod +x /usr/local/bin/superhq-hook")
        .await;

    if let Err(e) = sandbox
        .write_file(
            "/usr/local/bin/superhq-claude-hook",
            claude_hook.as_bytes(),
        )
        .await
    {
        eprintln!("[hook_scripts] failed to write superhq-claude-hook: {e}");
        return;
    }
    let _ = sandbox
        .exec_in("sh", "chmod +x /usr/local/bin/superhq-claude-hook")
        .await;
}
