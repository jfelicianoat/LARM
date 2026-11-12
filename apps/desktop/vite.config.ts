import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// https://vite.dev/config/
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  // src-tauri se recompila aparte: vigilarlo provoca EBUSY sobre los binarios bloqueados.
  server: { port: 5173, strictPort: true, watch: { ignored: ["**/src-tauri/**"] } },
  envPrefix: ["VITE_", "TAURI_"],
  build: { target: "chrome105", outDir: "dist" },
});
