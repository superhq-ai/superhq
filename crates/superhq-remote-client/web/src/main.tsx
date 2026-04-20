import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { BrowserRouter } from "react-router";
import App from "./App";
import "./styles.css";

// Best-effort portrait lock. Browsers generally ignore this outside an
// installed PWA, so it's defence-in-depth, not load-bearing.
async function lockPortrait() {
    try {
        // @ts-expect-error — lock() typings lag in some TS lib versions.
        await screen.orientation?.lock?.("portrait-primary");
    } catch {
        /* ignore — non-standalone contexts always reject. */
    }
}
lockPortrait();

createRoot(document.getElementById("root")!).render(
    <StrictMode>
        <BrowserRouter>
            <App />
        </BrowserRouter>
    </StrictMode>,
);
