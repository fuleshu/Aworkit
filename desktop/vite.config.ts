import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

/** Vite hosts the unprivileged presentation client only. */
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: { port: 1420, strictPort: true },
});
