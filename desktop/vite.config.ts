import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import { configDefaults } from "vitest/config";

/** Vite hosts the unprivileged presentation client only. */
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: { port: 1420, strictPort: true },
  test: {
    setupFiles: ["./src/test/setup.ts"],
    // Wall-clock gates live in src/perf and run through `pnpm test:perf`.
    exclude: [...configDefaults.exclude, "src/perf/**"],
    // Every file here drives a full React app through jsdom, so a worker per
    // core oversubscribes the machine and turns cheap tests into 5s timeouts.
    // Half the cores measured both green and fastest: 454 tests in 96s, against
    // 103s with three load-dependent failures at the default worker count.
    maxWorkers: "50%",
  },
});
