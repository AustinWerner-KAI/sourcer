import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// In development, /api is proxied to the Rust server.
export default defineConfig({
  plugins: [react()],
  server: {
    port: 5173,
    proxy: { "/api": "http://127.0.0.1:8080" },
  },
});
