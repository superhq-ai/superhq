# Remote Control — Overview

Let users connect to their local SuperHQ instance from a phone, tablet, or
another computer via a web interface. Screen-share with their agents, send
input, review diffs, all without SSH, VPN, port forwarding, or any server
infrastructure.

## Why

The desktop app is where agents run, but agents run long. Users want to:
- Check on a running agent from their phone when away from the desk
- Read/accept a diff on an iPad without opening a laptop
- Have a second device tailing output without remote-desktop latency
- Not set up a VPN/SSH to do any of the above

## What we're building

- **A drop-down in the desktop title bar** → "Remote Control" → reveals a QR code and NodeID
- **A web app at `remote.superhq.ai`** (static PWA) — scan the QR or paste the NodeID, pair with the host, then interact with active sessions
- **Peer-to-peer transport via iroh** — no SuperHQ servers in the path beyond public relays
- **Per-device auth** with pairing approval on host + TOTP fallback
- **Daemon/tray mode** so closing the main window doesn't kill running agents
- **Multiple connected devices** — pair your phone, tablet, and work laptop; revoke any at any time

## What we're not building (yet)

- Native mobile apps — PWA only
- Backend services / SuperHQ-owned relay servers (use iroh's public relays first)
- Cross-user sharing / collaboration (strictly "your devices accessing your host")
- Offline queuing of commands

## Architecture at 10k feet

```
Desktop SuperHQ (host)                    Web client (remote.superhq.ai)
┌──────────────────────┐                  ┌──────────────────────┐
│   GPUI app           │                  │   PWA (Solid/React)  │
│                      │                  │                      │
│   ┌──────────────┐   │                  │   ┌──────────────┐   │
│   │ iroh Endpoint│◄──┼── QUIC/relay ───►┼──►│ iroh (WASM)  │   │
│   └──────┬───────┘   │                  │   └──────┬───────┘   │
│          │           │                  │          │           │
│   ALPN: superhq/     │                  │   ALPN: superhq/     │
│   remote/1           │                  │   remote/1           │
│   (multiplexed:      │                  │   (same)             │
│   control/pty/       │                  │                      │
│   diff/status)       │                  │                      │
│                      │                  │                      │
│   ALPN: iroh-blobs   │◄── parallel ───►│   ALPN: iroh-blobs   │
│   (for binary xfer)  │                  │   (for binary xfer)  │
└──────────────────────┘                  └──────────────────────┘
```

Both ends are first-class iroh peers. Browsers always connect outbound via
iroh's relays (no direct UDP possible). Desktop can be direct or relayed
depending on NAT.

## Tracks

Work splits into five tracks. Each gets its own spec. Order below is spec/build
order, not a waterfall — tracks overlap.

### Track 1 — Transport layer
Custom iroh protocols, connection lifecycle, stream multiplexing, relay config,
reconnect behavior. The foundation. Spec: `01-transport.md`.

### Track 2 — Auth & pairing
Pairing flow (QR + host approval, TOTP as fallback), per-device credentials,
secure storage (OS keychain on host, WebAuthn on client), revocation, session
binding. Spec: `02-auth.md`.

### Track 3 — Desktop integration
Title-bar button, remote-control popover, paired-devices UI, tray icon, daemon
mode, graceful window close. How the feature lives inside the existing GPUI
app. Spec: `03-desktop.md`.

### Track 4 — Web client PWA
Static site architecture, iroh-WASM in a worker, terminal renderer, custom
diff component built on Shiki (to match the GPUI diff's look-and-feel
instead of using an off-the-shelf diff viewer), service worker,
installability, multi-session UI. Spec: `04-web.md`.

### Track 5 — Operational polish
Notifications (in-app → web push), reconnect UX, connection-health indicators,
observability, error recovery. Spec: `05-polish.md`.

## Sequencing

1. **Track 1** first — everything depends on it.
2. **Track 3 (MVP slice: button + popover showing NodeID)** in parallel with
   **Track 4 (PWA shell that connects to a NodeID)**. Together they prove a
   demo-able end-to-end path: click in desktop, scan on phone, see mirrored
   terminal.
3. **Track 2 (auth)** once the pipe works. Must land before any public shipping.
4. **Track 4 — feature parity** (multi-tab, diffs, Ask Agent).
5. **Track 3 — daemon + tray** once the feature earns its keep.
6. **Track 5** — ongoing, as rough edges surface.

## Design considerations

- **Shared theme tokens** — GPUI themes (light/dark, accents) must work on the
  web out of the box. We define tokens once and consume them from both sides.
  Options to explore when specing Track 4: emit a JSON theme file from the
  GPUI theme definition and load it into CSS custom properties at runtime, or
  bake a build step. Either way, **one source of truth for colors** — no
  hand-picking hexes on the web side.
- **Tailwind CSS 4** for the web UI. Tailwind 4 has meaningful differences
  from v3 (CSS-first config, native CSS layer usage, new engine). When we
  write the Track 4 spec, read the current Tailwind 4 docs and tailor the
  setup to them rather than assuming v3 muscle memory.
- **The web UI is not obligated to mirror the desktop UI.** Mobile and "other
  device" contexts have different affordances — touch targets, single-hand
  use, portrait aspect, background execution limits. The desktop's
  dense-sidebar-plus-terminal layout isn't automatically the right answer on
  a phone. Treat the web UI as a fresh design opportunity, aiming for
  ergonomic and a little radical where it earns ergonomics. Shared theme
  tokens and shared protocols keep the app *recognizable* as SuperHQ without
  forcing pixel parity.

## Constraints that shaped the design

- **iroh-WASM in browsers is relay-only** (no direct UDP from a browser sandbox).
  Latency floor is ~30-100ms added hop, but E2E encryption preserved.
- **Ring (crypto) needs LLVM clang with wasm32 target** — contributor setup cost,
  scripted in `build.sh` per project.
- **iroh NodeId is identity, not a credential** — pairing is what makes it safe.
- **PWA notifications on iOS require home-screen install** (iOS 16.4+); Android
  and desktop more permissive.
- **Sandboxes must outlive the main window** for daemon mode to be meaningful.
  Already true (they're separate processes), but needs verification.

## Validation done

- **Spike at `~/Projects/iroh-echo-spike`** — browser→native custom-protocol
  echo works end-to-end.
  - WASM bundle: 2.5MB uncompressed, ~700-900KB after brotli.
  - Cold start: 33ms (WASM load + node spawn).
  - First echo RTT: 540ms (includes connection setup; steady-state will be
    much lower).
  - Transport confirmed: iroh's browser path uses relays, custom ALPN works.

## Open questions (to resolve as specs land)

- **Relay strategy** — use iroh public relays V1, self-host later? Affects trust
  surface and ops burden.
- **Tray/daemon crate** — pull `tray-icon` (Tauri's) or roll direct OS APIs?
- **Web framework** — Solid, React, or vanilla? Depends on what terminal/diff
  libraries we pick.
- **Web Push infrastructure** — do we stand up a push server, or V1 = "app must
  be open to get notifications"?
- **`remote.superhq.ai` hosting** — CDN + DNS + cert ownership.
- **neko-computer reuse** — what from that project's architecture is salvageable?
  (Need to audit.)

## How specs evolve

Each track's spec is a living doc. When implementation surfaces a forced choice
or a better approach, we update the spec before writing code. Status at the top
of each spec: `draft` / `approved` / `in-progress` / `shipped`.

Cross-cutting decisions that affect multiple specs get recorded here in
`00-overview.md` under a "Decisions" section (to be added as they land) so the
track specs stay focused.
