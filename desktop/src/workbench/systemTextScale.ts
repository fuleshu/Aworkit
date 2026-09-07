/** OS accessibility text size is independent of both app preferences and monitor DPI.
 * Tauri/WebView already scales CSS pixels for monitor DPI; never multiply it here.
 */
import { projectNativeMenuTypography } from "./nativeMenuTypography";

export function projectSystemTextScale(value: number): void {
  const scale = Number.isFinite(value) && value > 0 ? value : 1;
  document.documentElement.style.setProperty("--aw-system-text-scale", String(scale));
  projectNativeMenuTypography();
}

/** Subscribe before reading so a setting change cannot be lost during startup. */
export async function initializeSystemTextScale(): Promise<() => void> {
  if (!("__TAURI_INTERNALS__" in window)) return () => {};
  const [{ invoke }, { listen }] = await Promise.all([
    import("@tauri-apps/api/core"),
    import("@tauri-apps/api/event"),
  ]);
  let revision = 0;
  const unlisten = await listen<number>("aworkit:system-text-scale", ({ payload }) => {
    revision++;
    projectSystemTextScale(payload);
  });
  const initialRevision = revision;
  try {
    const scale = await invoke<number>("native_system_text_scale");
    if (revision === initialRevision) projectSystemTextScale(scale);
  } catch (error) {
    unlisten();
    throw error;
  }
  return unlisten;
}
