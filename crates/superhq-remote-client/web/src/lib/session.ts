// Establish an authenticated session against a paired host. Loads the
// WebAuthn-wrapped device credential (cache hit on page-load unlock,
// cold-path prompts once), opens an iroh connection via the WASM
// client, and calls `session.hello` to prime the snapshot.
//
// Callers get back a live `ClientHandle` to reuse for follow-up RPCs
// (tabs.list, pty.attach, …). Closing it is the caller's job.

import { connect, makeCredentialHandle, type ClientHandle } from "./wasm";
import { loadCredential } from "./storage";
import type {
    AgentInfo,
    SessionHelloResult,
    TabCreateSpec,
    TabInfo,
    WorkspaceInfo,
} from "./types";

export interface SessionBootstrap {
    client: ClientHandle;
    workspaces: WorkspaceInfo[];
    tabs: TabInfo[];
    agents: AgentInfo[];
    hello: SessionHelloResult;
}

export async function establishSession(peerId: string): Promise<SessionBootstrap> {
    const cred = await loadCredential(peerId);
    if (!cred) {
        throw new Error("No stored credential for this host. Pair again.");
    }
    const client = await connect(peerId);
    const label = navigator.platform || "browser";
    const credHandle = await makeCredentialHandle(cred.device_id, cred.device_key);
    let json: string;
    try {
        json = await client.session_hello_auth(label, credHandle);
    } catch (e) {
        try {
            client.close();
        } catch {
            /* no-op */
        }
        throw e;
    }
    const hello = JSON.parse(json) as SessionHelloResult;
    return {
        client,
        workspaces: hello.workspaces,
        tabs: hello.tabs,
        agents: hello.agents ?? [],
        hello,
    };
}

export async function refreshSnapshot(
    client: ClientHandle,
): Promise<{ workspaces: WorkspaceInfo[]; tabs: TabInfo[] }> {
    const [wsJson, tabsJson] = await Promise.all([
        client.workspaces_list(),
        client.tabs_list(),
    ]);
    return {
        workspaces: JSON.parse(wsJson) as WorkspaceInfo[],
        tabs: JSON.parse(tabsJson) as TabInfo[],
    };
}

/// Start a background loop that drains host-pushed notifications and
/// invokes `onInvalidated` whenever the host signals its snapshot
/// has changed. Resolves when the client disconnects. Cancel by
/// calling `close` on the returned handle.
export function watchNotifications(
    client: ClientHandle,
    onInvalidated: () => void,
): { close: () => void } {
    let stopped = false;
    (async () => {
        while (!stopped) {
            let payload: string | undefined;
            try {
                payload = await client.next_notification();
            } catch {
                return;
            }
            if (!payload) return;
            try {
                const note = JSON.parse(payload) as { method: string };
                if (note.method === "snapshot.invalidated") {
                    onInvalidated();
                }
            } catch {
                /* ignore malformed lines */
            }
        }
    })();
    return {
        close: () => {
            stopped = true;
        },
    };
}

/// Ask the host to spin up a stopped workspace (restore checkpointed
/// tabs, auto-launch the default agent, etc). Resolves with the
/// post-activation snapshot of that workspace.
export async function activateWorkspace(
    client: ClientHandle,
    workspaceId: number,
): Promise<{ workspace: WorkspaceInfo; tabs: TabInfo[] }> {
    const json = await client.workspace_activate(BigInt(workspaceId));
    return JSON.parse(json) as { workspace: WorkspaceInfo; tabs: TabInfo[] };
}

/// Create a new tab in the given workspace. Spec kinds:
///   `{kind:"host_shell"}`
///   `{kind:"guest_shell", parent_tab_id}`
///   `{kind:"agent", agent_id?}`  (default agent when `agent_id` omitted)
/// Returns just the new tab's identifier — the full `TabInfo` arrives
/// via a `snapshot.invalidated` push once the host has the tab in its
/// session state.
export async function createTab(
    client: ClientHandle,
    workspaceId: number,
    spec: TabCreateSpec,
): Promise<{ workspace_id: number; tab_id: number }> {
    const json = await client.tabs_create(
        BigInt(workspaceId),
        JSON.stringify(spec),
    );
    return JSON.parse(json) as { workspace_id: number; tab_id: number };
}

/// Close a tab. `mode: "checkpoint"` snapshots an agent's sandbox and
/// leaves a stopped row the user can resume later; `mode: "force"`
/// tears the tab down entirely. The host will push a fresh snapshot
/// via `snapshot.invalidated` when the close completes.
export async function closeTab(
    client: ClientHandle,
    workspaceId: number,
    tabId: number,
    mode: "checkpoint" | "force",
): Promise<void> {
    await client.tabs_close(BigInt(workspaceId), BigInt(tabId), mode);
}
