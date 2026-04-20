// Full-screen QR scanner modal. Opens the rear camera, shows a live
// preview, and calls `onDetected` the first time a QR code resolves
// — the caller is responsible for closing the modal. Uses
// `qr-scanner` which wraps `BarcodeDetector` where available and
// falls back to a tiny WASM decoder elsewhere.

import { useEffect, useRef } from "react";
import QrScanner from "qr-scanner";

interface Props {
    onDetected: (payload: string) => void;
    onClose: () => void;
}

export default function QrScannerModal({ onDetected, onClose }: Props) {
    const videoRef = useRef<HTMLVideoElement | null>(null);
    const scannerRef = useRef<QrScanner | null>(null);
    const firedRef = useRef(false);
    const errorRef = useRef<HTMLDivElement | null>(null);

    useEffect(() => {
        const video = videoRef.current;
        if (!video) return;
        const scanner = new QrScanner(
            video,
            (result) => {
                if (firedRef.current) return;
                firedRef.current = true;
                onDetected(result.data.trim());
            },
            {
                preferredCamera: "environment",
                highlightScanRegion: true,
                highlightCodeOutline: true,
                returnDetailedScanResult: true,
            },
        );
        scannerRef.current = scanner;
        scanner.start().catch((err) => {
            if (errorRef.current) {
                errorRef.current.textContent =
                    err instanceof Error
                        ? err.message
                        : "Couldn't start the camera.";
            }
        });
        return () => {
            scanner.stop();
            scanner.destroy();
            scannerRef.current = null;
        };
    }, [onDetected]);

    return (
        <div className="fixed inset-0 z-50 flex flex-col bg-black">
            <div
                className="flex items-center justify-between px-4 py-3"
                style={{ paddingTop: "calc(env(safe-area-inset-top) + 12px)" }}
            >
                <div className="text-sm font-medium text-white">
                    Scan QR code
                </div>
                <button
                    onClick={onClose}
                    className="rounded-md px-3 py-1.5 text-sm text-white/80 active:bg-white/10"
                >
                    Close
                </button>
            </div>
            <div className="relative flex-1 overflow-hidden">
                <video
                    ref={videoRef}
                    className="h-full w-full object-cover"
                    playsInline
                    muted
                />
                <div
                    ref={errorRef}
                    className="pointer-events-none absolute inset-x-6 bottom-20 text-center text-sm text-red-300"
                />
            </div>
            <div
                className="px-6 py-4 text-center text-sm text-white/70"
                style={{
                    paddingBottom: "calc(env(safe-area-inset-bottom) + 16px)",
                }}
            >
                Point your camera at the QR code in the SuperHQ popover.
            </div>
        </div>
    );
}
