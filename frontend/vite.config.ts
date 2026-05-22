import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// Vite config for the studio frontend.
//
// `server.host` is pinned to 127.0.0.1 to match CLAUDE.md's documented dev
// origin (so the backend's permissive CORS matches a known surface). Override
// with `--host 0.0.0.0` from the CLI when sharing a dev server on a LAN.
export default defineConfig({
  plugins: [react()],
  server: {
    host: "127.0.0.1",
    port: 5173,
    strictPort: true,
  },
});
