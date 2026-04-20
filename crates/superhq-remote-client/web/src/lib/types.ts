// Mirrors the subset of `superhq-remote-proto` types we render on the
// client. The wire format is plain JSON (session_hello_auth returns a
// JSON string we parse here), so strict runtime validation isn't
// necessary — but keeping these types in one place makes the callers
// typesafe and acts as a pseudo-schema doc.

export type WorkspaceId = number;
export type TabId = number;

export type TabKind = "agent" | "shell" | "host_shell";

export type AgentState =
    | { state: "unknown" }
    | { state: "running"; tool?: string | null }
    | { state: "needs_input"; message?: string | null }
    | { state: "idle" };

export interface WorkspaceInfo {
    workspace_id: WorkspaceId;
    label: string;
    is_active: boolean;
    /// Repo name (mount basename) for git-backed workspaces.
    repo_name?: string | null;
    /// Current git branch.
    branch?: string | null;
    /// GitHub owner/org — clients can render
    /// `https://github.com/{owner}.png` as the workspace avatar.
    github_owner?: string | null;
}

export interface TabInfo {
    workspace_id: WorkspaceId;
    tab_id: TabId;
    label: string;
    kind: TabKind;
    agent_state: AgentState;
    /// True only once the host has a live PTY registered for this tab —
    /// remote clients should not attempt `pty.attach` before this flips.
    pty_ready?: boolean;
    /// Populated when the host failed to bring this tab up (e.g. agent
    /// whose required secrets aren't configured). When set, `pty_ready`
    /// will stay false forever — render the error instead of waiting.
    setup_error?: string | null;
}

export interface HostInfo {
    app_version: string;
    os: string;
    hostname: string;
}

export interface AgentInfo {
    id: number;
    display_name: string;
    slug?: string | null;
    /// Inline SVG markup — renderable via `dangerouslySetInnerHTML`
    /// inside a styled wrapper that sets `currentColor` to the
    /// agent's accent.
    icon_svg?: string | null;
    color?: string | null;
}

export interface SessionHelloResult {
    protocol_version: number;
    session_id: string;
    resume_token: string;
    host_info: HostInfo;
    workspaces: WorkspaceInfo[];
    tabs: TabInfo[];
    agents: AgentInfo[];
}

export type TabCreateSpec =
    | { kind: "host_shell" }
    | { kind: "guest_shell"; parent_tab_id: number }
    | { kind: "agent"; agent_id?: number };
