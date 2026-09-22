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
    // Wall-clock gates only mean something when nothing else is running: with
    // parallel files the frame gate measured a sibling suite's CPU load (83ms
    // for a 16ms budget) instead of its own work.
    fileParallelism: false,
  },
});
