// Minimal toast system — one transient message at the bottom of the
// screen. The component reads from the connection store and auto-
// clears after `TOAST_DURATION_MS`. Call `showToast(...)` from
// anywhere to push a new message; the newer one replaces any in-
// flight toast (which is what you want 90% of the time on mobile —
// stacking toasts feels chatty).

import { useEffect } from "react";
import { useConnectionStore } from "../state/store";

const TOAST_DURATION_MS = 1800;

export default function Toast() {
    const toast = useConnectionStore((s) => s.toast);
    const clearToast = useConnectionStore((s) => s.clearToast);

    useEffect(() => {
        if (!toast) return;
        const id = window.setTimeout(() => clearToast(), TOAST_DURATION_MS);
        return () => window.clearTimeout(id);
    }, [toast, clearToast]);

    if (!toast) return null;

    const toneClasses =
        toast.tone === "error"
            ? "bg-red-500/15 text-red-200 ring-1 ring-red-500/30"
            : "bg-app-surface-3 text-app-text ring-1 ring-white/10";

    return (
        <div
            className="pointer-events-none fixed inset-x-0 z-50 flex justify-center px-4"
            style={{
                bottom: "calc(env(safe-area-inset-bottom) + 16px)",
            }}
            aria-live="polite"
        >
            <div
                className={[
                    "pointer-events-auto flex max-w-sm items-center gap-2 rounded-full px-4 py-2 text-[13px] font-medium backdrop-blur-md",
                    toneClasses,
                ].join(" ")}
            >
                {toast.message}
            </div>
        </div>
    );
}
