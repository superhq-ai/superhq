# Track 1 — Transport Layer

Status: `draft`

## Context

Everything else in the remote-control feature sits on top of a bidirectional
peer-to-peer link between the desktop host and a connected device. This track
defines that link — the iroh ALPN, the stream types multiplexed over it, the
message shapes, and the lifecycle behavior. Once this is specced and built,
Tracks 2–5 slot into clearly defined seams.

Validated already: the spike at `~/Projects/iroh-echo-spike` confirmed iroh
with a custom ALPN works browser↔native via relay. This spec picks up where
that left off.

## Goals

- **One persistent connection per device** for the lifetime of an attach
  session. No per-action reconnects.
- **Multiplex everything** over that connection: control commands, N concurrent
  terminal streams, diff events, status updates.
- **Evolvable** — protocol version field from day one; unknown message types
  ignored rather than fatal.
- **Same code both sides** — protocol types and serializer in a shared Rust
  crate compiled natively and to WASM.
- **Cheap on the wire** for PTY data (hot path), acceptable overhead for
  control-plane messages.

## Non-goals

- Authentication / authorization logic (that's Track 2). This spec assumes the
  peer has already been authenticated by the time a session is established.
- UI rendering, client state management, notification routing (Tracks 3–5).
- Replication / persistence. Streams are real-time; nothing is stored by the
  transport layer.
- Cross-device sharing (a session attached from one device is independent from
  another device attached to the same host).

## High-level shape

**One ALPN**, many streams.

```
ALPN: superhq/remote/1

[Host iroh::Endpoint]  ◄── 1 connection ──►  [Client iroh::Endpoint]

        Multiplexed QUIC streams on that connection:
        ┌──────────────────────────────────────────┐
        │ [0] Control stream (always-open, bi)     │
        │ [1] PTY stream for tab_id=A      (bi)    │
        │ [2] PTY stream for tab_id=B      (bi)    │
        │ [3] Diff events                  (bi)    │
        │ [4] Status events         (uni: h→c)     │
        │ ...                                      │
        └──────────────────────────────────────────┘
```

Client opens control stream as stream 0 on connect. Other streams are opened
on demand (by either side), and their first message is a `StreamInit`
identifying what they carry.

## Workspace model

SuperHQ has the concept of **workspaces**: the user can have many open at
once, each with its own sandbox, tabs, and review panel state. The remote
transport treats workspaces as first-class.

- **Flat, fully-qualified references.** Every tab reference carries
  `(workspace_id, tab_id)`. `tab_id` alone is a per-workspace counter and
  is *not* globally unique on a host.
- **Session sees all workspaces.** A connected client can enumerate every
  workspace and interact with tabs across all of them concurrently, mirroring
  how the desktop UI works. No "active workspace" session state.
- **Per-workspace permission scoping is an auth-layer concern**, not a
  transport concern. Track 2 may restrict which workspaces a paired device
  can access; that shows up as a JSON-RPC permission-denied error, not a
  different wire shape.
- **Dynamic**: workspaces can be added or removed while the session is
  connected. `workspaces.added` / `workspaces.removed` notifications keep
  the client in sync.

## Stream types

### Control (bidirectional)
Opened by client immediately after connection. Carries session-level messages
as JSON-RPC calls and notifications:

Requests (client → host, responded with id match):
- `session.hello` → `{ protocol_version, session_id, resume_token, host_info, workspaces, tabs }`
- `workspaces.list` → `[ WorkspaceInfo, ... ]`
- `tabs.list` → `[ TabInfo, ... ]` (across all workspaces)
- `pty.attach { workspace_id, tab_id }` → `{ cols, rows, initial_buffer }`
- `pty.detach { workspace_id, tab_id }` → `{ ok: true }`
- `pty.resize { workspace_id, tab_id, cols, rows }` → `{ ok: true }`
- `diff.subscribe { workspace_id }` → `{ ok: true }` (notifications follow)
- `diff.unsubscribe { workspace_id }` → `{ ok: true }`
- `diff.keep { workspace_id, path }` → `{ ok: true }`
- `diff.discard { workspace_id, path }` → `{ ok: true }`
- `diff.apply_partial { workspace_id, path, discarded_lines }` → `{ ok: true }`
- `diff.ask_agent { workspace_id, path, selected_text, instruction }` → `{ ok: true }`
- `status.subscribe` → `{ ok: true }`
- `status.unsubscribe` → `{ ok: true }`
- `session.ping` → `{ ok: true }` (optional soft-stall detector)
- `session.close { reason }` → `{ ok: true }` (polite shutdown)

Notifications (host → client, no response expected):
- `workspaces.added { workspace: WorkspaceInfo }`
- `workspaces.removed { workspace_id }`
- `tabs.added { tab: TabInfo }`
- `tabs.removed { workspace_id, tab_id }`
- `tabs.updated { tab: TabInfo }`
- `diff.file_changed { workspace_id, path, stats }`
- `diff.file_removed { workspace_id, path }`
- `diff.full_diff { workspace_id, path, hunks }` (sent on demand)
- `status.agent_state { workspace_id, tab_id, state }`

### PTY (bidirectional, one per attached terminal)
First frame: `StreamInit::Pty { workspace_id, tab_id, cols, rows }`. After that, each
direction is raw bytes — no framing, no encoding overhead.

- Host → client: terminal output bytes (what the terminal emulator would
  receive from the PTY master).
- Client → host: terminal input bytes (keystrokes, paste, etc.).

Resize events are sent as **out-of-band requests** on the control stream
(not inline in the PTY stream), to keep the hot path byte-clean:
`pty.resize { workspace_id, tab_id, cols, rows }`.

### Diff (bidirectional, one per subscribed workspace)
First frame: `StreamInit::Diff { workspace_id }`.

- Host → client: `diff.file_changed`, `diff.file_removed`, `diff.full_diff`,
  `diff.stats` notifications (same model as the desktop `ChangesTab`).
- Client → host: `diff.keep`, `diff.discard`, `diff.apply_partial`,
  `diff.ask_agent` requests (above).

### Status (unidirectional host → client)
First frame: `StreamInit::Status`. After that, a stream of
`status.agent_state` notifications matching the existing `AgentStatus` enum
from `sandbox/event_watcher.rs`.

## Wire format

- **Framing**: newline-delimited (JSONL) on every stream *except* PTY data
  streams, where bytes are raw (no framing — the stream itself is the frame).
- **Encoding**: JSON-RPC 2.0 via `serde_json` for structured messages.
  Standardized request/response + notification semantics match our use cases
  (RPC calls, subscribe + push stream, one-shot events) cleanly.
- **Why JSON-RPC 2.0, not a custom envelope**:
  - Correlation via `id` is already specced — we don't reinvent it.
  - Notifications (messages with no `id`) naturally model server-pushed events.
  - Error object (`code`/`message`/`data`) is standardized — our error handling
    is obvious to anyone reading the protocol.
  - Low bus-factor: "it's JSON-RPC 2.0" tells a new contributor everything
    they need to know about the envelope.
  - Rolling our own envelope would arrive at roughly the same shape, with
    more drift risk and no gain.
- **Why we don't pull a library (e.g. `jsonrpsee`)**: we only need the envelope
  types, not a whole server framework. Our proto crate is ~50-ish lines of
  structs + a serializer. Keeps WASM bundle lean and the protocol surface
  explicit in our own code.
- **Envelope types** (abbreviated; full definitions in the proto crate):
  ```rust
  enum Message {
      Request {
          jsonrpc: &'static str, // "2.0"
          id: u64,
          method: String,        // e.g. "tabs.list", "pty.attach"
          params: serde_json::Value,
      },
      Response {
          jsonrpc: &'static str, // "2.0"
          id: u64,
          #[serde(flatten)]
          body: ResponseBody,    // Success { result } | Error { error: RpcError }
      },
      Notification {
          jsonrpc: &'static str, // "2.0"
          method: String,        // e.g. "diff.file_changed", "tab.closed"
          params: serde_json::Value,
      },
  }
  ```
  Method names use dot-separated namespacing (`tabs.list`, `pty.attach`,
  `diff.subscribe`, `diff.file_changed`, `status.agent_state`) so they group
  readably in logs.
- **Forward-compat**: unknown methods are logged and ignored. New methods can
  be added freely without bumping the protocol version.
- **Protocol version** (distinct from envelope wire format) negotiated in the
  `session.hello` method — see Versioning.

## Blob handling

Binary payloads (images, file content, scrollback snapshots, attachments) use
**iroh-blobs** for transfer. The JSON-RPC message carries a small handle
referencing the blob; the actual bytes are fetched via the iroh-blobs
protocol over its own ALPN.

### Why iroh-blobs

- **Integrity by default** — BLAKE3-verified chunks, so a corrupt or truncated
  transfer is detected.
- **Resume on partial** — if the connection drops mid-transfer, the receiver
  picks up from the last verified chunk instead of restarting. Important for
  the flaky-relay case.
- **Chunked streaming** — large blobs don't require the sender to materialize
  the whole payload in memory first.
- **Minimal code on our side** — we use the library rather than reinventing
  integrity, framing, and resume.
- **Validated** in the browser-blobs example: works in WASM, runs in the
  browser, compatible with our architecture.

### Tradeoffs accepted

- Adds ~300-800 KB to the WASM bundle (measured after integration; acceptable
  per validation).
- Another crate to keep version-aligned with `iroh` (compatible major/minor
  pairs are published together).
- Slight CPU cost to hash each blob (negligible for our volumes).
- Runs an in-memory blob store on both sides. Persistence explicitly off for
  V1.

### Shape in JSON-RPC

Any method that carries binary references it via a handle:

```jsonc
{ "hash": "bafk...blake3...", "format": "raw", "mime": "image/png", "size": 142337 }
```

- `hash` — the BLAKE3 hash iroh-blobs uses to address the blob.
- `format` — iroh-blobs' format (`raw` for arbitrary bytes, `hash_seq` for
  lists; we use `raw`).
- `mime` — application-level hint for rendering on the client (not used by
  iroh-blobs itself).
- `size` — for progress UI.

We pass only the hash, not a full `BlobTicket`, because both peers are
already connected with known NodeIds — the ticket's addressing info would be
redundant.

### Host → client flow

1. Host adds bytes to its local store: `store.add_bytes(data)` → `hash`.
2. Host sends JSON-RPC response/notification with the handle.
3. Client calls `downloader.download(hash_and_format, [host_node_id])` via
   iroh-blobs — streams the bytes into its own in-memory store with BLAKE3
   verification.
4. Client reads from its store when it needs the bytes.

### Client → host flow

1. Client adds bytes to its local store: `store.add_bytes(data)` → `hash`.
2. Client sends JSON-RPC request whose params reference the handle.
3. Host's request handler fetches from the client via
   `downloader.download(hash_and_format, [client_node_id])`.
4. Once the fetch completes (verified), host processes the request.

### Cleanup

- In-memory store on both sides. V1 has no persistence — closing the session
  frees everything.
- Short GC interval: blobs the application has finished with are tagged for
  deletion and reaped.
- Specifics (retention duration, eviction policy) are tuning-time decisions,
  not protocol-level.

### Scoping

V1 uses iroh-blobs purely as a transport primitive: transient in-memory
store, session-scoped blobs, no cross-session sharing or persistence. If V2
needs persistent artifacts (file history, image library, agent output
archives), we already have the infrastructure — flip on disk storage and
adjust retention. No wire-format change required.

## Shared crate

New workspace member: `crates/superhq-remote-proto`.

- Message types (structs/enums for each `Envelope::kind`)
- `encode_envelope()`, `decode_envelope()` helpers
- Stream init message types
- Protocol version constant

Both desktop (`crates/superhq-remote-host`, new) and web client
(`crates/superhq-remote-client`, new) depend on this.

Structure:
```
crates/
├── superhq-remote-proto/      # shared message types, serialization
│   └── src/
│       ├── lib.rs
│       ├── control.rs
│       ├── pty.rs
│       ├── diff.rs
│       └── status.rs
├── superhq-remote-host/       # host-side: accepts connections, maps to app state
│   └── src/
│       └── ...
└── superhq-remote-client/     # client-side: builds to WASM, used by PWA
    └── src/
        └── ...
```

## Lifecycle

### Connection establishment
1. Client opens iroh connection to host's NodeId with ALPN `superhq/remote/1`.
2. Client opens control stream (bidirectional) and calls `session.hello`.
3. Host responds with accepted version, host info, and initial tabs snapshot.
4. Client makes subscription / attach requests as needed.

### Attaching a PTY
1. Client calls `pty.attach { tab_id }` on the control stream.
2. Host responds with `{ cols, rows, initial_buffer }`. The `initial_buffer`
   is the last N KB of the terminal's scrollback so the client doesn't start
   on a blank screen.
3. Client opens a new bidirectional QUIC stream, sends
   `StreamInit::Pty { workspace_id, tab_id, cols, rows }`.
4. Host pipes PTY output → stream → client; stream → PTY input.

### Detachment
- Either side can shut down a PTY stream. Call `pty.detach` on the control
  stream first (polite), then close the stream. Abrupt closure is also handled.

### Reconnect
- If the iroh connection drops (relay hop, network flap), the client:
  1. Attempts to reconnect with exponential backoff (starting 500ms, capped at 15s).
  2. On successful reconnect, re-opens control stream and calls
     `session.hello` again, including a `resume_token` in params if it has
     one.
  3. Host either accepts (returns the same session id) or rejects (forces a
     fresh session).
- On the host, pending PTY streams and subscriptions are kept alive for 30s
  after a connection drop to allow resume without losing terminal state.
  After 30s they're torn down.

### Shutdown
- Polite: client calls `session.close { reason }`, waits for response, closes
  connection.
- Abrupt: connection drops. Host treats as "pending reconnect" for 30s.

## Error handling

- **Malformed messages**: log, drop message, do not close connection. (This
  protects against forward-compat and buggy client versions.)
- **Unknown `method`**: if it was a request (has `id`), respond with JSON-RPC
  error code `-32601` ("Method not found"). If notification (no `id`), silently
  ignore. Forward-compat.
- **Domain errors** (permission denied, tab not found, etc.): respond with a
  JSON-RPC error using `data` field for any structured detail.
- **Stream init missing/invalid**: close the offending stream, do not affect
  others.
- **Control stream closed unexpectedly**: treat as connection loss, tear down
  all subordinate streams, begin reconnect backoff.
- **Host-side permission denied** (post-auth, if a client requests a tab it
  shouldn't see): respond with a typed error reply, do not kill the stream.

## Versioning

- **Envelope version**: `"jsonrpc": "2.0"` — fixed. We're not inventing a new
  envelope wire format, so there's nothing to bump here.
- **Protocol version**: negotiated in `session.hello` params. Clients
  advertise highest they support; host picks the min of the two. V1 supports
  exactly protocol version 1.
- New methods and notification kinds can be added freely without bumping the
  protocol version — unknown methods are logged and ignored, so older peers
  just don't see new features.

## Reliability & ordering

- QUIC streams are reliable and ordered. Within a stream, message order is
  preserved.
- Across streams, no ordering guarantees. If a feature needs cross-stream
  ordering, it must thread that itself (e.g. sequence numbers in messages).

## Open questions

- **Resume tokens**: how are they generated and validated? Simplest: host
  issues a random opaque token on `HelloAck`, client echoes it on reconnect.
  Track 2 may extend this with auth-binding.
- **Backpressure**: QUIC has its own flow control, but for control-stream
  bursts (e.g. a giant initial tabs snapshot on a workspace with hundreds of
  agents) we may want app-level chunking. Defer to measurement.
- **Stream count limits**: QUIC imposes caps, usually 100+. We need to
  understand iroh's configuration and whether a user with many agents could
  hit them.
- **Compression**: PTY output of a busy agent can be repetitive. zstd on
  host→client PTY streams is an option. Not V1.

## Verification

- **Unit tests** (in `superhq-remote-proto`): roundtrip each message type,
  unknown-kind handling, version negotiation.
- **Integration test** (new binary, similar to the spike): two nodes in one
  process — native host + native client — go through the full lifecycle
  (connect, attach PTY, echo bytes both ways, detach, reconnect resume).
- **Browser smoke**: evolve `iroh-echo-spike` into a "tiny remote client" that
  connects and attaches to one PTY on a running SuperHQ instance. Confirms
  WASM path works against the real host implementation.
- **Chaos**: kill the network mid-session, confirm reconnect + resume restores
  terminal state correctly.

## Out of this spec, into others

- **Auth gate** before `HelloAck` succeeds — Track 2.
- **Mapping from incoming protocol messages to the SuperHQ app state**
  (terminal panel, review panel, etc.) — Track 3, because that's app-internal
  plumbing.
- **What the client does with the streams** — Track 4.
