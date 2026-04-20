// Workspace detail — mirrors the desktop layout but stacked for mobile:
//
//   ┌────────────────────────────────────────┐
//   │ ←  Workspace name · Active             │
//   ├────────────────────────────────────────┤
//   │ tab1   tab2   tab3                 +   │ ← tab bar
//   ├────────────────────────────────────────┤
//   │         terminal viewport              │
//   ├────────────────────────────────────────┤
//   │ Esc  Tab  Ctrl  ↑  ↓  ←  →             │
//   └────────────────────────────────────────┘
//
// The `+` opens a bottom-sheet menu matching the desktop's agent-menu:
// per running agent → "Guest Shell (label)", then "Host Shell", then
// one row per configured agent (with its inline SVG icon + color).

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useNavigate, useParams } from "react-router";
import Screen from "../components/Screen";
import Sheet from "../components/Sheet";
import TerminalHost, { type TerminalHandle } from "../components/Terminal";
import { useConnectionStore } from "../state/store";
import { closeTab, createTab, refreshSnapshot } from "../lib/session";
import { track } from "../lib/analytics";
import type { AgentInfo, TabCreateSpec, TabInfo } from "../lib/types";

export default function WorkspaceRoute() {
    const navigate = useNavigate();
    const params = useParams();
    const wsId = Number(params.ws);

    const workspaces = useConnectionStore((s) => s.workspaces);
    const tabs = useConnectionStore((s) => s.tabs);
    const agents = useConnectionStore((s) => s.agents);
    const allowHostShell = useConnectionStore((s) => s.allowHostShell);
    const client = useConnectionStore((s) => s.client);
    const replaceSnapshot = useConnectionStore((s) => s.replaceSnapshot);

    const workspace = useMemo(
        () => workspaces.find((w) => w.workspace_id === wsId),
        [workspaces, wsId],
    );
    const wsTabs = useMemo(
        () => tabs.filter((t) => t.workspace_id === wsId),
        [tabs, wsId],
    );

    const [activeTabId, setActiveTabId] = useState<number | null>(null);
    const [menuOpen, setMenuOpen] = useState(false);
    const [busyCreate, setBusyCreate] = useState(false);
    const [actionError, setActionError] = useState<string | null>(null);
    const [ctrlArmed, setCtrlArmed] = useState(false);
    const [altArmed, setAltArmed] = useState(false);
    const [closeTarget, setCloseTarget] = useState<TabInfo | null>(null);
    const [busyClose, setBusyClose] = useState(false);
    const terminalRef = useRef<TerminalHandle | null>(null);

    useEffect(() => {
        if (wsTabs.length === 0) {
            setActiveTabId(null);
            return;
        }
        if (
            activeTabId == null ||
            !wsTabs.some((t) => t.tab_id === activeTabId)
        ) {
            setActiveTabId(wsTabs[0].tab_id);
        }
    }, [wsTabs, activeTabId]);

    const activeTab = useMemo(
        () => wsTabs.find((t) => t.tab_id === activeTabId) ?? null,
        [wsTabs, activeTabId],
    );

    const runningAgentTabs = useMemo(
        () => wsTabs.filter((t) => t.kind === "agent"),
        [wsTabs],
    );


    const createWithSpec = useCallback(
        async (spec: TabCreateSpec) => {
            if (!client) return;
            setMenuOpen(false);
            setBusyCreate(true);
            setActionError(null);
            try {
                const created = await createTab(client, wsId, spec);
                const snap = await refreshSnapshot(client);
                replaceSnapshot(snap.workspaces, snap.tabs);
                setActiveTabId(created.tab_id);
                track("tab.open", { kind: spec.kind });
            } catch (e) {
                setActionError(
                    e instanceof Error ? e.message : "Failed to create tab",
                );
            } finally {
                setBusyCreate(false);
            }
        },
        [client, wsId, replaceSnapshot],
    );

    // Desktop treats live agent tabs (kind: agent with a running sandbox)
    // as worth prompting about — mirror that here via `pty_ready`. Shells
    // and stopped/checkpointed agents close directly.
    const requestClose = useCallback(
        (t: TabInfo) => {
            const needsConfirm = t.kind === "agent" && !!t.pty_ready;
            if (needsConfirm) {
                setCloseTarget(t);
            } else {
                void performClose(t, "force");
            }
        },
        // eslint-disable-next-line react-hooks/exhaustive-deps
        [client, wsId],
    );

    const performClose = useCallback(
        async (t: TabInfo, mode: "checkpoint" | "force") => {
            if (!client) return;
            setBusyClose(true);
            setActionError(null);
            try {
                await closeTab(client, wsId, t.tab_id, mode);
                const snap = await refreshSnapshot(client);
                replaceSnapshot(snap.workspaces, snap.tabs);
                setCloseTarget(null);
                track("tab.close", { mode, kind: t.kind });
            } catch (e) {
                setActionError(
                    e instanceof Error ? e.message : "Failed to close tab",
                );
            } finally {
                setBusyClose(false);
            }
        },
        [client, wsId, replaceSnapshot],
    );

    if (!workspace) {
        return (
            <Screen>
                <div
                    className="flex items-center gap-1.5 px-2"
                    style={{
                        paddingTop: "calc(env(safe-area-inset-top) + 6px)",
                        paddingBottom: "8px",
                    }}
                >
                    <button
                        onClick={() => navigate("/")}
                        className="glass-pill flex h-9 w-9 items-center justify-center rounded-full text-app-text"
                        aria-label="Back"
                    >
                        <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth={2.2} strokeLinecap="round" strokeLinejoin="round">
                            <path d="M15 18l-6-6 6-6" />
                        </svg>
                    </button>
                </div>
                <div className="flex flex-1 items-center justify-center text-sm text-app-text-muted">
                    That workspace isn’t in the current snapshot.
                </div>
            </Screen>
        );
    }

    return (
        <Screen>
            <NavTabs
                agents={agents}
                tabs={wsTabs}
                activeTabId={activeTabId}
                onSelect={setActiveTabId}
                onBack={() => navigate("/")}
                onOpenMenu={() => setMenuOpen(true)}
                onRequestClose={requestClose}
                busyCreate={busyCreate}
            />

            {actionError ? (
                <div className="mx-3 mt-2 rounded-xl bg-red-500/10 px-3 py-2 text-xs text-app-error">
                    {actionError}
                </div>
            ) : null}

            {client && wsTabs.length > 0 ? (
                // One persistent xterm per ready tab, stacked and
                // visibility-toggled — switching tabs is now instant
                // and doesn't flicker through a scrollback replay.
                // Booting / setup-error overlays paint on top when
                // the active tab isn't ready yet.
                <div className="relative flex flex-1 overflow-hidden">
                    <TerminalHost
                        ref={terminalRef}
                        client={client}
                        workspaceId={wsId}
                        tabs={wsTabs.filter((t) => t.pty_ready)}
                        activeTabId={
                            activeTab && activeTab.pty_ready && !activeTab.setup_error
                                ? activeTab.tab_id
                                : null
                        }
                    />
                    {activeTab && activeTab.setup_error ? (
                        <div className="absolute inset-0 bg-app-base">
                            <SetupErrorView tab={activeTab} />
                        </div>
                    ) : activeTab && !activeTab.pty_ready ? (
                        <div className="absolute inset-0 bg-app-base">
                            <BootingView tab={activeTab} />
                        </div>
                    ) : null}
                </div>
            ) : (
                <div className="flex flex-1 items-center justify-center px-4 text-center text-xs text-app-text-muted">
                    No tab open. Tap + to start one.
                </div>
            )}

            <KeyBar
                ctrlArmed={ctrlArmed}
                altArmed={altArmed}
                onPress={(bytes) => terminalRef.current?.send(bytes)}
                onToggleCtrl={() => {
                    const next = !ctrlArmed;
                    setCtrlArmed(next);
                    terminalRef.current?.setCtrlArmed(next);
                }}
                onToggleAlt={() => setAltArmed((v) => !v)}
            />

            <NewTabSheet
                open={menuOpen}
                agents={agents}
                runningAgentTabs={runningAgentTabs}
                allowHostShell={allowHostShell}
                onClose={() => setMenuOpen(false)}
                onPick={createWithSpec}
            />

            <CloseTabSheet
                tab={closeTarget}
                busy={busyClose}
                onCancel={() => setCloseTarget(null)}
                onConfirm={(mode) => closeTarget && performClose(closeTarget, mode)}
            />
        </Screen>
    );
}

function NavTabs({
    agents,
    tabs,
    activeTabId,
    onSelect,
    onBack,
    onOpenMenu,
    onRequestClose,
    busyCreate,
}: {
    agents: AgentInfo[];
    tabs: TabInfo[];
    activeTabId: number | null;
    onSelect: (id: number) => void;
    onBack: () => void;
    onOpenMenu: () => void;
    onRequestClose: (t: TabInfo) => void;
    busyCreate: boolean;
}) {
    const activeRef = useRef<HTMLDivElement | null>(null);
    useEffect(() => {
        activeRef.current?.scrollIntoView({
            behavior: "smooth",
            block: "nearest",
            inline: "nearest",
        });
    }, [activeTabId]);
    return (
        <div
            className="relative z-10 flex shrink-0 items-center gap-1.5 px-2"
            style={{
                paddingTop: "calc(env(safe-area-inset-top) + 6px)",
                paddingBottom: "8px",
            }}
        >
            <button
                onClick={onBack}
                className="glass-pill flex h-9 w-9 shrink-0 items-center justify-center rounded-full text-app-text"
                aria-label="Back"
            >
                <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth={2.2} strokeLinecap="round" strokeLinejoin="round">
                    <path d="M15 18l-6-6 6-6" />
                </svg>
            </button>
            <div className="no-scrollbar flex min-w-0 flex-1 items-center gap-1 overflow-x-auto">
                {tabs.map((t) => {
                    const active = t.tab_id === activeTabId;
                    return (
                        <div
                            key={t.tab_id}
                            ref={active ? activeRef : undefined}
                            className={[
                                "flex h-9 shrink-0 items-center rounded-full transition-colors",
                                active
                                    ? "glass-pill text-app-text"
                                    : "text-app-text-muted active:bg-white/5",
                            ].join(" ")}
                        >
                            <button
                                onClick={() => onSelect(t.tab_id)}
                                className={[
                                    "flex h-full items-center gap-1.5 rounded-full text-[12.5px] font-medium",
                                    active ? "pl-3 pr-1.5" : "px-3",
                                ].join(" ")}
                            >
                                <TabGlyph tab={t} agents={agents} size={12} />
                                <span className="max-w-[140px] truncate">
                                    {t.label}
                                </span>
                                <AgentStateDot state={t.agent_state} />
                            </button>
                            {active ? (
                                <button
                                    onClick={() => onRequestClose(t)}
                                    aria-label="Close tab"
                                    className="flex h-5 w-5 items-center justify-center rounded-full mr-1.5 text-app-text-muted active:text-app-text active:bg-white/10"
                                >
                                    <svg width="10" height="10" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth={2.6} strokeLinecap="round" strokeLinejoin="round">
                                        <line x1="18" y1="6" x2="6" y2="18" />
                                        <line x1="6" y1="6" x2="18" y2="18" />
                                    </svg>
                                </button>
                            ) : null}
                        </div>
                    );
                })}
            </div>
            <button
                onClick={onOpenMenu}
                disabled={busyCreate}
                className="glass-pill flex h-9 w-9 shrink-0 items-center justify-center rounded-full text-app-text disabled:opacity-40"
                aria-label="New tab"
            >
                {busyCreate ? (
                    <span className="text-xs">…</span>
                ) : (
                    <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth={2.2} strokeLinecap="round" strokeLinejoin="round">
                        <line x1="12" y1="5" x2="12" y2="19" />
                        <line x1="5" y1="12" x2="19" y2="12" />
                    </svg>
                )}
            </button>
        </div>
    );
}


function BootingView({ tab }: { tab: TabInfo }) {
    return (
        <div className="flex flex-1 flex-col items-center justify-center gap-3 bg-app-base px-6 text-center">
            <span className="h-2 w-2 animate-pulse rounded-full bg-app-accent" />
            <div className="text-sm text-app-text">
                Starting <span className="font-medium">{tab.label}</span>…
            </div>
            <div className="text-xs text-app-text-muted">
                Waiting for the host to finish booting the sandbox.
            </div>
        </div>
    );
}

function SetupErrorView({ tab }: { tab: TabInfo }) {
    return (
        <div className="flex flex-1 flex-col items-center justify-center gap-3 bg-app-base px-6 text-center">
            <span className="flex h-8 w-8 items-center justify-center rounded-full bg-white/5 text-app-text-muted">
                <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth={2} strokeLinecap="round" strokeLinejoin="round">
                    <path d="M10.29 3.86 1.82 18a2 2 0 0 0 1.71 3h16.94a2 2 0 0 0 1.71-3L13.71 3.86a2 2 0 0 0-3.42 0Z" />
                    <line x1="12" y1="9" x2="12" y2="13" />
                    <line x1="12" y1="17" x2="12.01" y2="17" />
                </svg>
            </span>
            <div className="text-sm text-app-text">
                <span className="font-medium">{tab.label}</span> couldn&apos;t start
            </div>
            <div className="max-w-[28ch] text-xs text-app-text-muted">
                {tab.setup_error}
            </div>
            <div className="max-w-[32ch] pt-1 text-[11px] text-app-text-muted/70">
                Fix the setup on the host, then start a new tab.
            </div>
        </div>
    );
}

function KeyBar({
    ctrlArmed,
    altArmed,
    onPress,
    onToggleCtrl,
    onToggleAlt,
}: {
    ctrlArmed: boolean;
    altArmed: boolean;
    onPress: (bytes: Uint8Array) => void;
    onToggleCtrl: () => void;
    onToggleAlt: () => void;
}) {
    const bytes = (s: string) => new TextEncoder().encode(s);
    type Entry =
        | { id: string; label: React.ReactNode; send: Uint8Array; wide?: boolean }
        | {
              id: string;
              label: React.ReactNode;
              toggle: () => void;
              active: boolean;
              wide?: boolean;
          };
    const keys: Entry[] = [
        { id: "enter", label: <ReturnIcon />, send: bytes("\r") },
        { id: "esc", label: "Esc", send: bytes("\x1b"), wide: true },
        { id: "tab", label: "Tab", send: bytes("\t"), wide: true },
        {
            id: "ctrl",
            label: "Ctrl",
            toggle: onToggleCtrl,
            active: ctrlArmed,
            wide: true,
        },
        {
            id: "alt",
            label: "Alt",
            toggle: onToggleAlt,
            active: altArmed,
            wide: true,
        },
        { id: "up", label: <ChevronUp />, send: bytes("\x1b[A") },
        { id: "down", label: <ChevronDown />, send: bytes("\x1b[B") },
        { id: "left", label: <ChevronLeft />, send: bytes("\x1b[D") },
        { id: "right", label: <ChevronRight />, send: bytes("\x1b[C") },
    ];
    return (
        <div
            className="glass-chrome relative z-10"
            style={{
                paddingTop: "8px",
                paddingBottom: "calc(env(safe-area-inset-bottom) + 8px)",
            }}
        >
            <div className="no-scrollbar flex items-center gap-1.5 overflow-x-auto px-3">
                {keys.map((k) => {
                    const isToggle = "toggle" in k;
                    const active = isToggle && k.active;
                    return (
                        <button
                            key={k.id}
                            onClick={() => {
                                if (isToggle) {
                                    k.toggle();
                                } else if ("send" in k) {
                                    onPress(k.send);
                                }
                            }}
                            className={[
                                "flex h-10 shrink-0 items-center justify-center rounded-[14px] px-3 text-[13.5px] font-medium",
                                k.wide ? "min-w-[54px]" : "min-w-[44px]",
                                active ? "glass-pill glass-pill--accent text-white" : "glass-pill text-app-text",
                            ].join(" ")}
                            aria-label={k.id}
                        >
                            {k.label}
                        </button>
                    );
                })}
            </div>
        </div>
    );
}

function ReturnIcon() {
    return (
        <svg
            width="16"
            height="16"
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            strokeWidth={2}
            strokeLinecap="round"
            strokeLinejoin="round"
        >
            <polyline points="9 10 4 15 9 20" />
            <path d="M20 4v7a4 4 0 0 1-4 4H4" />
        </svg>
    );
}

function ChevronUp() {
    return (
        <svg
            width="14"
            height="14"
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            strokeWidth={2}
            strokeLinecap="round"
            strokeLinejoin="round"
        >
            <polyline points="6 15 12 9 18 15" />
        </svg>
    );
}
function ChevronDown() {
    return (
        <svg
            width="14"
            height="14"
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            strokeWidth={2}
            strokeLinecap="round"
            strokeLinejoin="round"
        >
            <polyline points="6 9 12 15 18 9" />
        </svg>
    );
}
function ChevronLeft() {
    return (
        <svg
            width="14"
            height="14"
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            strokeWidth={2}
            strokeLinecap="round"
            strokeLinejoin="round"
        >
            <polyline points="15 18 9 12 15 6" />
        </svg>
    );
}
function ChevronRight() {
    return (
        <svg
            width="14"
            height="14"
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            strokeWidth={2}
            strokeLinecap="round"
            strokeLinejoin="round"
        >
            <polyline points="9 18 15 12 9 6" />
        </svg>
    );
}

function CloseTabSheet({
    tab,
    busy,
    onCancel,
    onConfirm,
}: {
    tab: TabInfo | null;
    busy: boolean;
    onCancel: () => void;
    onConfirm: (mode: "checkpoint" | "force") => void;
}) {
    // Only agent tabs with a live sandbox get the three-way prompt;
    // shells / stopped agents go through `requestClose` → force and
    // never reach this sheet.
    return (
        <Sheet
            open={tab != null}
            onClose={busy ? () => {} : onCancel}
            title={tab ? `Close ${tab.label}?` : undefined}
        >
            <div className="flex flex-col gap-2 px-1 pt-2">
                <p className="px-2 text-[13px] text-app-text-muted">
                    Checkpoint saves the sandbox state so you can resume this
                    agent later. Close ends the session and throws away
                    in-progress work.
                </p>
                <button
                    disabled={busy}
                    onClick={() => onConfirm("checkpoint")}
                    className="glass-pill mt-1 flex h-11 items-center justify-center rounded-[14px] px-4 text-[14px] font-medium text-app-text disabled:opacity-40"
                >
                    {busy ? "Working…" : "Checkpoint & close"}
                </button>
                <button
                    disabled={busy}
                    onClick={() => onConfirm("force")}
                    className="flex h-11 items-center justify-center rounded-[14px] bg-red-500/15 px-4 text-[14px] font-medium text-app-error disabled:opacity-40 active:bg-red-500/25"
                >
                    Close without saving
                </button>
                <button
                    disabled={busy}
                    onClick={onCancel}
                    className="flex h-11 items-center justify-center rounded-[14px] px-4 text-[14px] text-app-text-muted active:bg-white/5 disabled:opacity-40"
                >
                    Cancel
                </button>
            </div>
        </Sheet>
    );
}

function NewTabSheet({
    open,
    agents,
    runningAgentTabs,
    allowHostShell,
    onClose,
    onPick,
}: {
    open: boolean;
    agents: AgentInfo[];
    runningAgentTabs: TabInfo[];
    allowHostShell: boolean;
    onClose: () => void;
    onPick: (spec: TabCreateSpec) => void;
}) {
    return (
        <Sheet open={open} onClose={onClose} title="New tab">
            <ul className="flex flex-col pt-1">
                {runningAgentTabs.map((t) => (
                    <li key={`gshell-${t.tab_id}`}>
                        <MenuRow
                            icon={<ShellIcon />}
                            label={`Guest Shell (${t.label})`}
                            onClick={() =>
                                onPick({
                                    kind: "guest_shell",
                                    parent_tab_id: t.tab_id,
                                })
                            }
                        />
                    </li>
                ))}
                {allowHostShell ? (
                    <li>
                        <MenuRow
                            icon={<ShellIcon />}
                            label="Host Shell"
                            onClick={() => onPick({ kind: "host_shell" })}
                        />
                    </li>
                ) : null}
                {agents.map((a) => (
                    <li key={`agent-${a.id}`}>
                        <MenuRow
                            icon={<AgentIcon agent={a} />}
                            label={a.display_name}
                            color={a.color ?? undefined}
                            onClick={() =>
                                onPick({ kind: "agent", agent_id: a.id })
                            }
                        />
                    </li>
                ))}
            </ul>
        </Sheet>
    );
}

function MenuRow({
    icon,
    label,
    color,
    onClick,
}: {
    icon: React.ReactNode;
    label: string;
    color?: string;
    onClick: () => void;
}) {
    return (
        <button
            onClick={onClick}
            className="flex w-full items-center gap-3 rounded-[14px] px-3 py-3 text-left text-[15px] text-app-text active:bg-white/10"
        >
            <span
                className="flex h-5 w-5 shrink-0 items-center justify-center"
                style={color ? { color } : undefined}
            >
                {icon}
            </span>
            <span className="truncate">{label}</span>
        </button>
    );
}

function AgentStateDot({ state }: { state: TabInfo["agent_state"] }) {
    // Matches the desktop status colors in `assets/themes/superhq-dark.json`:
    // agent_running → blue, agent_needs_input → amber. Idle / unknown
    // render nothing so shells and quiet agents don't get decorated.
    switch (state.state) {
        case "running":
            return (
                <span
                    className="inline-block h-1.5 w-1.5 shrink-0 animate-pulse rounded-full bg-[#3B82F6]"
                    title={state.tool ?? "Running"}
                    aria-label={state.tool ? `Running: ${state.tool}` : "Running"}
                />
            );
        case "needs_input":
            return (
                <span
                    className="inline-block h-1.5 w-1.5 shrink-0 animate-pulse rounded-full bg-[#F59E0B]"
                    title={state.message ?? "Needs input"}
                    aria-label="Needs input"
                />
            );
        case "idle":
        case "unknown":
        default:
            return null;
    }
}

function TabGlyph({
    tab,
    agents,
    size,
}: {
    tab: TabInfo;
    agents: AgentInfo[];
    size: number;
}) {
    if (tab.kind === "agent") {
        // Best-effort match by label (proto doesn't carry agent_id on
        // the tab). Falls back to a generic agent glyph.
        const agent = agents.find((a) => a.display_name === tab.label);
        if (agent) {
            return (
                <span
                    className="inline-flex shrink-0 items-center justify-center"
                    style={{
                        width: size,
                        height: size,
                        color: agent.color ?? "currentColor",
                    }}
                >
                    <AgentIcon agent={agent} />
                </span>
            );
        }
    }
    return <ShellIcon size={size} />;
}

function ShellIcon({ size = 14 }: { size?: number }) {
    return (
        <svg
            width={size}
            height={size}
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            strokeWidth={2}
            strokeLinecap="round"
            strokeLinejoin="round"
        >
            <polyline points="4 17 10 11 4 5" />
            <line x1="12" y1="19" x2="20" y2="19" />
        </svg>
    );
}

/// Render host-supplied `icon_svg` directly. Pairing established a
/// crypto-verified trust relationship with the host; its agent
/// metadata is treated the same as desktop-side agent config. A
/// compromised host has much bigger leverage than injecting an icon,
/// so separate sanitization here is overhead without changing the
/// threat model.
function AgentIcon({ agent }: { agent: AgentInfo }) {
    if (agent.icon_svg) {
        return (
            <span
                className="inline-flex h-full w-full items-center justify-center [&>svg]:h-full [&>svg]:w-full"
                // eslint-disable-next-line react/no-danger
                dangerouslySetInnerHTML={{ __html: agent.icon_svg }}
            />
        );
    }
    return <DefaultAgentIcon />;
}

function DefaultAgentIcon() {
    return (
        <svg
            width="14"
            height="14"
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            strokeWidth={2}
            strokeLinecap="round"
            strokeLinejoin="round"
        >
            <circle cx="12" cy="12" r="3" />
            <path d="M12 2v3M12 19v3M4.22 4.22l2.12 2.12M17.66 17.66l2.12 2.12M2 12h3M19 12h3M4.22 19.78l2.12-2.12M17.66 6.34l2.12-2.12" />
        </svg>
    );
}
