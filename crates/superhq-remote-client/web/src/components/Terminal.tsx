// Persistent xterm.js host with a module-level registry so terminals
// survive *any* navigation — not just tab-switches inside a workspace.
//
// Why this shape:
//   xterm.js is expensive to construct and its `.element` holds the
//   rendered scrollback. If we tie its lifecycle to React component
//   mounts, leaving the /workspace/:ws route (e.g. back to /) nukes
//   every terminal and the host has to re-play scrollback on return.
//
//   Instead, each `(workspaceId, tabId)` pair gets an entry in a module-
//   scope `Map`: xterm + FitAddon + live PtyStreamHandle + input state.
//   React components are thin views that re-parent `term.element` into
//   whatever container is currently mounted and detach on unmount.
//   Disposal only happens via `pruneTerminalEntries` (called from App
//   when the snapshot drops a tab or the client identity changes) or
//   explicit close.
//
// Component tree:
//   <TerminalHost>                  one per workspace route
//     <PersistentTerminal>          one per ready tab; siblings stacked
//     <PersistentTerminal>          absolute inset-0, visibility-toggled
//     ...
//
// Handle forwarding: `TerminalHost` holds a map of per-tab handles and
// exposes a single `TerminalHandle` that dispatches to the active child.

import {
	forwardRef,
	useCallback,
	useEffect,
	useImperativeHandle,
	useRef,
} from "react";
import { Terminal as XtermTerminal, type ITheme } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import "@xterm/xterm/css/xterm.css";
import type { ClientHandle } from "../lib/wasm";
import type { TabInfo } from "../lib/types";

// ── Public handle forwarded to the KeyBar in workspace.tsx ─────────
export interface TerminalHandle {
	/// Write raw bytes to the ACTIVE tab's PTY.
	send: (bytes: Uint8Array) => void;
	/// Toggle / set the "next-key is Ctrl+" latch on the active tab.
	setCtrlArmed: (value: boolean) => void;
	getCtrlArmed: () => boolean;
	/// Toggle / set the "next-key is Alt+ / Meta+" latch on the active tab.
	setAltArmed: (value: boolean) => void;
	getAltArmed: () => boolean;
}

// xterm theme keyed to the app's near-black base.
const THEME: ITheme = {
	background: "#0a0a0b",
	foreground: "#e5e5e7",
	cursor: "#e5e5e7",
	selectionBackground: "rgba(99, 102, 241, 0.35)",
};

// ── Module-level registry ──────────────────────────────────────────

type PtyHandle = Awaited<ReturnType<ClientHandle["open_pty"]>>;

interface Entry {
	term: XtermTerminal;
	fit: FitAddon;
	ptyHandle: PtyHandle | null;
	/// Flipped by `disposeEntry` to stop the read loop. Once true the
	/// entry is considered dead — React views should no longer touch it.
	stopped: boolean;
	/// Key-bar latches live on the entry so they persist across nav.
	ctrlArmed: boolean;
	altArmed: boolean;
}

const registry = new Map<string, Entry>();

function entryKey(workspaceId: number, tabId: number): string {
	return `${workspaceId}:${tabId}`;
}

function createEntry(
	client: ClientHandle,
	workspaceId: number,
	tabId: number,
	container: HTMLElement,
): Entry {
	const term = new XtermTerminal({
		theme: THEME,
		fontFamily:
			"ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace",
		fontSize: 13,
		lineHeight: 1.2,
		cursorBlink: true,
		convertEol: true,
		scrollback: 5000,
		allowProposedApi: true,
	});
	const fit = new FitAddon();
	term.loadAddon(fit);
	term.open(container);
	try {
		fit.fit();
	} catch {
		/* container may be 0x0 during initial commit; a later ResizeObserver fit covers it */
	}

	// Auto-copy selections. xterm's canvas selection doesn't trigger
	// the OS copy menu on mobile, so we push to the clipboard ourselves
	// whenever the user releases a selection gesture. The selection
	// stays visible briefly after copying so the user sees what went
	// onto the clipboard.
	const onPointerUp = () => {
		const sel = term.getSelection();
		if (!sel) return;
		navigator.clipboard?.writeText(sel).catch(() => {
			/* ignore: permission denied / unsupported */
		});
	};
	container.addEventListener("pointerup", onPointerUp);
	container.addEventListener("pointercancel", onPointerUp);

	const entry: Entry = {
		term,
		fit,
		ptyHandle: null,
		stopped: false,
		ctrlArmed: false,
		altArmed: false,
	};

	// Input — stays wired for the life of the entry.
	term.onData((data) => {
		if (entry.stopped) return;
		let bytes: Uint8Array;
		if (entry.ctrlArmed && data.length === 1) {
			const code = data.charCodeAt(0);
			if (code >= 0x61 && code <= 0x7a) {
				bytes = new Uint8Array([code - 0x60]);
			} else if (code >= 0x41 && code <= 0x5a) {
				bytes = new Uint8Array([code - 0x40]);
			} else {
				bytes = new TextEncoder().encode(data);
			}
			entry.ctrlArmed = false;
		} else {
			bytes = new TextEncoder().encode(data);
		}
		if (entry.altArmed) {
			const prefixed = new Uint8Array(bytes.length + 1);
			prefixed[0] = 0x1b;
			prefixed.set(bytes, 1);
			bytes = prefixed;
			entry.altArmed = false;
		}
		entry.ptyHandle?.write(bytes).catch((err) => {
			console.warn("pty write failed:", err);
		});
	});

	term.onResize(({ cols, rows }) => {
		if (entry.stopped) return;
		entry.ptyHandle?.resize(cols, rows).catch(() => {
			/* host will catch up on the next frame */
		});
	});

	// PTY boot + read loop run for the life of the entry. Any failure
	// disposes the registry entry so the next mount triggers a fresh
	// open_pty — previously the entry stayed in the registry with a
	// dead ptyHandle, and `getOrCreateEntry` kept handing it back.
	void (async () => {
		const key = entryKey(workspaceId, tabId);
		const { cols, rows } = term;
		try {
			entry.ptyHandle = await client.open_pty(
				BigInt(workspaceId),
				BigInt(tabId),
				cols,
				rows,
			);
		} catch (err) {
			term.writeln(
				`\x1b[31mpty.open failed: ${
					err instanceof Error ? err.message : String(err)
				}\x1b[0m`,
			);
			// Self-heal: drop the stuck entry so a later remount can
			// try again from scratch.
			disposeEntry(key);
			return;
		}
		while (!entry.stopped) {
			try {
				const chunk = await entry.ptyHandle.read_chunk();
				if (!chunk.length) {
					term.writeln("\r\n\x1b[90m[stream closed by host]\x1b[0m");
					break;
				}
				term.write(chunk);
			} catch (err) {
				console.warn("pty read failed:", err);
				break;
			}
		}
		// Read loop exited for any reason (stream closed, read error,
		// stopped flag). If the loop wasn't stopped by an external
		// dispose, evict the entry so the next mount re-opens.
		if (!entry.stopped) {
			disposeEntry(key);
		}
	})();

	return entry;
}

function getOrCreateEntry(
	client: ClientHandle,
	workspaceId: number,
	tabId: number,
	container: HTMLElement,
): Entry {
	const key = entryKey(workspaceId, tabId);
	const existing = registry.get(key);
	if (existing && !existing.stopped) {
		// Re-parent xterm's root into the new container if needed.
		// This is the path that makes re-entering /workspace/:ws instant.
		const el = existing.term.element;
		if (el && el.parentNode !== container) {
			container.appendChild(el);
			try {
				existing.fit.fit();
			} catch {
				/* no-op */
			}
		}
		return existing;
	}
	const entry = createEntry(client, workspaceId, tabId, container);
	registry.set(key, entry);
	return entry;
}

function disposeEntry(key: string) {
	const entry = registry.get(key);
	if (!entry) return;
	entry.stopped = true;
	try {
		entry.ptyHandle?.free?.();
	} catch {
		/* no-op */
	}
	try {
		entry.term.dispose();
	} catch {
		/* no-op */
	}
	registry.delete(key);
}

/// Dispose every entry whose (workspace_id, tab_id) pair is *not* in
/// the `live` set. Call from App-level effects that watch the snapshot
/// so closed tabs / torn-down workspaces don't leak xterm instances.
export function pruneTerminalEntries(live: Set<string>) {
	for (const key of Array.from(registry.keys())) {
		if (!live.has(key)) disposeEntry(key);
	}
}

/// Dispose absolutely every entry. Used when the paired host changes
/// — stale `PtyStreamHandle`s are bound to the old client and useless.
export function resetTerminalRegistry() {
	pruneTerminalEntries(new Set());
}

// ── TerminalHost ───────────────────────────────────────────────────

interface HostProps {
	client: ClientHandle;
	workspaceId: number;
	tabs: TabInfo[];
	activeTabId: number | null;
}

const TerminalHost = forwardRef<TerminalHandle, HostProps>(
	function TerminalHost({ client, workspaceId, tabs, activeTabId }, ref) {
		const handlesRef = useRef<Map<number, TerminalHandle>>(new Map());

		const registerHandle = useCallback(
			(tabId: number, handle: TerminalHandle) => {
				handlesRef.current.set(tabId, handle);
				return () => {
					handlesRef.current.delete(tabId);
				};
			},
			[],
		);

		useImperativeHandle(
			ref,
			() => ({
				send: (bytes) => {
					if (activeTabId == null) return;
					handlesRef.current.get(activeTabId)?.send(bytes);
				},
				setCtrlArmed: (v) => {
					if (activeTabId == null) return;
					handlesRef.current.get(activeTabId)?.setCtrlArmed(v);
				},
				getCtrlArmed: () => {
					if (activeTabId == null) return false;
					return handlesRef.current.get(activeTabId)?.getCtrlArmed() ?? false;
				},
				setAltArmed: (v) => {
					if (activeTabId == null) return;
					handlesRef.current.get(activeTabId)?.setAltArmed(v);
				},
				getAltArmed: () => {
					if (activeTabId == null) return false;
					return handlesRef.current.get(activeTabId)?.getAltArmed() ?? false;
				},
			}),
			[activeTabId],
		);

		return (
			<div className="relative flex flex-1 overflow-hidden bg-app-base">
				{tabs.map((t) => (
					<PersistentTerminal
						key={`${workspaceId}:${t.tab_id}`}
						client={client}
						workspaceId={workspaceId}
						tabId={t.tab_id}
						isActive={t.tab_id === activeTabId}
						registerHandle={registerHandle}
					/>
				))}
			</div>
		);
	},
);

export default TerminalHost;

// ── PersistentTerminal ─────────────────────────────────────────────

interface PersistentProps {
	client: ClientHandle;
	workspaceId: number;
	tabId: number;
	isActive: boolean;
	registerHandle: (tabId: number, handle: TerminalHandle) => () => void;
}

function PersistentTerminal({
	client,
	workspaceId,
	tabId,
	isActive,
	registerHandle,
}: PersistentProps) {
	const containerRef = useRef<HTMLDivElement | null>(null);

	// Mount: attach the registry entry's DOM element into our container.
	// Cleanup: detach the element (but do NOT dispose the entry — the
	// registry keeps it alive across navigation).
	useEffect(() => {
		const container = containerRef.current;
		if (!container) return;

		const entry = getOrCreateEntry(client, workspaceId, tabId, container);

		const onResize = () => {
			if (entry.stopped) return;
			try {
				entry.fit.fit();
			} catch {
				/* no-op */
			}
		};
		const ro = new ResizeObserver(onResize);
		ro.observe(container);
		window.addEventListener("resize", onResize);

		// Register a handle that reads live state from the entry so
		// key-bar input lands on the correct PTY even across remounts.
		const handle: TerminalHandle = {
			send: (bytes) => {
				if (entry.stopped) return;
				entry.ptyHandle?.write(bytes).catch((err) => {
					console.warn("pty write failed:", err);
				});
			},
			setCtrlArmed: (v) => {
				entry.ctrlArmed = v;
			},
			getCtrlArmed: () => entry.ctrlArmed,
			setAltArmed: (v) => {
				entry.altArmed = v;
			},
			getAltArmed: () => entry.altArmed,
		};
		const unregister = registerHandle(tabId, handle);

		return () => {
			unregister();
			ro.disconnect();
			window.removeEventListener("resize", onResize);
			// Detach xterm's root without disposing the instance. If the
			// entry was pruned concurrently, `.element` is null and we
			// skip safely.
			const el = entry.term.element;
			if (el && el.parentNode === container) {
				container.removeChild(el);
			}
		};
	}, [client, workspaceId, tabId, registerHandle]);

	// On activation: refit (the container may have resized while we were
	// detached or hidden) and focus. Deferred one frame so the CSS
	// visibility flip has committed before we measure.
	useEffect(() => {
		if (!isActive) return;
		const entry = registry.get(entryKey(workspaceId, tabId));
		if (!entry || entry.stopped) return;
		const id = requestAnimationFrame(() => {
			try {
				entry.fit.fit();
			} catch {
				/* no-op */
			}
			entry.term.focus();
		});
		return () => cancelAnimationFrame(id);
	}, [isActive, workspaceId, tabId]);

	return (
		<div
			ref={containerRef}
			className="absolute inset-0 overflow-hidden"
			style={{
				visibility: isActive ? "visible" : "hidden",
				pointerEvents: isActive ? "auto" : "none",
			}}
			aria-hidden={!isActive}
		/>
	);
}
