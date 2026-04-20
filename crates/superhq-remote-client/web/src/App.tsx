import { useCallback, useEffect } from "react";
import { Routes, Route, Navigate } from "react-router";
import { useConnectionStore } from "./state/store";
import {
    establishSession,
    refreshSnapshot,
    watchNotifications,
} from "./lib/session";
import Toast from "./components/Toast";
import PWAUpdatePrompt from "./components/PWAUpdatePrompt";
import PairRoute from "./routes/pair";
import HomeRoute from "./routes/home";
import WorkspaceRoute from "./routes/workspace";
import {
    pruneTerminalEntries,
    resetTerminalRegistry,
} from "./components/Terminal";
import { track } from "./lib/analytics";

function RequirePair({ children }: { children: React.ReactNode }) {
    const paired = useConnectionStore((s) => s.pairedHost);
    if (!paired) return <Navigate to="/pair" replace />;
    return <>{children}</>;
}

/// Top-level session gate. Owns the lifecycle of the `ClientHandle`:
/// auto-connects whenever we have a paired host but no live session,
/// and drops the session back to `idle` on disconnect so the next
/// render re-connects.
///
/// Previously this logic lived inside `HomeRoute`, which meant a cold
/// launch directly into `/workspace/:ws` (PWA restore, deep link) never
/// established a session and the workspace view rendered against empty
/// state until the user manually navigated back to `/`.
function PairedSessionGate() {
    const host = useConnectionStore((s) => s.pairedHost);
    const sessionKind = useConnectionStore((s) => s.session.kind);
    const setSessionConnecting = useConnectionStore(
        (s) => s.setSessionConnecting,
    );
    const setSessionReady = useConnectionStore((s) => s.setSessionReady);
    const setSessionError = useConnectionStore((s) => s.setSessionError);

    const connectNow = useCallback(async () => {
        if (!host) return;
        setSessionConnecting();
        try {
            const boot = await establishSession(host.peerId);
            setSessionReady(
                boot.client,
                boot.workspaces,
                boot.tabs,
                boot.agents,
            );
            track("session.ready");
        } catch (e) {
            setSessionError(e instanceof Error ? e.message : String(e));
            track("session.error");
        }
    }, [host, setSessionConnecting, setSessionReady, setSessionError]);

    useEffect(() => {
        if (host && sessionKind === "idle") {
            void connectNow();
        }
    }, [host, sessionKind, connectNow]);

    return null;
}

/// Global host → client notification listener. Lives for the full
/// authenticated session (not tied to any route), so `snapshot.invalidated`
/// pushes are never missed while the user is mid-terminal.
///
/// Surfaces disconnects: when the host closes the control stream or
/// errors out, the session drops to `idle`, the stale client is closed,
/// and `PairedSessionGate` picks up the reconnect on the next render.
function NotificationSubscriber() {
    const client = useConnectionStore((s) => s.client);
    const sessionKind = useConnectionStore((s) => s.session.kind);
    const replaceSnapshot = useConnectionStore((s) => s.replaceSnapshot);
    const clearSession = useConnectionStore((s) => s.clearSession);
    useEffect(() => {
        if (!client || sessionKind !== "ready") return;
        const watcher = watchNotifications(
            client,
            () => {
                refreshSnapshot(client)
                    .then((snap) => replaceSnapshot(snap.workspaces, snap.tabs))
                    .catch(() => {
                        /* transient — the next push will retry */
                    });
            },
            () => {
                // Host dropped the control stream. Tear down the dead
                // client and let PairedSessionGate reconnect.
                clearSession();
            },
        );
        return () => watcher.close();
    }, [client, sessionKind, replaceSnapshot, clearSession]);
    return null;
}

/// Keeps the module-level xterm registry in sync with the host snapshot.
/// Any tab that disappears from the snapshot gets its xterm + PTY stream
/// disposed. A paired-host swap (new `client`) wipes everything — stale
/// `PtyStreamHandle`s are bound to the old iroh session and can't recover.
function TerminalRegistryManager() {
    const tabs = useConnectionStore((s) => s.tabs);
    const client = useConnectionStore((s) => s.client);
    useEffect(() => {
        const live = new Set(tabs.map((t) => `${t.workspace_id}:${t.tab_id}`));
        pruneTerminalEntries(live);
    }, [tabs]);
    useEffect(() => {
        return () => {
            resetTerminalRegistry();
        };
    }, [client]);
    return null;
}

export default function App() {
    return (
        <>
            <PairedSessionGate />
            <NotificationSubscriber />
            <TerminalRegistryManager />
            <Toast />
            <PWAUpdatePrompt />
            <Routes>
                <Route path="/pair" element={<PairRoute />} />
                <Route
                    path="/"
                    element={
                        <RequirePair>
                            <HomeRoute />
                        </RequirePair>
                    }
                />
                <Route
                    path="/workspace/:ws"
                    element={
                        <RequirePair>
                            <WorkspaceRoute />
                        </RequirePair>
                    }
                />
            </Routes>
        </>
    );
}
