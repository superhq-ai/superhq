// App-wide state — kept tiny. Two layers:
//
// • Persisted: the currently paired host. Only the peerId + label
//   + pairedAt timestamp. Device key / credentials are elsewhere
//   (IndexedDB, WebAuthn-PRF-wrapped).
// • Ephemeral: the live ClientHandle + latest workspaces/tabs snapshot.
//   Reset on reload; HomeRoute re-establishes on mount.

import { create } from "zustand";
import { persist } from "zustand/middleware";
import type { AgentInfo, TabInfo, WorkspaceInfo } from "../lib/types";
import type { ClientHandle } from "../lib/wasm";

export interface PairedHost {
    peerId: string;
    label: string;
    pairedAt: number;
}

export type SessionStatus =
    | { kind: "idle" }
    | { kind: "connecting" }
    | { kind: "ready" }
    | { kind: "error"; message: string };

interface PersistedState {
    pairedHost: PairedHost | null;
}

export interface ToastMessage {
    id: number;
    message: string;
    tone: "info" | "error";
}

interface EphemeralState {
    client: ClientHandle | null;
    workspaces: WorkspaceInfo[];
    tabs: TabInfo[];
    agents: AgentInfo[];
    session: SessionStatus;
    toast: ToastMessage | null;
}

interface Actions {
    setPairedHost: (host: PairedHost | null) => void;
    forgetHost: () => void;
    setSessionConnecting: () => void;
    setSessionReady: (
        client: ClientHandle,
        workspaces: WorkspaceInfo[],
        tabs: TabInfo[],
        agents: AgentInfo[],
    ) => void;
    setSessionError: (message: string) => void;
    replaceSnapshot: (workspaces: WorkspaceInfo[], tabs: TabInfo[]) => void;
    clearSession: () => void;
    showToast: (message: string, tone?: "info" | "error") => void;
    clearToast: () => void;
}

let nextToastId = 1;

type Store = PersistedState & EphemeralState & Actions;

export const useConnectionStore = create<Store>()(
    persist(
        (set, get) => ({
            pairedHost: null,
            client: null,
            workspaces: [],
            tabs: [],
            agents: [],
            session: { kind: "idle" },
            toast: null,

            setPairedHost: (host) => set({ pairedHost: host }),

            forgetHost: () => {
                const { client } = get();
                if (client) {
                    try {
                        client.close();
                    } catch {
                        /* no-op */
                    }
                }
                set({
                    pairedHost: null,
                    client: null,
                    workspaces: [],
                    tabs: [],
                    agents: [],
                    session: { kind: "idle" },
                });
            },

            setSessionConnecting: () =>
                set({ session: { kind: "connecting" } }),

            setSessionReady: (client, workspaces, tabs, agents) =>
                set({
                    client,
                    workspaces,
                    tabs,
                    agents,
                    session: { kind: "ready" },
                }),

            setSessionError: (message) =>
                set({ session: { kind: "error", message } }),

            replaceSnapshot: (workspaces, tabs) =>
                set({ workspaces, tabs }),

            clearSession: () => {
                const { client } = get();
                if (client) {
                    try {
                        client.close();
                    } catch {
                        /* no-op */
                    }
                }
                set({
                    client: null,
                    workspaces: [],
                    tabs: [],
                    agents: [],
                    session: { kind: "idle" },
                });
            },

            showToast: (message, tone = "info") =>
                set({ toast: { id: nextToastId++, message, tone } }),
            clearToast: () => set({ toast: null }),
        }),
        {
            name: "superhq-remote.v1",
            partialize: (state) => ({ pairedHost: state.pairedHost }),
        },
    ),
);
