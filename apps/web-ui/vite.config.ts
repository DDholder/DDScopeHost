import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

const host = process.env.TAURI_DEV_HOST;

export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: {
    port: 5173,
    strictPort: true,
    host: host || false,
    hmr: host
      ? {
          protocol: "ws",
          host,
          port: 1421,
        }
      : undefined,
    watch: {
      ignored: ["**/apps/desktop/**", "**/crates/**", "**/target/**"],
    },
  },
  envPrefix: ["VITE_", "TAURI_ENV_*"],
  build: {
    rollupOptions: {
      output: {
        manualChunks(id) {
          const norm = id.replaceAll("\\", "/");
          if (!norm.includes("/node_modules/")) return undefined;
          if (/\/node_modules\/(dockview|dockview-react|dockview-core)\//.test(norm)) {
            return "vendor-dockview";
          }
          if (/\/node_modules\/(react|react-dom|scheduler|zustand)\//.test(norm)) {
            return "vendor-react";
          }
          return undefined;
        },
      },
    },
  },
});
