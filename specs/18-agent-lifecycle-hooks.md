# 18 -- Agent Lifecycle Hooks

## Problem

We have no visibility into what agents are doing inside sandboxes. Users
running multiple agents in parallel can't tell which ones are waiting for
input, running, idle, or errored -- they have to click through every tab
to check.

cmux solves this with push-based hooks: agents report their own state
back to the host. We can do the same, but better -- we control the
sandbox, so we don't need PATH shims or wrapper scripts.

## Architecture

```
┌──────────────────────────────────────────────┐
│  Sandbox                                     │
│                                              │
│  Claude Code ──hook──> superhq-hook ...       │
│  Codex       ──hook──> superhq-hook ...       │
│  Pi          ──ext───> superhq-hook ...       │
│                           │                  │
│                      append JSONL line        │
│                           ▼                  │
│              /root/.superhq/events.jsonl      │
└──────────────────────────────────────────────┘
                    │
        overlayfs upper dir inotify
                    │
                    ▼
┌──────────────────────────────────────────────┐
│  Host                                        │
│                                              │
│  EventWatcher (one per agent tab)            │
│    ├─ watches upper dir for events.jsonl     │
│    ├─ reads new lines (tail -f style)        │
│    ├─ parses JSON, updates AgentStatus       │
│    └─ sends via flume to GPUI                │
│                                              │
│  TerminalTab.agent_status: AgentStatus       │
│    ├─ Running { tool: Option<String> }       │
│    ├─ NeedsInput { message: String }         │
│    ├─ Idle                                   │
│    └─ Unknown (agent not started yet)        │
│                                              │
│  UI                                          │
│    ├─ Tab dot indicator (color)              │
│    ├─ Sidebar workspace badge                │
│    └─ macOS notification (when bg)           │
└──────────────────────────────────────────────┘
```

No HTTP server needed. No port allocation. No expose_host_ports.
The overlayfs upper dir is already on the host filesystem -- we just
watch it with inotify (same pattern as the review panel watcher).

## Components

### 1. Event File (`/root/.superhq/events.jsonl`)

Agents append one JSON line per state change:

```jsonl
{"event":"session_start","ts":1712880000}
{"event":"running","tool":"Bash","ts":1712880001}
{"event":"running","tool":"Edit","ts":1712880005}
{"event":"needs_input","title":"Permission","message":"Allow Bash?","ts":1712880010}
{"event":"running","ts":1712880015}
{"event":"idle","ts":1712880020}
{"event":"session_end","ts":1712880025}
```

Events:
- `session_start` -- agent process started
- `running` -- agent is working (optional `tool` field)
- `needs_input` -- agent waiting for user (optional `title`, `message`)
- `idle` -- agent finished turn, waiting for next prompt
- `session_end` -- agent process exited

### 2. Sandbox CLI (`/usr/local/bin/superhq-hook`)

A bash script written into the sandbox during `post_boot_setup`.
Appends a JSON line to the event file. No network, no curl, just echo.

```bash
#!/bin/bash
# Usage: superhq-hook <event> [--tool NAME] [--title TEXT] [--message TEXT]
# Appends a JSONL event to /root/.superhq/events.jsonl

EVENT="$1"; shift
TOOL="" TITLE="" MESSAGE=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --tool)    TOOL="$2"; shift 2 ;;
    --title)   TITLE="$2"; shift 2 ;;
    --message) MESSAGE="$2"; shift 2 ;;
    *)         shift ;;
  esac
done

TS=$(date +%s)
mkdir -p /root/.superhq

# Build JSON line (no jq dependency for the writer)
LINE="{\"event\":\"${EVENT}\",\"ts\":${TS}"
[ -n "$TOOL" ]    && LINE="${LINE},\"tool\":\"${TOOL}\""
[ -n "$TITLE" ]   && LINE="${LINE},\"title\":\"${TITLE}\""
[ -n "$MESSAGE" ] && LINE="${LINE},\"message\":\"${MESSAGE}\""
LINE="${LINE}}"

echo "$LINE" >> /root/.superhq/events.jsonl
```

Fast, no dependencies, no network. Works even if the agent hooks are
synchronous (blocking) since it's just a file append.

### 3. Claude Hook Wrapper (`/usr/local/bin/superhq-claude-hook`)

Claude Code hooks receive JSON on stdin. This wrapper reads stdin,
extracts relevant fields, and calls `superhq-hook`.

```bash
#!/bin/bash
# Usage: superhq-claude-hook <hook_event>
# Reads Claude hook JSON from stdin and forwards to superhq-hook.

HOOK="$1"
INPUT=$(cat)

case "$HOOK" in
  pre_tool_use)
    TOOL=$(echo "$INPUT" | jq -r '.tool_name // empty')
    superhq-hook running --tool "$TOOL"
    ;;
  notification)
    TITLE=$(echo "$INPUT" | jq -r '.title // empty')
    MSG=$(echo "$INPUT" | jq -r '.message // empty')
    superhq-hook needs_input --title "$TITLE" --message "$MSG"
    ;;
  stop)
    superhq-hook idle
    ;;
  session_start)
    superhq-hook session_start
    ;;
  session_end)
    superhq-hook session_end
    ;;
  prompt_submit)
    superhq-hook running
    ;;
esac
```

Requires `jq` in the sandbox (already in base image or add to install).

### 4. Hook Injection (per agent)

Hooks are written during each agent's `auth_setup`.

#### Claude Code

Write `~/.claude/settings.json`:

```json
{
  "hooks": {
    "PreToolUse": [{ "hooks": [{ "type": "command", "command": "superhq-claude-hook pre_tool_use", "async": true }] }],
    "Notification": [{ "hooks": [{ "type": "command", "command": "superhq-claude-hook notification", "async": true }] }],
    "Stop": [{ "hooks": [{ "type": "command", "command": "superhq-claude-hook stop", "async": true }] }],
    "SessionStart": [{ "hooks": [{ "type": "command", "command": "superhq-claude-hook session_start", "async": true }] }],
    "SessionEnd": [{ "hooks": [{ "type": "command", "command": "superhq-claude-hook session_end", "async": true }] }],
    "UserPromptSubmit": [{ "hooks": [{ "type": "command", "command": "superhq-claude-hook prompt_submit", "async": true }] }]
  }
}
```

#### Codex

Write `~/.codex/hooks.json`:

```json
{
  "session-start": "superhq-hook session_start",
  "prompt-submit": "superhq-hook running",
  "stop": "superhq-hook idle"
}
```

Codex has fewer hook points than Claude. We get session start, prompt
submit, and stop. No tool-level granularity or notification events.

#### Pi

Pi has a TypeScript extension API. Write a file to
`~/.pi/agent/extensions/superhq-hooks.ts`:

```typescript
import type { ExtensionAPI } from "@mariozechner/pi-coding-agent";
import { execSync } from "child_process";

function emit(args: string) {
  try { execSync(`superhq-hook ${args}`, { stdio: "ignore" }); } catch {}
}

export default function (pi: ExtensionAPI) {
  pi.on("session_start", async () => { emit("session_start"); });
  pi.on("session_shutdown", async () => { emit("session_end"); });
  pi.on("agent_start", async () => { emit("running"); });
  pi.on("agent_end", async () => { emit("idle"); });
  pi.on("tool_execution_start", async (event) => {
    emit(`running --tool "${event.tool?.name || ""}"`);
  });
}
```

Pi's extension system is in-process TypeScript with full access to
session state. The extension fires on every agent turn and tool call.

### 5. Event Watcher (`src/sandbox/event_watcher.rs`)

Watches the overlayfs upper dir for changes to `.superhq/events.jsonl`.
Same inotify pattern as `src/ui/review/watcher.rs`.

```rust
pub struct EventWatcher {
    /// Offset into events.jsonl (bytes read so far).
    offset: u64,
    /// Channel to send parsed events to the UI.
    tx: flume::Sender<AgentEvent>,
}
```

On inotify MODIFY for `root/.superhq/events.jsonl` in the upper dir:
1. Read from `offset` to end of file
2. Split by newlines, parse each as JSON
3. Send `AgentEvent` on the flume channel
4. Update `offset`

The receiver side runs in a GPUI `cx.spawn` that updates
`TerminalTab.agent_status` and calls `cx.notify()`.

### 6. Tab State (`src/ui/terminal/session.rs`)

```rust
#[derive(Clone, Debug, Default)]
pub enum AgentStatus {
    #[default]
    Unknown,
    Running { tool: Option<String> },
    NeedsInput { message: Option<String> },
    Idle,
}

pub struct TerminalTab {
    // ... existing fields ...
    pub agent_status: AgentStatus,
}
```

### 7. UI Indicators

#### Tab bar dot

Small colored dot on each tab:
- **Blue pulsing** -- Running (agent working)
- **Orange** -- Needs Input (waiting for user)
- **Gray** -- Idle (finished, waiting for prompt)
- **No dot** -- Unknown / not started

#### Sidebar workspace badge

If ANY tab in a workspace needs input, show an orange dot on the
workspace item in the sidebar. This lets users see at a glance which
workspaces need attention without switching to them.

#### macOS notification

When the app is NOT focused and an agent enters NeedsInput state,
fire a macOS `UNUserNotificationCenter` notification:
- Title: agent name + workspace name
- Body: the message from the hook (e.g. "Claude needs your permission")
- Click: focus app and switch to the relevant tab

## Implementation Plan

### Phase 1: Event file + watcher
1. Add `AgentStatus` to `TerminalTab`
2. `src/sandbox/event_watcher.rs` -- inotify watcher for events.jsonl
3. Start watcher after sandbox boot (read upper dir path from sandbox)
4. Bridge events to GPUI via flume channel

### Phase 2: Sandbox scripts
1. Write `/usr/local/bin/superhq-hook` bash script during `post_boot_setup`
2. Write `/usr/local/bin/superhq-claude-hook` wrapper during `claude::auth_setup`
3. Create `/root/.superhq/` directory in sandbox

### Phase 3: Agent hook injection
1. Claude: write `~/.claude/settings.json` with hooks in `claude::auth_setup`
2. Codex: write `~/.codex/hooks.json` in `codex::auth_setup`
3. Pi: write `~/.pi/agent/extensions/superhq-hooks.ts` in `pi::auth_setup`

### Phase 4: Tab UI indicators
1. Render dot on tab bar based on `agent_status`
2. Animate blue dot for running state
3. Show orange dot for needs_input

### Phase 5: Sidebar + notifications
1. Aggregate tab statuses per workspace for sidebar badge
2. `UNUserNotificationCenter` for background notifications
3. Click notification -> focus app + switch workspace + tab

## Dependencies

- `jq` must be available in sandbox (needed for Claude hook stdin
  parsing). Add to install steps or bundle in base image.
- No curl, no HTTP server, no port allocation.

## Open Questions

- Should we show the tool name in the tab (e.g. "Running: Bash")?
  cmux does this in verbose mode. Could be noisy.
- Should idle timeout to "Unknown" after N minutes? Prevents stale
  indicators if an agent crashes without firing session_end.
- Should we truncate events.jsonl periodically? It only grows.
  Could truncate on session_start or cap at 1000 lines.
