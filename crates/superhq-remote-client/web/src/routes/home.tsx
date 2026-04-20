// Home — a flat list of workspaces, mirroring the desktop sidebar.
//
// Tap a workspace → activate if stopped → navigate to
// `/workspace/:id`. No inline tab list, no "Start" chip, no "+ New
// tab" — the tab bar lives on the workspace view where it belongs.

import { useCallback, useEffect, useState } from "react";
import { useNavigate } from "react-router";
import Screen from "../components/Screen";
import SettingsSheet from "../components/SettingsSheet";
import { useConnectionStore } from "../state/store";
import {
    activateWorkspace,
    establishSession,
    refreshSnapshot,
} from "../lib/session";
import type { WorkspaceInfo } from "../lib/types";

export default function HomeRoute() {
    const navigate = useNavigate();
    const host = useConnectionStore((s) => s.pairedHost);
    const session = useConnectionStore((s) => s.session);
    const client = useConnectionStore((s) => s.client);
    const workspaces = useConnectionStore((s) => s.workspaces);
    const tabs = useConnectionStore((s) => s.tabs);
    const setSessionConnecting = useConnectionStore((s) => s.setSessionConnecting);
    const setSessionReady = useConnectionStore((s) => s.setSessionReady);
    const setSessionError = useConnectionStore((s) => s.setSessionError);
    const replaceSnapshot = useConnectionStore((s) => s.replaceSnapshot);

    const [busyWs, setBusyWs] = useState<number | null>(null);
    const [actionError, setActionError] = useState<string | null>(null);
    const [refreshing, setRefreshing] = useState(false);
    const [settingsOpen, setSettingsOpen] = useState(false);
    const showToast = useConnectionStore((s) => s.showToast);

    const connectNow = useCallback(async () => {
        if (!host) return;
        setSessionConnecting();
        try {
            const boot = await establishSession(host.peerId);
            setSessionReady(boot.client, boot.workspaces, boot.tabs, boot.agents);
        } catch (e) {
            setSessionError(e instanceof Error ? e.message : String(e));
        }
    }, [host, setSessionConnecting, setSessionReady, setSessionError]);

    useEffect(() => {
        if (session.kind === "idle") {
            void connectNow();
        }
    }, [session.kind, connectNow]);

    const refreshNow = useCallback(async () => {
        if (!client) {
            await connectNow();
            return;
        }
        if (refreshing) return;
        setRefreshing(true);
        try {
            const snap = await refreshSnapshot(client);
            replaceSnapshot(snap.workspaces, snap.tabs);
            const count = snap.workspaces.length;
            showToast(
                `Refreshed · ${count} workspace${count === 1 ? "" : "s"}`,
            );
        } catch (e) {
            const msg = e instanceof Error ? e.message : String(e);
            showToast(`Refresh failed: ${msg}`, "error");
        } finally {
            setRefreshing(false);
        }
    }, [client, connectNow, refreshing, replaceSnapshot, showToast]);

    const onOpenWorkspace = useCallback(
        async (ws: WorkspaceInfo) => {
            if (ws.is_active) {
                navigate(`/workspace/${ws.workspace_id}`);
                return;
            }
            if (!client) return;
            setBusyWs(ws.workspace_id);
            setActionError(null);
            try {
                await activateWorkspace(client, ws.workspace_id);
                const snap = await refreshSnapshot(client);
                replaceSnapshot(snap.workspaces, snap.tabs);
                navigate(`/workspace/${ws.workspace_id}`);
            } catch (e) {
                setActionError(
                    e instanceof Error ? e.message : "Failed to open workspace",
                );
            } finally {
                setBusyWs(null);
            }
        },
        [client, navigate, replaceSnapshot],
    );

    const tabCountByWs = new Map<number, number>();
    for (const t of tabs) {
        tabCountByWs.set(t.workspace_id, (tabCountByWs.get(t.workspace_id) ?? 0) + 1);
    }

    return (
        <Screen>
            <HomeHeader
                connecting={session.kind === "connecting"}
                refreshing={refreshing}
                onRefresh={refreshNow}
                onOpenSettings={() => setSettingsOpen(true)}
            />

            {session.kind === "connecting" ? (
                <LoadingView label="Connecting to host…" />
            ) : session.kind === "error" ? (
                <ErrorView message={session.message} onRetry={connectNow} />
            ) : workspaces.length === 0 ? (
                <EmptyView />
            ) : (
                <div className="flex flex-1 flex-col overflow-y-auto px-4 pb-8">
                    {actionError ? (
                        <div className="mt-3 rounded-2xl bg-red-500/10 px-3 py-2.5 text-[13px] text-app-error">
                            {actionError}
                        </div>
                    ) : null}
                    <div className="mt-3 text-[11px] font-semibold uppercase tracking-wider text-app-text-muted">
                        Workspaces
                    </div>
                    <ul className="glass-card mt-2 flex flex-col overflow-hidden rounded-[20px]">
                        {workspaces.map((ws, i) => (
                            <li key={ws.workspace_id}>
                                {i > 0 ? (
                                    <div className="ml-[60px] h-px bg-white/6" />
                                ) : null}
                                <WorkspaceRow
                                    ws={ws}
                                    tabCount={tabCountByWs.get(ws.workspace_id) ?? 0}
                                    busy={busyWs === ws.workspace_id}
                                    onOpen={() => onOpenWorkspace(ws)}
                                />
                            </li>
                        ))}
                    </ul>
                </div>
            )}
            <SettingsSheet
                open={settingsOpen}
                onClose={() => setSettingsOpen(false)}
            />
        </Screen>
    );
}

function WorkspaceRow({
    ws,
    tabCount,
    busy,
    onOpen,
}: {
    ws: WorkspaceInfo;
    tabCount: number;
    busy: boolean;
    onOpen: () => void;
}) {
    return (
        <button
            onClick={onOpen}
            disabled={busy}
            className="flex w-full items-center gap-3 px-4 py-3.5 text-left transition-colors active:bg-white/5 disabled:opacity-60"
        >
            {ws.github_owner ? (
                <img
                    src={`https://github.com/${ws.github_owner}.png?size=56`}
                    alt=""
                    width={28}
                    height={28}
                    className="h-7 w-7 shrink-0 rounded-full bg-white/10 object-cover"
                    aria-hidden
                />
            ) : (
                <div
                    className="flex h-7 w-7 shrink-0 items-center justify-center rounded-full bg-white/8 text-[10px] font-semibold tracking-tight text-app-text-muted"
                    aria-hidden
                >
                    {ws.label.slice(0, 2).toUpperCase()}
                </div>
            )}
            <div className="flex min-w-0 flex-1 flex-col gap-0.5">
                <div className="flex min-w-0 items-center gap-1.5">
                    <span className="truncate text-[15px] font-medium text-app-text">
                        {ws.label}
                    </span>
                    <span
                        className={`h-1.5 w-1.5 shrink-0 rounded-full ${
                            ws.is_active ? "bg-emerald-400" : "bg-app-text-ghost"
                        }`}
                        aria-hidden
                    />
                </div>
                <WorkspaceMeta ws={ws} tabCount={tabCount} busy={busy} />
            </div>
        </button>
    );
}

function WorkspaceMeta({
    ws,
    tabCount,
    busy,
}: {
    ws: WorkspaceInfo;
    tabCount: number;
    busy: boolean;
}) {
    if (busy) {
        return (
            <div className="truncate text-[11px] text-app-text-muted">
                Starting…
            </div>
        );
    }
    if (ws.repo_name) {
        return (
            <div className="flex min-w-0 items-center gap-1.5 truncate text-[11px] text-app-text-muted">
                <span className="truncate">{ws.repo_name}</span>
                {ws.branch ? (
                    <>
                        <span aria-hidden>·</span>
                        <BranchIcon />
                        <span className="truncate">{ws.branch}</span>
                    </>
                ) : null}
                {ws.is_active && tabCount > 0 ? (
                    <>
                        <span aria-hidden>·</span>
                        <span className="truncate">
                            {tabCount} tab{tabCount === 1 ? "" : "s"}
                        </span>
                    </>
                ) : null}
            </div>
        );
    }
    return (
        <div className="truncate text-[11px] text-app-text-muted">
            {ws.is_active
                ? tabCount > 0
                    ? `scratch sandbox · ${tabCount} tab${tabCount === 1 ? "" : "s"}`
                    : "scratch sandbox"
                : "scratch sandbox · tap to start"}
        </div>
    );
}

function BranchIcon() {
    return (
        <svg
            width="11"
            height="11"
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            strokeWidth={2}
            strokeLinecap="round"
            strokeLinejoin="round"
            className="shrink-0"
            aria-hidden
        >
            <line x1="6" y1="3" x2="6" y2="15" />
            <circle cx="18" cy="6" r="3" />
            <circle cx="6" cy="18" r="3" />
            <path d="M18 9a9 9 0 0 1-9 9" />
        </svg>
    );
}

function LoadingView({ label }: { label: string }) {
    return (
        <div className="flex flex-1 items-center justify-center gap-3 text-sm text-app-text-muted">
            <span className="h-2 w-2 animate-pulse rounded-full bg-app-accent" />
            {label}
        </div>
    );
}

function ErrorView({ message, onRetry }: { message: string; onRetry: () => void }) {
    return (
        <div className="flex flex-1 items-center justify-center px-5 py-8">
            <div className="glass-card flex w-full max-w-[380px] flex-col items-center gap-4 rounded-[24px] p-6 text-center">
                <div className="glass-pill flex h-12 w-12 items-center justify-center rounded-full text-red-300">
                    <svg width="22" height="22" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth={2.2} strokeLinecap="round" strokeLinejoin="round">
                        <circle cx="12" cy="12" r="10" />
                        <line x1="12" y1="8" x2="12" y2="12" />
                        <line x1="12" y1="16" x2="12.01" y2="16" />
                    </svg>
                </div>
                <div className="flex flex-col gap-1.5">
                    <div className="text-[17px] font-semibold tracking-[-0.01em] text-app-text">
                        Couldn&rsquo;t reach the host
                    </div>
                    <p className="max-w-sm text-[13.5px] leading-relaxed break-words text-app-text-secondary">
                        {message}
                    </p>
                </div>
                <button
                    onClick={onRetry}
                    className="glass-pill glass-pill--accent mt-1 flex h-10 w-full items-center justify-center rounded-full text-[14px] font-medium text-white"
                >
                    Try again
                </button>
            </div>
        </div>
    );
}

function HomeHeader({
    connecting,
    refreshing,
    onRefresh,
    onOpenSettings,
}: {
    connecting: boolean;
    refreshing: boolean;
    onRefresh: () => void;
    onOpenSettings: () => void;
}) {
    return (
        <div
            className="flex items-center justify-end gap-1.5 px-3"
            style={{
                paddingTop: "calc(env(safe-area-inset-top) + 6px)",
                paddingBottom: "4px",
            }}
        >
            <button
                onClick={onRefresh}
                disabled={connecting || refreshing}
                className="glass-pill flex h-8 w-8 items-center justify-center rounded-full text-app-text disabled:opacity-50"
                aria-label="Refresh"
            >
                <svg
                    width="13"
                    height="13"
                    viewBox="0 0 24 24"
                    fill="none"
                    stroke="currentColor"
                    strokeWidth={2.2}
                    strokeLinecap="round"
                    strokeLinejoin="round"
                    className={refreshing ? "animate-spin" : ""}
                >
                    <polyline points="23 4 23 10 17 10" />
                    <polyline points="1 20 1 14 7 14" />
                    <path d="M3.51 9a9 9 0 0 1 14.85-3.36L23 10M1 14l4.64 4.36A9 9 0 0 0 20.49 15" />
                </svg>
            </button>
            <button
                onClick={onOpenSettings}
                className="glass-pill flex h-8 w-8 items-center justify-center rounded-full text-app-text"
                aria-label="Settings"
            >
                <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth={2.2} strokeLinecap="round" strokeLinejoin="round">
                    <circle cx="12" cy="12" r="3" />
                    <path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 0 1 0 2.83 2 2 0 0 1-2.83 0l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 1 1-4 0v-.09a1.65 1.65 0 0 0-1-1.51 1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 1 1-2.83-2.83l.06-.06a1.65 1.65 0 0 0 .33-1.82 1.65 1.65 0 0 0-1.51-1H3a2 2 0 0 1 0-4h.09a1.65 1.65 0 0 0 1.51-1 1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 1 1 2.83-2.83l.06.06a1.65 1.65 0 0 0 1.82.33h.01a1.65 1.65 0 0 0 1-1.51V3a2 2 0 1 1 4 0v.09a1.65 1.65 0 0 0 1 1.51h.01a1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 1 1 2.83 2.83l-.06.06a1.65 1.65 0 0 0-.33 1.82v.01a1.65 1.65 0 0 0 1.51 1H21a2 2 0 1 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z" />
                </svg>
            </button>
        </div>
    );
}

function EmptyView() {
    return (
        <div className="flex flex-1 flex-col items-center justify-center gap-2 px-6 text-center">
            <div className="text-sm font-medium text-app-text">
                No workspaces yet
            </div>
            <p className="max-w-sm text-sm text-app-text-muted">
                Create a workspace on your desktop, then pull to refresh here.
            </p>
        </div>
    );
}
