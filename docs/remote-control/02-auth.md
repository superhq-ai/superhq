# Track 2 — Auth & Pairing

Status: `draft`

## Context

Track 1 gives us a working pipe between browser and desktop via iroh. Anyone
who learns a host's NodeId can attempt to connect. That's fine for a spike
but not for shipping — we need to ensure only devices the user has
explicitly blessed get access. This track defines how pairing works, how
credentials are stored, and how each connection proves it belongs.

## Goals

- **Explicit pairing.** First-time access requires an affirmative action on
  the host (approve-on-desktop) or a time-bounded out-of-band code (TOTP).
- **Durable credentials.** After pairing, the device reconnects without
  re-prompting.
- **Revocable.** User can kick any paired device at any time. Revocation
  propagates on the next connection attempt.
- **Per-workspace scoping, ready** (not enforced in V1). The data model
  supports "this device may access workspaces X, Y, Z" so Track 2 can be
  extended without a wire change.
- **Storage at rest protected.** On the host: OS keychain. On the client
  (browser): WebAuthn-wrapped key preferred, passphrase-wrapped key as
  fallback.

## Non-goals

- User accounts, SSO, identity providers. This is "your devices, your host."
- Multi-user shared-host scenarios. One operating-system user = one SuperHQ
  identity.
- Cross-host pairing federation. A pairing is specific to one host.

## Threat model

**In scope:**
- Attacker who learns the host's NodeId and tries to connect.
- Attacker who steals a paired device's ephemeral iroh key (browser refresh
  can rotate NodeId, so this shouldn't be load-bearing for identity).
- Replay of old handshakes.
- Stolen pairing credential on a client (laptop/phone).

**Out of scope:**
- Host compromise (attacker has root on the desktop). If your desktop is
  popped, SuperHQ is the least of your problems.
- Side channels against iroh / QUIC / WASM runtimes.
- Coerced user approval (someone watching the screen tricks you into
  clicking Approve).

## Identity model

The iroh NodeId is a **transport identity**, not an **application identity**.
Browser clients regenerate their ed25519 key on every session (when calling
`Endpoint::bind()` without a stored secret), so NodeId is not stable enough
to be used for pairing identity.

Instead, pairing mints a **device credential** independent of NodeId:

```rust
struct DeviceCredential {
    device_id: String,      // opaque, host-assigned (ULID)
    device_key: [u8; 32],   // random, known only to host + paired client
}
```

On every connection, the client proves possession of `device_key` via an
HMAC-based proof. The NodeId is still cryptographically verified by iroh
(giving us E2E encryption) but plays no role in authZ.

**Benefit**: browser clients don't need to persist iroh keys; they can
generate fresh NodeIds per session. What they persist is the device
credential.

## Pairing flow

### Approve-on-host (default)

```
Client                                  Host
  │                                       │
  │─── iroh connect (no auth yet) ────────▶
  │─── open control stream ────────────────▶
  │─── pairing.request {                  │
  │      device_label: "iPhone 15"        │
  │    } ─────────────────────────────────▶
  │                                       │ (popover on desktop)
  │                                       │ User clicks Approve
  │                                       │
  │◀─── pairing.approved {                │
  │       device_id, device_key,          │
  │       server_proof                    │
  │     } ────────────────────────────────│
  │                                       │
  │ (client persists credential)          │
  │                                       │
  │─── session.hello { auth, ... } ───────▶
  │◀─── hello_ack ─────────────────────────
```

Timeout: if the user doesn't approve within 60s, host responds
`pairing.rejected { reason: "timeout" }` and closes the stream.

### TOTP fallback

For pairing when the user can't click Approve on the desktop (e.g. away
from the computer, troubleshooting someone else's setup):

1. User has previously enrolled TOTP on the host (see Enrollment below).
2. Client sends `pairing.request { device_label, totp_code: "123456" }`.
3. Host validates the TOTP code (standard 30s window, 1 step of drift).
4. On success, proceeds as if approved-on-host.

TOTP is **one-factor** for pairing; the resulting `device_key` is still the
long-lived credential. TOTP is a mechanism to authorize issuing it.

### Subsequent connections

```
Client                                  Host
  │                                       │
  │─── iroh connect (fresh NodeId ok) ────▶
  │─── open control stream ────────────────▶
  │─── session.hello {                    │
  │      protocol_version,                │
  │      device_label,                    │
  │      auth: {                          │
  │        device_id,                     │
  │        timestamp: 1700000000,         │
  │        proof: HMAC(device_key,        │
  │           "superhq:v1:" ||            │
  │           host_node_id ||             │
  │           device_id ||                │
  │           ":" || timestamp)           │
  │      }                                │
  │    } ─────────────────────────────────▶
  │                                       │ Verify HMAC, ts within 5 min
  │◀─── session_hello result ──────────────
```

If verification fails, host responds with JSON-RPC error
`code: -32601 + {auth reason}` and closes the connection.

The 5-minute timestamp window is a belt-and-suspenders against replay;
iroh's QUIC handshake already protects against same-session replay.

## New protocol methods

Added to `superhq-remote-proto`:

### `pairing.request`
```rust
struct PairingRequestParams {
    device_label: String,        // user-visible name ("iPhone 15")
    totp_code: Option<String>,   // 6-digit code if using TOTP fallback
}

struct PairingRequestResult {
    // On success:
    device_id: String,
    device_key: String,           // base64-encoded 32 random bytes
    server_proof: String,          // base64 HMAC proving server knows the key
}
```

Errors: `pairing.rejected` (user denied), `pairing.timeout`,
`pairing.totp_invalid`.

### `session.hello` (extended)
```rust
struct SessionHelloParams {
    protocol_version: u32,
    device_label: String,
    resume_token: Option<String>,
    auth: SessionAuth,           // NEW: required in V1
}

struct SessionAuth {
    device_id: String,
    timestamp: u64,
    proof: String,               // base64 HMAC-SHA256
}
```

Error on auth fail: `code: 1004 "auth_required"` or `1005 "auth_invalid"`
depending on whether credentials were provided. Connection closed after.

### `pairing.list` (on-host admin surface)
```rust
struct PairingListResult(Vec<PairedDevice>);

struct PairedDevice {
    device_id: String,
    device_label: String,
    created_at: u64,
    last_seen_at: Option<u64>,
    allowed_workspaces: Option<Vec<WorkspaceId>>,  // None = all
}
```

Not callable over the remote protocol (attackers-from-paired-device would be
a concern); exposed via the desktop app's own UI.

### `pairing.revoke`
Same — local-only admin action. The desktop UI calls into local storage to
remove an entry; any in-flight connection from that device is torn down.

## TOTP enrollment

First-time TOTP setup is a desktop-UI action:

1. User opens Settings → Remote Control → Enable TOTP
2. Desktop generates a random 160-bit secret
3. Stores in OS keychain under `superhq.remote.totp`
4. Shows QR code encoding `otpauth://totp/SuperHQ:...` for any authenticator app
5. User scans, confirms a generated code to prove enrollment worked
6. TOTP is now active for pairing fallback

Only one TOTP secret per host. Regenerating invalidates the old one.

## Storage

### Host

- **SQLite** (existing SuperHQ DB):
  - `paired_devices` table: `device_id`, `device_label`, `created_at`,
    `last_seen_at`, `allowed_workspaces_json`
  - Does **not** store `device_key` — that goes in the keychain (next bullet).
- **OS Keychain**:
  - Per-device entry: key = `superhq.remote.device.<device_id>`, value = raw
    32-byte key.
  - TOTP secret: key = `superhq.remote.totp`, value = 20 raw bytes.
  - Crate: `keyring` (well-maintained, cross-platform — macOS Keychain,
    Windows Credential Manager, Linux Secret Service).

Why split keys to keychain? Two reasons: (a) defense in depth against DB
dumps, (b) OS-level user permission prompts on access in case malware tries
to read the DB.

### Client (browser PWA)

The credential is `{ host_node_id, device_id, device_key }`.

- **WebAuthn-wrapped (preferred)**:
  - First run: user creates a passkey ("Set up device").
  - Credential is encrypted with a random data key; the data key is wrapped
    by a key derived from WebAuthn credential assertion.
  - Stored in IndexedDB. Reads require a WebAuthn gesture (Touch ID /
    Windows Hello / hardware key).
- **Passphrase-wrapped (fallback)**:
  - If WebAuthn isn't available or user declines.
  - User provides a passphrase; we PBKDF2 it to derive a key-encryption key.
  - Same storage shape in IndexedDB.

WebAuthn is preferred because typing passphrases is the worst part of
every mobile app ever built.

## Session binding

- `device_id` is bound to the pairing record, not a NodeId.
- A client may use the same credential from multiple NodeIds (e.g. two
  browser tabs, session rotations). From the host's perspective this looks
  like two concurrent sessions by the same device.
- Per-session state (subscriptions, attached PTYs) is per-connection. Two
  concurrent sessions from the same device are independent.

## Revocation

- Desktop UI has a "Paired Devices" list showing `device_label`,
  `last_seen`, and a Revoke button.
- Revoke:
  1. Remove row from `paired_devices` in SQLite.
  2. Delete keychain entry for that `device_id`.
  3. Close any live connection whose `device_id` matches (synchronous tear-down).
- After revoke: any future connection from that device fails
  `auth_invalid` — same response as an impostor trying with a fabricated id.
- Client side has no way to know it's been revoked other than seeing the
  error on next connect. That's fine — it's a kick, not a negotiation.

## Per-workspace scoping (V2 hook, not enforced in V1)

Data model supports it: `paired_devices.allowed_workspaces_json` stores
either `null` (all workspaces) or a JSON array of `workspace_id`s.

When enforced, every method that references a `workspace_id` checks against
this list and returns JSON-RPC error `code: 1001 "permission_denied"` on
mismatch. Notifications to subscribed clients filter by this list.

V1: field exists, is always `null` (all workspaces allowed). V2: desktop UI
for per-workspace scoping.

## Storage schema migrations

Adding these tables/columns requires a SQLite migration:

```sql
CREATE TABLE IF NOT EXISTS paired_devices (
    device_id TEXT PRIMARY KEY,
    device_label TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    last_seen_at INTEGER,
    allowed_workspaces_json TEXT -- nullable; null = all
);
```

Plus a schema version bump in whatever SuperHQ uses for migrations.

## Open questions

- **Which `keyring` crate feature set** — does it play nicely with the macOS
  codesigning setup SuperHQ uses? Needs a smoke test.
- **WebAuthn UX on iOS Safari** — passkeys on iOS require a specific flow;
  we should test `credential.create` + `credential.get` work cleanly from a
  PWA installed on iOS before committing WebAuthn-wrapped as the default.
- **TOTP drift window** — 1 step (30s) is standard; ±1 step is common for
  leniency. Decide at implementation time.
- **Client-side clock skew** — if the device clock is way off, the
  `timestamp` in session.hello will fail. Do we allow larger skew if the
  user enables "this device clock is unreliable"? Defer.
- **Concurrent pairing requests** — multiple clients asking to pair at the
  same time. Desktop should queue or serialize; doesn't need a wire change.

## Verification

- **Unit tests** (proto crate): HMAC proof construction & verification
  round-trip; TOTP generation & verification across the expected window.
- **Integration tests** (host crate): full pairing flow (request →
  approve/reject/totp), subsequent connect with valid/invalid auth, revoke
  in-flight.
- **WebAuthn smoke test**: in the demo page, add a "Set up device" flow
  using `navigator.credentials.create/get`; verify the ciphertext is
  recoverable only with a successful assertion.
- **Security review** before public rollout: HMAC construction, timestamp
  window, TOTP implementation, keychain access patterns.

## Out of this spec, into others

- **Pairing UI** (approve dialog, TOTP enrollment, paired-devices list) —
  Track 3 (desktop integration).
- **Web client pairing UX** (QR scanner, credential creation, passphrase
  fallback, recovery) — Track 4.
- **Audit log / session history** (who connected when) — Track 5 polish.
