// Thin wrapper around the umami global loaded from index.html's
// <script>. No-op if the script failed (ad-blocked, offline first
// load, etc.) so analytics can never take down the app.
//
// Payload discipline: only aggregatable metadata — event kind, mode,
// boolean flags. No workspace names, host ids, tab labels, pasted
// content, user strings. These all stay on the device.

type EventValue = string | number | boolean | undefined;
type EventPayload = Record<string, EventValue>;

interface UmamiGlobal {
    track: (event: string, data?: EventPayload) => void;
}

function umami(): UmamiGlobal | null {
    if (typeof window === "undefined") return null;
    const u = (window as unknown as { umami?: UmamiGlobal }).umami;
    return u ?? null;
}

export function track(event: string, data?: EventPayload): void {
    const u = umami();
    if (!u) return;
    try {
        u.track(event, data);
    } catch {
        /* swallow — analytics must never surface as an app error */
    }
}
