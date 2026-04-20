// Pair screen. Two layouts collapsed into one route so transitions
// stay cheap and state is kept in memory:
//
//   status.idle      → the scroll form with instructions, QR scan,
//                      host id fallback.
//   status.approving → dedicated full-page view with an illustration
//                      of the actual desktop approval modal.
//   status.securing  → dedicated full-page view with a Touch ID /
//                      Face ID hint.
//   status.error     → full-page card with Retry / Back.
//
// Design language matches the desktop (superhq-dark tokens, flat
// surfaces, #7b9ef0 accent). Mobile layout uses text-sm / text-base
// for legibility.

import { useCallback, useRef, useState } from "react";
import { useNavigate } from "react-router";
import { useConnectionStore } from "../state/store";
import { saveCredential } from "../lib/storage";
import { connect, type ClientHandle } from "../lib/wasm";
import QrScannerModal from "../components/QrScannerModal";
import {
    BiometricIllustration,
    PairingModalIllustration,
} from "../components/DesktopModalIllustration";
import { track } from "../lib/analytics";

type Status =
    | { kind: "idle" }
    | { kind: "approving" }
    | { kind: "securing" }
    | { kind: "error"; message: string };

export default function PairRoute() {
    const navigate = useNavigate();
    const setPairedHost = useConnectionStore((s) => s.setPairedHost);
    const [peerId, setPeerId] = useState("");
    const [status, setStatus] = useState<Status>({ kind: "idle" });
    const [scannerOpen, setScannerOpen] = useState(false);

    // Live ClientHandle during the approving phase. Kept in a ref so
    // the Cancel button can close the iroh connection, which signals
    // the host to dismiss its approval modal.
    const pairingClientRef = useRef<ClientHandle | null>(null);
    // Flipped true when the user cancels via Cancel. The in-flight
    // pairing_request promise will reject as the connection closes;
    // we consult this flag to classify the rejection as "user-initiated"
    // and return to idle instead of flashing an error screen.
    const cancelledRef = useRef(false);

    const runPair = useCallback(
        async (id: string) => {
            cancelledRef.current = false;
            setStatus({ kind: "approving" });
            let result: { device_id: string; device_key: string };
            try {
                const client = await connect(id);
                pairingClientRef.current = client;
                const label = navigator.platform || "browser";
                result = await client.pairing_request(label);
            } catch (e) {
                if (cancelledRef.current) {
                    track("pair.cancelled");
                    setStatus({ kind: "idle" });
                    return;
                }
                track("pair.failure");
                setStatus({
                    kind: "error",
                    message:
                        e instanceof Error
                            ? e.message
                            : "Pairing was rejected or timed out on the host.",
                });
                return;
            } finally {
                try {
                    pairingClientRef.current?.close();
                } catch {
                    /* no-op */
                }
                pairingClientRef.current = null;
            }
            setStatus({ kind: "securing" });
            try {
                await saveCredential(id, {
                    device_id: result.device_id,
                    device_key: result.device_key,
                });
            } catch (e) {
                setStatus({
                    kind: "error",
                    message: e instanceof Error ? e.message : String(e),
                });
                return;
            }
            setPairedHost({
                peerId: id,
                label: `SuperHQ host ${id.slice(0, 8)}…`,
                pairedAt: Date.now(),
            });
            track("pair.success");
            navigate("/", { replace: true });
        },
        [navigate, setPairedHost],
    );

    const cancelApproving = useCallback(() => {
        cancelledRef.current = true;
        try {
            pairingClientRef.current?.close();
        } catch {
            /* no-op — the runPair finally will try again */
        }
        setStatus({ kind: "idle" });
    }, []);

    async function onManualSubmit() {
        const id = peerId.trim();
        if (id.length < 32) return;
        await runPair(id);
    }

    async function onQrDetected(id: string) {
        setScannerOpen(false);
        setPeerId(id);
        await runPair(id);
    }

    const body = (() => {
        switch (status.kind) {
            case "idle":
                return (
                    <IdleForm
                        peerId={peerId}
                        setPeerId={setPeerId}
                        onOpenScanner={() => setScannerOpen(true)}
                        onManualSubmit={onManualSubmit}
                    />
                );
            case "approving":
                return (
                    <ApprovingView
                        onCancel={cancelApproving}
                        deviceLabel={navigator.platform || "browser"}
                    />
                );
            case "securing":
                return <SecuringView />;
            case "error":
                return (
                    <ErrorView
                        message={status.message}
                        onRetry={() => {
                            if (peerId.trim().length >= 32) {
                                void runPair(peerId.trim());
                            } else {
                                setStatus({ kind: "idle" });
                            }
                        }}
                        onBack={() => setStatus({ kind: "idle" })}
                    />
                );
        }
    })();

    return (
        <div
            className="h-full w-full overflow-y-auto bg-app-base"
            style={{
                paddingTop: "env(safe-area-inset-top)",
                paddingBottom: "env(safe-area-inset-bottom)",
            }}
        >
            {body}
            {scannerOpen ? (
                <QrScannerModal
                    onDetected={onQrDetected}
                    onClose={() => setScannerOpen(false)}
                />
            ) : null}
        </div>
    );
}

// ── Idle (form) ──────────────────────────────────────────────────────

function IdleForm({
    peerId,
    setPeerId,
    onOpenScanner,
    onManualSubmit,
}: {
    peerId: string;
    setPeerId: (v: string) => void;
    onOpenScanner: () => void;
    onManualSubmit: () => void;
}) {
    const canSubmit = peerId.trim().length >= 32;
    const steps = [
        "Open SuperHQ on your desktop.",
        "Click the network icon next to the sidebar toggle.",
        "Scan the QR in the popover, or copy the host id.",
        "Approve the pairing request on your desktop.",
        "Confirm with Touch ID, Face ID, or your device password.",
    ];
    return (
        <div className="mx-auto flex max-w-md flex-col gap-7 px-5 py-7">
            <div className="flex items-center gap-3">
                <img
                    src="/app-icon-128.png"
                    alt="SuperHQ"
                    className="h-11 w-11 rounded-[12px] ring-1 ring-app-hairline"
                />
                <div className="flex flex-col gap-0.5">
                    <h1 className="text-base font-semibold text-app-text">
                        Connect a SuperHQ host
                    </h1>
                    <p className="text-sm text-app-text-muted">
                        Pair this browser with the desktop app
                    </p>
                </div>
            </div>

            <div className="flex flex-col gap-3">
                <h2 className="text-base font-semibold text-app-text">
                    How to pair this device
                </h2>
                <ol className="flex flex-col gap-3">
                    {steps.map((s, i) => (
                        <li key={i} className="flex items-start gap-3">
                            <span className="mt-[2px] w-5 shrink-0 text-sm font-medium text-app-text-muted">
                                {i + 1}.
                            </span>
                            <span className="text-sm leading-relaxed text-app-text">
                                {s}
                            </span>
                        </li>
                    ))}
                </ol>
            </div>

            <button
                onClick={onOpenScanner}
                className="glass-pill glass-pill--accent flex h-11 w-full items-center justify-center gap-2 rounded-2xl text-[14px] font-medium text-white"
            >
                <svg
                    width="16"
                    height="16"
                    viewBox="0 0 24 24"
                    fill="none"
                    stroke="currentColor"
                    strokeWidth={2}
                    strokeLinecap="round"
                    strokeLinejoin="round"
                >
                    <rect x="3" y="3" width="7" height="7" />
                    <rect x="14" y="3" width="7" height="7" />
                    <rect x="3" y="14" width="7" height="7" />
                    <path d="M14 14h3v3h-3zM20 14h1v1h-1zM14 20h1v1h-1zM17 17h4v4h-4z" />
                </svg>
                Scan QR code
            </button>

            <div className="flex items-center gap-3">
                <div className="h-px flex-1 bg-app-surface" />
                <span className="text-xs uppercase tracking-wider text-app-text-muted">
                    or paste manually
                </span>
                <div className="h-px flex-1 bg-app-surface" />
            </div>

            <div className="flex flex-col gap-2">
                <label className="text-sm font-medium text-app-text">
                    Host id
                </label>
                <input
                    value={peerId}
                    onChange={(e) => setPeerId(e.target.value)}
                    placeholder="64 hex characters"
                    autoCapitalize="none"
                    autoCorrect="off"
                    spellCheck={false}
                    className="glass-card w-full rounded-2xl px-4 py-3 font-mono text-[13px] text-app-text placeholder:text-app-text-muted focus:outline-none focus:ring-2 focus:ring-app-accent/60"
                />
                <button
                    onClick={onManualSubmit}
                    disabled={!canSubmit}
                    className="glass-pill mt-2 flex h-11 w-full items-center justify-center rounded-2xl text-[14px] font-medium text-app-text disabled:opacity-40"
                >
                    Connect with host id
                </button>
            </div>
        </div>
    );
}

// ── Approving ────────────────────────────────────────────────────────

function ApprovingView({
    onCancel,
    deviceLabel,
}: {
    onCancel: () => void;
    deviceLabel: string;
}) {
    return (
        <div className="mx-auto flex h-full max-w-md flex-col px-5 py-7">
            <div className="flex flex-1 flex-col items-center justify-center gap-6 text-center">
                <PairingModalIllustration
                    className="w-full max-w-[340px]"
                    deviceLabel={deviceLabel}
                />
                <div className="flex flex-col gap-2">
                    <h2 className="text-base font-semibold text-app-text">
                        Approve on your desktop
                    </h2>
                    <p className="max-w-sm text-sm leading-relaxed text-app-text-secondary">
                        A pairing dialog just appeared in SuperHQ on your
                        desktop. Click{" "}
                        <span className="font-medium text-app-text">
                            Approve
                        </span>{" "}
                        there to continue.
                    </p>
                </div>
                <div className="flex items-center gap-2 text-sm text-app-text-muted">
                    <span className="h-2 w-2 animate-pulse rounded-full bg-app-accent" />
                    Waiting for approval…
                </div>
            </div>
            <button
                onClick={onCancel}
                className="glass-pill mt-4 flex h-11 w-full items-center justify-center rounded-2xl text-[14px] text-app-text"
            >
                Cancel
            </button>
        </div>
    );
}

// ── Securing ─────────────────────────────────────────────────────────

function SecuringView() {
    return (
        <div className="mx-auto flex h-full max-w-md flex-col px-5 py-7">
            <div className="flex flex-1 flex-col items-center justify-center gap-6 text-center">
                <BiometricIllustration className="h-[180px] w-[180px]" />
                <div className="flex flex-col gap-2">
                    <h2 className="text-base font-semibold text-app-text">
                        Confirm with your device
                    </h2>
                    <p className="max-w-sm text-sm leading-relaxed text-app-text-secondary">
                        Your browser is asking to register a passkey. Accept
                        the prompt with Touch ID, Face ID, or your device
                        password to secure this connection.
                    </p>
                </div>
            </div>
        </div>
    );
}

// ── Error ────────────────────────────────────────────────────────────

function ErrorView({
    message,
    onRetry,
    onBack,
}: {
    message: string;
    onRetry: () => void;
    onBack: () => void;
}) {
    return (
        <div className="mx-auto flex h-full max-w-md items-center justify-center px-5 py-7">
            <div className="glass-card flex w-full flex-col items-center gap-4 rounded-[24px] p-6 text-center">
                <div className="glass-pill flex h-12 w-12 items-center justify-center rounded-full text-red-300">
                    <svg
                        width="22"
                        height="22"
                        viewBox="0 0 24 24"
                        fill="none"
                        stroke="currentColor"
                        strokeWidth={2.2}
                        strokeLinecap="round"
                        strokeLinejoin="round"
                    >
                        <circle cx="12" cy="12" r="10" />
                        <line x1="12" y1="8" x2="12" y2="12" />
                        <line x1="12" y1="16" x2="12.01" y2="16" />
                    </svg>
                </div>
                <div className="flex flex-col gap-1.5">
                    <h2 className="text-[17px] font-semibold tracking-[-0.01em] text-app-text">
                        Pairing didn’t complete
                    </h2>
                    <p className="max-w-sm text-[13.5px] leading-relaxed break-words text-app-text-secondary">
                        {message}
                    </p>
                </div>
                <div className="mt-1 flex w-full flex-col gap-2">
                    <button
                        onClick={onRetry}
                        className="glass-pill glass-pill--accent flex h-10 w-full items-center justify-center rounded-full text-[14px] font-medium text-white"
                    >
                        Try again
                    </button>
                    <button
                        onClick={onBack}
                        className="glass-pill flex h-10 w-full items-center justify-center rounded-full text-[14px] text-app-text"
                    >
                        Back
                    </button>
                </div>
            </div>
        </div>
    );
}
