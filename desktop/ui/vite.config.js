import { defineConfig } from "vite";
import { svelte } from "@sveltejs/vite-plugin-svelte";

// Tauri expects a fixed dev port and a modern target (WKWebView / WebView2).
// The gateway is proxied in dev so `npm run dev` talks to the real API on the
// same origin, exactly like the built bundle does when axum serves it.
const GATEWAY = process.env.VITE_GATEWAY_URL ?? "http://127.0.0.1:20128";

export default defineConfig({
  plugins: [svelte()],
  clearScreen: false,
  server: {
    port: 5173,
    strictPort: true,
    proxy: Object.fromEntries(
      ["/api", "/v1", "/healthz"].map((path) => [path, { target: GATEWAY, changeOrigin: true }])
    ),
  },
  build: {
    target: "esnext",
    emptyOutDir: true,
  },
});
