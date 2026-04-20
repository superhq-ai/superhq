// Apple-style bottom sheet — liquid-glass material, spring-present,
// draggable dismiss.
//
// Layout strategy: `fixed inset-0 flex flex-col`, the backdrop takes
// `flex-1` above, the sheet sits underneath pinned to the bottom by
// flexbox. No `absolute bottom-0` positioning — that interacted badly
// with `max-h` and caused the sheet to render flush to the top in some
// layouts.
//
// Present / dismiss:
//   • Opens with a translate-in animation from below + fade-in backdrop.
//   • Closes by: tap backdrop · drag the handle down past threshold ·
//     flick down fast · Escape key.
//
// Pointer handling is driven entirely by Pointer Events and
// `setPointerCapture`, so touch / mouse / stylus all work. A stray
// `pointercancel` or lost capture is treated as an abort — the sheet
// snaps back instead of getting stuck mid-drag.

import { useCallback, useEffect, useRef, useState } from "react";

interface Props {
    open: boolean;
    onClose: () => void;
    title?: string;
    children: React.ReactNode;
}

const DISMISS_DRAG_RATIO = 0.22;
const FLICK_VELOCITY = 0.5; // px/ms

export default function Sheet({ open, onClose, title, children }: Props) {
    const sheetRef = useRef<HTMLDivElement | null>(null);
    const dragRef = useRef<{
        startY: number;
        lastY: number;
        lastT: number;
        height: number;
        pointerId: number;
    } | null>(null);
    const [dragY, setDragY] = useState(0);
    const [mounted, setMounted] = useState(open);
    const [visible, setVisible] = useState(false);

    // Mount the DOM first, then flip `visible` on the next frame so
    // CSS transitions can pick up the enter animation. On close we do
    // the reverse — flip visible off, wait for transition, unmount.
    useEffect(() => {
        if (open) {
            setMounted(true);
            const id = window.requestAnimationFrame(() => setVisible(true));
            return () => window.cancelAnimationFrame(id);
        }
        setVisible(false);
        const id = window.setTimeout(() => setMounted(false), 400);
        return () => window.clearTimeout(id);
    }, [open]);

    // Reset any stale drag state + re-enable transitions whenever we
    // open fresh — protects against a bug where a previous drag left
    // the sheet's inline `transition: none` in place.
    useEffect(() => {
        if (!open) return;
        setDragY(0);
        const el = sheetRef.current;
        if (el) el.style.transition = "";
    }, [open]);

    // Escape dismiss.
    useEffect(() => {
        if (!open) return;
        const onKey = (e: KeyboardEvent) => {
            if (e.key === "Escape") onClose();
        };
        window.addEventListener("keydown", onKey);
        return () => window.removeEventListener("keydown", onKey);
    }, [open, onClose]);

    const onHandlePointerDown = useCallback((e: React.PointerEvent) => {
        const el = sheetRef.current;
        if (!el) return;
        e.preventDefault();
        (e.currentTarget as Element).setPointerCapture(e.pointerId);
        dragRef.current = {
            startY: e.clientY,
            lastY: e.clientY,
            lastT: e.timeStamp,
            height: el.getBoundingClientRect().height,
            pointerId: e.pointerId,
        };
        el.style.transition = "none";
    }, []);

    const onHandlePointerMove = useCallback((e: React.PointerEvent) => {
        const st = dragRef.current;
        if (!st || st.pointerId !== e.pointerId) return;
        const dy = Math.max(0, e.clientY - st.startY);
        st.lastY = e.clientY;
        st.lastT = e.timeStamp;
        setDragY(dy);
    }, []);

    const endDrag = useCallback(
        (dismiss: boolean) => {
            const el = sheetRef.current;
            if (el) el.style.transition = "";
            dragRef.current = null;
            if (dismiss) {
                onClose();
            } else {
                setDragY(0);
            }
        },
        [onClose],
    );

    const onHandlePointerUp = useCallback(
        (e: React.PointerEvent) => {
            const st = dragRef.current;
            if (!st || st.pointerId !== e.pointerId) return;
            const dy = Math.max(0, e.clientY - st.startY);
            const dt = Math.max(1, e.timeStamp - st.lastT);
            const velocity = (e.clientY - st.lastY) / dt;
            const dismiss =
                dy > st.height * DISMISS_DRAG_RATIO ||
                velocity > FLICK_VELOCITY;
            endDrag(dismiss);
        },
        [endDrag],
    );

    const onHandlePointerCancel = useCallback(() => {
        if (dragRef.current) endDrag(false);
    }, [endDrag]);

    if (!mounted) return null;

    const sheetTransform = visible
        ? `translate3d(0, ${dragY}px, 0)`
        : "translate3d(0, 100%, 0)";

    return (
        <div
            className="fixed inset-0 z-40 flex flex-col"
            role="dialog"
            aria-modal="true"
        >
            {/* Backdrop — takes all space above the sheet, tap to close. */}
            <div
                onPointerDown={onClose}
                className="flex-1 bg-black/45 backdrop-blur-sm transition-opacity"
                style={{
                    opacity: visible ? 1 : 0,
                    transitionDuration: "380ms",
                    transitionTimingFunction:
                        "cubic-bezier(0.32, 0.72, 0, 1)",
                }}
            />
            {/* Sheet itself — flex child, pinned to the bottom of the
             * column. translateY does the present animation. */}
            <div
                ref={sheetRef}
                className="glass-sheet max-h-[85vh] w-full overflow-hidden rounded-t-[28px]"
                style={{
                    transform: sheetTransform,
                    transitionProperty: "transform",
                    transitionDuration: "400ms",
                    transitionTimingFunction: "cubic-bezier(0.32, 0.72, 0, 1)",
                    paddingBottom: "env(safe-area-inset-bottom)",
                    willChange: "transform",
                }}
            >
                <div
                    className="flex flex-col items-center pt-2.5 pb-1 select-none"
                    style={{ touchAction: "none", cursor: "grab" }}
                    onPointerDown={onHandlePointerDown}
                    onPointerMove={onHandlePointerMove}
                    onPointerUp={onHandlePointerUp}
                    onPointerCancel={onHandlePointerCancel}
                    onLostPointerCapture={onHandlePointerCancel}
                >
                    <div className="h-1 w-10 rounded-full bg-white/20" />
                    {title ? (
                        <div className="mt-2 text-[13px] font-semibold tracking-[-0.005em] text-app-text">
                            {title}
                        </div>
                    ) : null}
                </div>
                <div className="max-h-[calc(85vh-44px)] overflow-y-auto px-3 pt-1 pb-3">
                    {children}
                </div>
            </div>
        </div>
    );
}
