// Settings as a bottom sheet that replaces the old `/settings` route.
// The host id + paired-on metadata that used to clutter the home
// header lives here now.

import { useNavigate } from "react-router";
import Sheet from "./Sheet";
import { useConnectionStore } from "../state/store";
import { clearCredential } from "../lib/storage";

export default function SettingsSheet({
    open,
    onClose,
}: {
    open: boolean;
    onClose: () => void;
}) {
    const navigate = useNavigate();
    const host = useConnectionStore((s) => s.pairedHost);
    const forget = useConnectionStore((s) => s.forgetHost);
    const showToast = useConnectionStore((s) => s.showToast);

    async function onForget() {
        if (host) await clearCredential(host.peerId);
        forget();
        onClose();
        navigate("/pair", { replace: true });
    }

    function onCopy() {
        if (!host) return;
        navigator.clipboard?.writeText(host.peerId);
        showToast("Host id copied");
    }

    return (
        <Sheet open={open} onClose={onClose} title="Settings">
            <div className="flex flex-col gap-3 pt-2">
                <div className="text-[11px] font-semibold uppercase tracking-wider text-app-text-muted">
                    Paired host
                </div>
                <div className="glass-card rounded-[20px] p-4">
                    <div className="text-[13px] font-medium text-app-text">
                        {host?.label ?? "—"}
                    </div>
                    <div className="mt-2 font-mono text-[11.5px] break-all text-app-text-secondary">
                        {host?.peerId ?? "—"}
                    </div>
                    {host ? (
                        <div className="mt-3 text-[12px] text-app-text-muted">
                            Paired {new Date(host.pairedAt).toLocaleString()}
                        </div>
                    ) : null}
                </div>
                <button
                    onClick={onCopy}
                    className="glass-pill flex h-11 w-full items-center justify-center rounded-2xl text-[14px] font-medium text-app-text"
                >
                    Copy host id
                </button>
                <button
                    onClick={onForget}
                    className="flex h-11 w-full items-center justify-center rounded-2xl bg-red-500/12 text-[14px] font-medium text-app-error transition-colors active:bg-red-500/20"
                >
                    Forget this host on this device
                </button>
                <div className="mt-2 text-center text-[11px] text-app-text-ghost">
                    SuperHQ Remote — v0.1.0
                </div>
            </div>
        </Sheet>
    );
}
