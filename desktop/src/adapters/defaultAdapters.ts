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
      if (document.hidden) await invokeNative("native_notify", { title, body });
    },
    async confirm(title, body): Promise<boolean> {
      const native = await invokeNativeResult<boolean>("native_confirm", {
        title,
        body,
      });
      if (native.available) return native.value;
      return await new Promise((resolve) =>
        dispatch({ kind: "confirmation", title, body, resolve }),
      );
    },
    async message(title, body): Promise<void> {
      dispatch({ kind: "notification", title, body });
    },
    async pickFile(): Promise<string | null> {
      const native = await invokeNativeResult<string | null>("native_pick_file");
      return native.available ? native.value : null;
    },
    async pickFolder(): Promise<string | null> {
      const native = await invokeNativeResult<string | null>("native_pick_folder");
      return native.available ? native.value : null;
    },
  },
};

async function invokeNative(
  command: string,
  args?: Record<string, unknown>,
): Promise<boolean> {
  const result = await invokeNativeResult<unknown>(command, args);
  return result.available;
}

async function invokeNativeResult<T>(
  command: string,
  args?: Record<string, unknown>,
): Promise<
  | { readonly available: true; readonly value: T }
  | { readonly available: false }
> {
  if (!("__TAURI_INTERNALS__" in window)) return { available: false };
  try {
    const { invoke } = await import("@tauri-apps/api/core");
    return { available: true, value: await invoke<T>(command, args) };
  } catch {
    return { available: false };
  }
}

function dispatch(request: NativePresentationRequest): void {
  window.dispatchEvent(
    new CustomEvent<NativePresentationRequest>(nativePresentationEvent, {
      detail: request,
    }),
  );
}
