# Changelog

## 0.4.2

- Review panel correctly labels modified files as changed instead of new. Host-side stat errors other than "file not found" (permission denied, I/O) previously collapsed to "brand new file" and painted the wrong badge.
- Discarded diff lines dim against the theme's panel color instead of a hard-coded dark overlay, so strikethrough rows stay readable in light and Catppuccin themes.

## 0.4.1

- Remote host-shell is opt-in. New toggle in Settings > Remote control, off by default. When off, paired devices can't open a new host-shell tab or attach to an existing one; the mobile PWA hides the Host Shell option accordingly.
- Replaced timestamped HMAC auth on session.hello with a nonce-based challenge-response. The host issues a one-shot nonce via session.challenge; the client HMACs it. No more clock-skew rejections, no replay surface.
- Multi-client PTY sessions no longer thrash dimensions. The host picks the minimum (cols, rows) across every attached client and sizes the PTY to that; each client letterboxes locally. Fixes the jittery redraws where two viewers made full-screen TUIs (Pi, Claude Code, Codex) paint at the wrong coordinates.
- Host-side transport hardening: 1 MiB cap on control-stream frames, handshake + stream-init deadlines, per-connection stream cap, frames decoded without logging attacker-controlled bodies.
- Notifications no longer reach unauthenticated peers; the control-stream subscription is deferred until after session.hello succeeds.
- Remote control toggle defaults to off on a settings-read failure instead of on. Migration 005 distinguishes duplicate-column from other ALTER failures so a partial migration can't silently look successful.
- Endpoint rotation blocks on the old server's shutdown and aborts the whole rotate if shutdown fails. pty_map entries are purged when a workspace is removed so stale tab ids can't attach.
- RPC client has a 60s per-call timeout and drains pending calls on disconnect. A dropped notification receiver no longer terminates the control-stream reader.
- PWA auto-connects on any authenticated route instead of only the home screen; cold launch into /workspace/:ws works. Disconnects clear the stale client and reconnect automatically.
- PWA terminal entries self-heal after a PTY failure; the next tab mount reopens cleanly instead of serving a dead handle.
- Agent SVG icons are sanitized with DOMPurify before rendering.
- Mobile tab bar: added close button on the active tab, confirm sheet with Checkpoint / Close / Cancel (mirrors the desktop prompt).
- Setup failures on agent tabs (missing API key, install script errors) surface to remote clients instead of leaving them on the spinner.
- PWA shows an in-app banner when a new version is installed by the service worker. Tapping Reload activates the waiting worker immediately; otherwise the update applies on the next cold launch.
- Software-keyboard no longer overlays the bottom action bar in the terminal. Modern browsers (iOS 17.4+, Chrome Android) resize the layout viewport automatically via `interactive-widget=resizes-content`; older browsers get a VisualViewport polyfill.
- QR scanner rewritten with jsQR. Handles QRs with embedded logos, which the previous decoder couldn't read.
- Cold launch into a workspace URL connects automatically instead of rendering blank.
- Minimal Umami analytics. Non-PII events for pair, session, workspace open, tab open/close, PWA update. No host ids, workspace/tab labels, or user content on the wire.

## 0.4.0

- Remote control: pair a phone or browser with a running host and drive your workspaces from it.
  - QR code or short host id for pairing.
  - Host runs an iroh endpoint behind a Settings toggle.
  - Paired devices list, audit log, and manual host id rotation in Settings. Rotation unpairs every device and generates a fresh id.
  - Mobile PWA at remote.superhq.ai with the workspace list, tab bar, xterm terminal, and the same new-tab menu as the desktop.
  - Tabs stay alive across navigation so switching between Home and a workspace does not replay scrollback.
  - Agent tab failures (missing API key, install script errors) now show on remote clients instead of leaving the tab spinning.
- Line-level staging in the review panel. Select individual lines of a diff to keep or discard without splitting hunks.
- Ask Agent from diff selection. Right-click a hunk and send a focused prompt to the active agent with the surrounding code.
- Remote transport hardening:
  - Auth is required by default on session.hello; notifications no longer reach unauthenticated peers.
  - Control-stream frame cap at 1 MiB. Handshake and stream-init deadlines. Per-connection stream count is bounded.
  - HMAC proofs are single-use per device within the skew window.
  - Endpoint rotation blocks on the old server's shutdown and aborts if it fails.
  - Remote-control setting defaults to off on a settings-read failure instead of on.
  - Workspace removal purges its pty_map entries.
- RPC client has a 60s per-call timeout and drains pending calls on disconnect instead of hanging.
- PWA propagates disconnects so the session reconnects instead of getting stuck.
- PWA terminal self-heals after a PTY failure so the next tab mount reopens cleanly.
- Host-supplied agent SVG icons are sanitized before rendering.

## 0.3.6

- Fixed crash when clicking Open Settings from the missing API key banner.
- Traffic lights and custom titlebar icons are now visually centered.

## 0.3.5

- OpenRouter is now a built-in provider with bundled icon, default host, and Codex integration. Anthropic and OpenAI rows in the Providers settings also pick up their proper logos.
- Each provider has its own enable/disable toggle. Disabled providers are treated as missing: the MITM proxy doesn't get configured for their hosts and they're not injected into the sandbox.
- New focusable Switch component (Tab + Space/Enter), used both for per-provider toggles and the General → Auto-launch agent setting.
- Providers tab restructured: switch on the title row, status indicator and Remove in a footer row, OpenAI gets a clarifying note about OAuth vs API key.
- Provider decision logic extracted into a pure resolver (`provider_resolve`) backed by unit tests, so disabled / required / one-of-group / gateway-handled rules live in one place.
- Text input fixes:
  - Long values no longer paint past the input bounds.
  - Caret stays visible while typing past the right edge.
  - Mouse wheel / trackpad horizontal scroll.
  - Drag-select past the input edge keeps extending the selection (auto-scrolls toward the cursor even when held still).
  - Double-click selects a word, triple-click selects all.
  - Click hit-testing fixed when the input is masked.

## 0.3.4

- Codex installer works again. Matches shuru 0.5.9 which drops the guest's silent-first-component stripping in favor of an explicit `strip_components` per download. Flat tarballs (Codex) and directory-wrapped ones (Node, Pi) now extract correctly.
- Bumped shuru runtime to 0.5.9.

## 0.3.3

- Appearance settings: theme picker with Light, Dark, Washi, Sumi, and Auto (follows system). Hot-swap — chrome, terminals, and scrollbars update in place without restart.
- Terminal palette, scrollbar, and agent icons adopt the active theme. Pi icon inverts on light themes instead of disappearing into the background.
- COLORFGBG hint passed to the guest and host shells so TUIs pick a matching light/dark palette.
- Runtime download is reliable on re-install: tar.gz is downloaded to a temp file first, then extracted, instead of streaming through gzip+tar in one pass. Progress bar no longer freezes on the rootfs write.
- Sandbox uses self-contained checkpoints (Direct mode) so a shuru version bump doesn't invalidate previously-saved workspaces.
- Terminal view no longer slips under the Ports status bar.
- Opening the Ports dialog while a terminal is still setting up no longer panics.
- App icon now renders in release builds (was referencing the source tree's absolute path).

## 0.3.2

- Review panel is a lot faster with many changed files. Each row is its own view, so hover no longer re-renders the full list every frame.
- Header totals (+N/-M) show up eagerly without computing the full hunk diff for every file.
- Deletions in mounted workspaces now show up reliably, including the case where a file only exists on the host.
- Discard on a deletion no longer flickers.
- Preserve the review panel's accumulated changes when switching to a tab without a sandbox instead of wiping it.

## 0.3.1

- Setting to disable auto-launch of the default agent on workspace open.
- New workspaces activate automatically on creation.
- About tab in settings.
- Settings content scrolls properly when it overflows.
- Shortcuts list regrouped to match the website.

## 0.3.0

- Clickable URLs in the terminal (cmd+click to open).
- Sidebar scrolls with a visible scrollbar.
- Settings moved to the titlebar.
- Review panel hidden on the host terminal.
- Keyboard badges suppressed while dialogs are open.

## 0.2.9

- Host terminal tab with a local PTY for host-side tasks.
- Collapsible sidebar.
- Workspace switch toast.
- Ports disabled on the host terminal.
- Fixed bracketed paste display garbling caused by readline's CR-only redisplay.

## 0.2.8

- Select and copy from the diff view.
- Logo centering fixes and OG image.

## 0.2.7

- Collapsible dock.
- Custom titlebar.
- Keyboard shortcuts with focus management.
- Ports shortcut.
- Enhanced superhq-dark theme.
- Extracted theme and syntax colors to JSON.

## 0.2.6

- Agent lifecycle hooks.
- Per-sandbox review watchers.
- OpenAI gateway fixes.
- Fixed re-entrant terminal panel borrow that crashed on opening settings.

## 0.2.4

- Agent notifications.
- Codex OAuth support for the Pi coding agent.
- Fixed missing keybinding hint for deactivated workspaces.
