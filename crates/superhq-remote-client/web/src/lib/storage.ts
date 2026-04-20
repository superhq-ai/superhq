// Credential storage — WebAuthn PRF only.
//
// The device_key issued during pairing is the HMAC secret the client
// sends on every session.hello. We never let it touch plain storage:
// the browser wraps it with an AES-GCM key derived from a WebAuthn PRF
// output, so decryption requires a user-verification gesture bound to
// a platform authenticator (Touch ID, Windows Hello, or a FIDO2 key
// with hmac-secret).
//
// No fallback. If the browser can't do WebAuthn-PRF, pairing fails
// with a clear error.

const DB_NAME = "superhq-remote";
const DB_VERSION = 1;
const STORE = "credentials";

export interface StoredCredential {
    device_id: string;
    device_key: string;
}

interface DbRecord {
    peerId: string;
    credentialId: string;
    device_id: string;
    ciphertext: string;
    nonce: string;
}

// TS 5.7+ distinguishes Uint8Array<ArrayBuffer> from
// Uint8Array<ArrayBufferLike> and WebAuthn / WebCrypto DOM types
// require the former. These tiny helpers return the narrow type so
// the rest of the module can just assign into BufferSource slots
// without casting each site.
function rand(n: number): Uint8Array<ArrayBuffer> {
    const buf = new Uint8Array(new ArrayBuffer(n));
    crypto.getRandomValues(buf);
    return buf;
}

function fromString(s: string): Uint8Array<ArrayBuffer> {
    const encoded = new TextEncoder().encode(s);
    const buf = new Uint8Array(new ArrayBuffer(encoded.byteLength));
    buf.set(encoded);
    return buf;
}

// Fixed per-peer input to the PRF extension. Changing this string
// invalidates every stored credential's encryption key — do not modify
// without a migration path.
function prfSalt(peerId: string): Uint8Array<ArrayBuffer> {
    return fromString(`superhq.prf.v1.${peerId}`);
}

function rpId(): string {
    // Either the exact hostname or a registrable suffix of it.
    // "localhost" is specifically allowed by WebAuthn for dev.
    return window.location.hostname;
}

function b64encode(bytes: Uint8Array | ArrayBuffer): string {
    const arr = bytes instanceof Uint8Array ? bytes : new Uint8Array(bytes);
    let s = "";
    for (const b of arr) s += String.fromCharCode(b);
    return btoa(s);
}

function b64decode(str: string): Uint8Array<ArrayBuffer> {
    const s = atob(str);
    const out = new Uint8Array(new ArrayBuffer(s.length));
    for (let i = 0; i < s.length; i++) out[i] = s.charCodeAt(i);
    return out;
}

// ── IndexedDB wrapper ────────────────────────────────────────────────

function openDb(): Promise<IDBDatabase> {
    return new Promise((resolve, reject) => {
        const req = indexedDB.open(DB_NAME, DB_VERSION);
        req.onupgradeneeded = () => {
            const db = req.result;
            if (!db.objectStoreNames.contains(STORE)) {
                db.createObjectStore(STORE, { keyPath: "peerId" });
            }
        };
        req.onsuccess = () => resolve(req.result);
        req.onerror = () => reject(req.error);
    });
}

async function dbGet(peerId: string): Promise<DbRecord | null> {
    const db = await openDb();
    return new Promise((resolve, reject) => {
        const tx = db.transaction(STORE, "readonly");
        const req = tx.objectStore(STORE).get(peerId);
        req.onsuccess = () => resolve((req.result as DbRecord) ?? null);
        req.onerror = () => reject(req.error);
    });
}

async function dbPut(record: DbRecord): Promise<void> {
    const db = await openDb();
    return new Promise((resolve, reject) => {
        const tx = db.transaction(STORE, "readwrite");
        tx.objectStore(STORE).put(record);
        tx.oncomplete = () => resolve();
        tx.onerror = () => reject(tx.error);
    });
}

async function dbDelete(peerId: string): Promise<void> {
    const db = await openDb();
    return new Promise((resolve, reject) => {
        const tx = db.transaction(STORE, "readwrite");
        tx.objectStore(STORE).delete(peerId);
        tx.oncomplete = () => resolve();
        tx.onerror = () => reject(tx.error);
    });
}

// ── WebAuthn + PRF ───────────────────────────────────────────────────

function requireWebAuthn(): void {
    if (!window.isSecureContext) {
        throw new Error(
            "WebAuthn requires a secure context (HTTPS or localhost).",
        );
    }
    if (!window.PublicKeyCredential) {
        throw new Error("This browser does not expose WebAuthn.");
    }
}

async function createWebAuthnCredential(
    peerId: string,
): Promise<{
    credentialId: Uint8Array<ArrayBuffer>;
    prfKey: Uint8Array<ArrayBuffer>;
}> {
    const publicKey: PublicKeyCredentialCreationOptions = {
        challenge: rand(32),
        rp: { name: "SuperHQ Remote", id: rpId() },
        user: {
            id: rand(16),
            name: `superhq:${peerId.slice(0, 16)}`,
            displayName: `SuperHQ host ${peerId.slice(0, 8)}…`,
        },
        pubKeyCredParams: [
            { type: "public-key", alg: -7 },
            { type: "public-key", alg: -257 },
        ],
        authenticatorSelection: {
            residentKey: "preferred",
            userVerification: "required",
        },
        extensions: {
            prf: { eval: { first: prfSalt(peerId) } },
        },
        timeout: 60_000,
    };
    const cred = (await navigator.credentials.create({
        publicKey,
    })) as PublicKeyCredential | null;
    if (!cred) throw new Error("WebAuthn create returned null");
    const ext = cred.getClientExtensionResults() as {
        prf?: { enabled?: boolean; results?: { first?: ArrayBuffer } };
    };
    if (!ext?.prf?.enabled) {
        console.warn("WebAuthn ext results:", ext);
        throw new Error(
            "PRF not enabled by this authenticator. Use a passkey-capable " +
                "platform (Touch ID / Windows Hello / recent Google Password " +
                "Manager) or a FIDO2 key with hmac-secret.",
        );
    }

    // If the authenticator returned PRF output at create time we use it
    // directly — one prompt, not two. GPM and Chrome on macOS 132+ do.
    const firstFromCreate = ext.prf.results?.first;
    const prfOut: Uint8Array<ArrayBuffer> = firstFromCreate
        ? new Uint8Array(firstFromCreate.slice(0))
        : await getWebAuthnPrfKey(peerId, cred.rawId);
    const credentialId = new Uint8Array((cred.rawId as ArrayBuffer).slice(0));
    return { credentialId, prfKey: prfOut };
}

async function getWebAuthnPrfKey(
    peerId: string,
    credentialIdBytes: BufferSource,
): Promise<Uint8Array<ArrayBuffer>> {
    const publicKey: PublicKeyCredentialRequestOptions = {
        challenge: rand(32),
        rpId: rpId(),
        allowCredentials: [
            {
                type: "public-key",
                id: credentialIdBytes,
                transports: ["internal", "hybrid", "usb", "nfc", "ble"],
            },
        ],
        userVerification: "required",
        extensions: {
            prf: { eval: { first: prfSalt(peerId) } },
        },
        timeout: 60_000,
    };
    const assertion = (await navigator.credentials.get({
        publicKey,
    })) as PublicKeyCredential | null;
    if (!assertion) throw new Error("WebAuthn get returned null");
    const ext = assertion.getClientExtensionResults() as {
        prf?: { results?: { first?: ArrayBuffer } };
    };
    const first = ext?.prf?.results?.first;
    if (!first) throw new Error("authenticator did not return PRF output");
    return new Uint8Array(first.slice(0));
}

async function deriveAesKey(
    prfBytes: Uint8Array<ArrayBuffer>,
): Promise<CryptoKey> {
    return crypto.subtle.importKey(
        "raw",
        prfBytes,
        { name: "AES-GCM" },
        false,
        ["encrypt", "decrypt"],
    );
}

async function encryptDeviceKey(
    prfBytes: Uint8Array<ArrayBuffer>,
    deviceKeyB64: string,
): Promise<{
    ciphertext: Uint8Array<ArrayBuffer>;
    nonce: Uint8Array<ArrayBuffer>;
}> {
    const key = await deriveAesKey(prfBytes);
    const nonce = rand(12);
    const plain = fromString(deviceKeyB64);
    const buf = await crypto.subtle.encrypt(
        { name: "AES-GCM", iv: nonce },
        key,
        plain,
    );
    return { ciphertext: new Uint8Array(buf), nonce };
}

async function decryptDeviceKey(
    prfBytes: Uint8Array<ArrayBuffer>,
    ciphertext: Uint8Array<ArrayBuffer>,
    nonce: Uint8Array<ArrayBuffer>,
): Promise<string> {
    const key = await deriveAesKey(prfBytes);
    const buf = await crypto.subtle.decrypt(
        { name: "AES-GCM", iv: nonce },
        key,
        ciphertext,
    );
    return new TextDecoder().decode(new Uint8Array(buf));
}

// ── Public API ───────────────────────────────────────────────────────

// Session-scoped plaintext cache keyed by peerId. Populated after the
// first successful unlock (or right after pair), consulted by
// loadCredential so subsequent RPCs in the same tab don't each trigger
// a WebAuthn prompt. Evicted on clear, naturally gone on tab close.
const sessionCache = new Map<string, StoredCredential>();

export async function describeCredential(
    peerId: string,
): Promise<{ device_id: string } | null> {
    if (!peerId) return null;
    const row = await dbGet(peerId);
    return row ? { device_id: row.device_id } : null;
}

export async function saveCredential(
    peerId: string,
    cred: StoredCredential,
): Promise<void> {
    requireWebAuthn();
    const { credentialId, prfKey } = await createWebAuthnCredential(peerId);
    const { ciphertext, nonce } = await encryptDeviceKey(
        prfKey,
        cred.device_key,
    );
    await dbPut({
        peerId,
        credentialId: b64encode(credentialId),
        device_id: cred.device_id,
        ciphertext: b64encode(ciphertext),
        nonce: b64encode(nonce),
    });
    // We just registered; stash the plaintext so the post-pair
    // session.hello doesn't immediately re-prompt for unlock.
    sessionCache.set(peerId, { ...cred });
}

export async function loadCredential(
    peerId: string,
): Promise<StoredCredential | null> {
    const cached = sessionCache.get(peerId);
    if (cached) return { ...cached };
    const row = await dbGet(peerId);
    if (!row) return null;
    requireWebAuthn();
    const prfKey = await getWebAuthnPrfKey(peerId, b64decode(row.credentialId));
    const device_key = await decryptDeviceKey(
        prfKey,
        b64decode(row.ciphertext),
        b64decode(row.nonce),
    );
    const unlocked: StoredCredential = {
        device_id: row.device_id,
        device_key,
    };
    sessionCache.set(peerId, unlocked);
    return { ...unlocked };
}

export async function clearCredential(peerId: string): Promise<void> {
    sessionCache.delete(peerId);
    await dbDelete(peerId);
}
