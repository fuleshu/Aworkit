import { invoke } from "@tauri-apps/api/core";

/**
 * Where the bundled example workflows and the FFmpeg plugin are written, and
 * whether that happens at all. The native host owns the OS documents folder;
 * the webview only ever receives this report.
 */
export interface DocumentsExtrasReport {
  readonly enabled: boolean;
  /** `<documents>/Aworkit`, or null when this system has no documents folder. */
  readonly folder: string | null;
  readonly exists: boolean;
  readonly exampleWorkflowCount: number;
  readonly pluginPresent: boolean;
  readonly writtenVersion: number;
}

function isNative(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

/** Current state, or null in a browser preview that has no native host. */
export async function documentsExtrasStatus(): Promise<DocumentsExtrasReport | null> {
  if (!isNative()) return null;
  return (await invoke("documents_extras_status")) as DocumentsExtrasReport;
}

/** Turns writing the extras on or off; turning it on writes the folder now. */
export async function setDocumentsExtrasEnabled(
  enabled: boolean,
): Promise<DocumentsExtrasReport | null> {
  if (!isNative()) return null;
  return (await invoke("documents_extras_set_enabled", {
    enabled,
  })) as DocumentsExtrasReport;
}

/** Writes any missing example files and the plugin, and turns writing on. */
export async function writeDocumentsExtras(): Promise<DocumentsExtrasReport | null> {
  if (!isNative()) return null;
  return (await invoke("documents_extras_write_now")) as DocumentsExtrasReport;
}

/** Reveals `<documents>/Aworkit` in the platform file manager. */
export async function openDocumentsFolder(): Promise<void> {
  if (!isNative()) return;
  await invoke("documents_extras_open_folder");
}
