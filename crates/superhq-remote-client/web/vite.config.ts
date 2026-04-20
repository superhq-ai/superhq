import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import { VitePWA } from "vite-plugin-pwa";
import { viteStaticCopy } from "vite-plugin-static-copy";
import { resolve } from "node:path";

// The WASM bundle lives at `crates/superhq-remote-client/pkg/` and is
// produced by `demo/build.sh`. We copy it verbatim into the web build
// so the client can `import init from "/pkg/superhq_remote_client.js"`
// at runtime — keeping the same import path the vanilla demo uses.
const WASM_PKG = resolve(__dirname, "../pkg");

// Icons are the SAME stroke-based Feather set the desktop app uses.
// We copy them into the web build's /icons/ so every <svg path="…">
// resolves to the identical artwork.
const DESKTOP_ICONS = resolve(__dirname, "../../../assets/icons");

// App icon — reused from the desktop build at build time.
const DESKTOP_APP_ICON = resolve(
    __dirname,
    "../../../assets/app-icon-128.png",
);

export default defineConfig({
    resolve: {
        alias: {
            "@": resolve(__dirname, "src"),
        },
    },
    plugins: [
        react(),
        tailwindcss(),
        viteStaticCopy({
            targets: [
                { src: `${WASM_PKG}/*`, dest: "pkg" },
                { src: `${DESKTOP_ICONS}/*`, dest: "icons" },
                { src: DESKTOP_APP_ICON, dest: "." },
            ],
        }),
        VitePWA({
            registerType: "autoUpdate",
            includeAssets: ["app-icon-128.png", "icons/*.svg"],
            // Keep the service worker active in `vite dev` too so the
            // runtime-cache rules below actually do something during
            // development (by default the SW only ships in prod).
            devOptions: {
                enabled: true,
                type: "module",
            },
            manifest: {
                name: "SuperHQ Remote",
                short_name: "SuperHQ",
                description:
                    "Remote control for a SuperHQ host — connect to your agents from anywhere.",
                theme_color: "#0b0b0c",
                background_color: "#0b0b0c",
                display: "standalone",
                orientation: "portrait-primary",
                start_url: "/",
                scope: "/",
                icons: [
                    {
                        src: "app-icon-128.png",
                        sizes: "128x128",
                        type: "image/png",
                        purpose: "any",
                    },
                    {
                        src: "app-icon-128.png",
                        sizes: "128x128",
                        type: "image/png",
                        purpose: "maskable",
                    },
                ],
            },
            workbox: {
                // WASM + the pkg/ JS glue are static and expensive to
                // re-download; aggressively cache them.
                globPatterns: ["**/*.{js,css,html,png,svg,wasm}"],
                maximumFileSizeToCacheInBytes: 12 * 1024 * 1024,
                runtimeCaching: [
                    // GitHub avatars — `github.com/{owner}.png` 302-redirects
                    // to `avatars.githubusercontent.com`. Cache both URLs so
                    // navigating Home → Workspace → Home doesn't re-fetch.
                    // 7-day TTL + LRU eviction gives GitHub room to update
                    // avatars without us pinning stale art forever.
                    {
                        urlPattern: ({ url }) =>
                            url.hostname === "github.com" &&
                            url.pathname.endsWith(".png"),
                        handler: "CacheFirst",
                        options: {
                            cacheName: "github-avatars",
                            expiration: {
                                maxEntries: 64,
                                maxAgeSeconds: 60 * 60 * 24 * 7,
                            },
                            cacheableResponse: {
                                statuses: [0, 200],
                            },
                        },
                    },
                    {
                        urlPattern: ({ url }) =>
                            url.hostname === "avatars.githubusercontent.com",
                        handler: "CacheFirst",
                        options: {
                            cacheName: "github-avatars",
                            expiration: {
                                maxEntries: 64,
                                maxAgeSeconds: 60 * 60 * 24 * 7,
                            },
                            cacheableResponse: {
                                statuses: [0, 200],
                            },
                        },
                    },
                ],
            },
        }),
    ],
    server: {
        port: 5173,
        host: true,
    },
});
