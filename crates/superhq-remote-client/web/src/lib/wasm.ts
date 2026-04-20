// Thin wrapper over the wasm-bindgen client so the rest of the app
// can `import { connect }` without knowing the init dance. We load
// the WASM module once lazily on first use; subsequent calls reuse it.
//
// The bundle is served from `/pkg/` by `viteStaticCopy` in vite.config.ts.
// We intentionally re-export the binding types we need at the top so
// callers don't reach across into `/pkg/` directly.

// Use dynamic import so Vite doesn't try to rewrite the WASM pkg at
// build time. The pkg/ tree is copied into dist/pkg/ verbatim by
// `viteStaticCopy`, matching the layout the vanilla demo expects.
type ClientHandle = import("../../../pkg/superhq_remote_client").ClientHandle;
type DeviceCredential = import("../../../pkg/superhq_remote_client").DeviceCredential;

let readyPromise: Promise<typeof import("../../../pkg/superhq_remote_client")> | null = null;

function ensureLoaded() {
    if (readyPromise) return readyPromise;
    const p = (async () => {
        const mod = await import(
            /* @vite-ignore */ `${import.meta.env.BASE_URL}pkg/superhq_remote_client.js`
        );
        await mod.default();
        return mod;
    })();
    readyPromise = p;
    return p;
}

export async function connect(peerId: string): Promise<ClientHandle> {
    const mod = await ensureLoaded();
    return mod.ClientHandle.connect(peerId);
}

export async function makeCredentialHandle(
    device_id: string,
    device_key: string,
): Promise<DeviceCredential> {
    const mod = await ensureLoaded();
    return new mod.DeviceCredential(device_id, device_key);
}

export type { ClientHandle, DeviceCredential };
