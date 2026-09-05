import { defineConfig, mergeConfig } from "vite";
import base from "./vite.config";

// Sandbox/remote-preview only. Same as vite.config.ts, but accepts any Host
// header so the app can be opened through a proxied preview URL.
// Run with: npx vite --config vite.config.sandbox.ts
export default defineConfig(async (env) => {
  const resolved = typeof base === "function" ? await base(env) : base;
  return mergeConfig(resolved, {
    server: {
      host: "0.0.0.0",
      port: 5173,
      allowedHosts: true,
    },
  });
});
