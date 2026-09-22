import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import { configDefaults } from "vitest/config";

/** The opt-in wall-clock gate suite: `pnpm test:perf`, never the default run. */
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  test: {
    setupFiles: ["./src/test/setup.ts"],
    include: ["src/perf/**/*.test.{ts,tsx}"],
    exclude: configDefaults.exclude,
  },
});
