// "A new version is available" banner.
//
// vite-plugin-pwa runs with `registerType: "autoUpdate"`, so a new
// service worker is downloaded and cached in the background as soon
// as a new build is deployed. `useRegisterSW` from the
// `virtual:pwa-register/react` module gives us the reactive
// `needRefresh` flag: it flips to true once the new worker has
// finished installing and is waiting to take over.
//
// The banner sits above every route, pinned to the top safe-area.
// Tapping "Reload" calls `updateServiceWorker(true)` which activates
// the waiting worker and reloads the page; "Later" dismisses the
// banner and the update silently applies on the next cold launch.

import { useRegisterSW } from "virtual:pwa-register/react";
import { track } from "../lib/analytics";

export default function PWAUpdatePrompt() {
    const {
        needRefresh: [needRefresh, setNeedRefresh],
        updateServiceWorker,
    } = useRegisterSW({
        onRegisteredSW(_swUrl, registration) {
            // Re-check every 30 minutes for a new build; Workbox's
            // default is not automatic beyond install-time.
            if (!registration) return;
            const THIRTY_MIN = 30 * 60 * 1000;
            setInterval(() => {
                void registration.update();
            }, THIRTY_MIN);
        },
    });

    if (!needRefresh) return null;

    return (
        <div
            className="pointer-events-none fixed inset-x-0 z-50 flex justify-center px-3"
            style={{ top: "calc(env(safe-area-inset-top) + 8px)" }}
            aria-live="polite"
        >
            <div className="glass-sheet pointer-events-auto flex items-center gap-2 rounded-full py-1.5 pr-1.5 pl-4 text-[13px] shadow-lg">
                <span className="text-app-text">Update available</span>
                <button
                    onClick={() => {
                        track("pwa.update.apply");
                        void updateServiceWorker(true);
                    }}
                    className="glass-pill glass-pill--accent rounded-full px-3 py-1.5 text-[12.5px] font-medium text-white"
                >
                    Reload
                </button>
                <button
                    onClick={() => setNeedRefresh(false)}
                    aria-label="Dismiss"
                    className="flex h-7 w-7 items-center justify-center rounded-full text-app-text-muted active:bg-white/10"
                >
                    <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth={2.4} strokeLinecap="round" strokeLinejoin="round">
                        <line x1="18" y1="6" x2="6" y2="18" />
                        <line x1="6" y1="6" x2="18" y2="18" />
                    </svg>
                </button>
            </div>
        </div>
    );
}
