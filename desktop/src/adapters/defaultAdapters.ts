import {
  nativePresentationEvent,
  type DesktopAdapters,
  type NativePresentationRequest,
} from "./contracts";

/** Browser-safe presentation fallback; a later platform milestone may replace
 * these prompts with native OS surfaces without changing feature contracts. */
export const defaultDesktopAdapters: DesktopAdapters = {
  components: { name: "aworkit-component-facade" },
  graph: { name: "aworkit-graph-facade" },
  collections: { name: "aworkit-collection-facade" },
  nativePresentation: {
    name: "tauri-native-presentation-facade",
    async notify(title, body): Promise<void> {
      dispatch({ kind: "notification", title, body });
    },
    async confirm(title, body): Promise<boolean> {
      return await new Promise((resolve) =>
        dispatch({ kind: "confirmation", title, body, resolve }),
      );
    },
  },
};

function dispatch(request: NativePresentationRequest): void {
  window.dispatchEvent(
    new CustomEvent<NativePresentationRequest>(nativePresentationEvent, {
      detail: request,
    }),
  );
}
