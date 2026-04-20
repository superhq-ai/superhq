import { useEffect } from "react";
import { Routes, Route, Navigate } from "react-router";
import { useConnectionStore } from "./state/store";
import { refreshSnapshot, watchNotifications } from "./lib/session";
import Toast from "./components/Toast";
import PairRoute from "./routes/pair";
import HomeRoute from "./routes/home";
import WorkspaceRoute from "./routes/workspace";
import {
    pruneTerminalEntries,
    resetTerminalRegistry,
} from "./components/Terminal";

function RequirePair({ children }: { children: React.ReactNode }) {
    const paired = useConnectionStore((s) => s.pairedHost);
    if (!paired) return <Navigate to="/pair" replace />;
    return <>{children}</>;
}

/// Global host → client notification listener. Lives for the full
/// authenticated session (not tied to any route), so `snapshot.invalidated`
/// pushes are never missed while the user is mid-terminal.
function NotificationSubscriber() {
    const client = useConnectionStore((s) => s.client);
    const replaceSnapshot = useConnectionStore((s) => s.replaceSnapshot);
    useEffect(() => {
        if (!client) return;
        const watcher = watchNotifications(client, () => {
            refreshSnapshot(client)
                .then((snap) => replaceSnapshot(snap.workspaces, snap.tabs))
                .catch(() => {
                    /* transient — the next push will retry */
                });
        });
        return () => watcher.close();
    }, [client, replaceSnapshot]);
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
            <NotificationSubscriber />
            <TerminalRegistryManager />
            <Toast />
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
