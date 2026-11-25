import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// Tauri expects a fixed port and no clearing of its own logs.
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: { port: 1420, strictPort: true, watch: { ignored: ["**/src-tauri/**"] } },
  envPrefix: ["VITE_", "TAURI_ENV_*"],
  build: { target: "es2022", sourcemap: false },
  test: { environment: "jsdom", include: ["src/**/*.test.{ts,tsx}"] },
});
