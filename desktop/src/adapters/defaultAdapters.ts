import type { DesktopAdapters } from "./contracts";

/**
 * The scaffold exposes named, no-op implementations until product features
 * choose concrete component, graph and collection providers.
 */
export const defaultDesktopAdapters: DesktopAdapters = {
  components: { name: "aworkit-component-facade" },
  graph: { name: "aworkit-graph-facade" },
  collections: { name: "aworkit-collection-facade" },
  nativePresentation: {
    name: "tauri-native-presentation-facade",
    async notify(): Promise<void> {
      // Native notification wiring intentionally belongs to a future feature.
    },
  },
};
