import { invoke } from "@tauri-apps/api/core";

/** Outer frame in physical pixels; separator preferences in CSS pixels. */
export interface DesktopLayout {
  readonly x?: number;
  readonly y?: number;
  readonly width?: number;
  readonly height?: number;
  readonly historyPaneWidth?: number;
  readonly inspectorPaneWidth?: number;
  readonly scaleFactor?: number;
  readonly maximized?: boolean;
}

export type PaneWidths = Pick<DesktopLayout, "historyPaneWidth" | "inspectorPaneWidth">;

export interface DesktopLayoutPort {
  snapshot(): Promise<DesktopLayout>;
  commit(panes: PaneWidths): Promise<void>;
}

function isNative(): boolean {
  return "__TAURI_INTERNALS__" in window;
}

/** The host owns frame capture and the awaited native close lifecycle. */
export class TauriDesktopLayoutPort implements DesktopLayoutPort {
  public async snapshot(): Promise<DesktopLayout> {
    if (!isNative()) return {};
    return (await invoke("desktop_layout")) as DesktopLayout;
  }

  public async commit(panes: PaneWidths): Promise<void> {
    if (!isNative()) return;
    // Pointer coordinates are fractional at non-integral DPI/zoom. The native
    // schema uses u32; sending a fraction rejects the whole separator update.
    await invoke("desktop_layout_commit", {
      historyPaneWidth: panes.historyPaneWidth === undefined ? null : Math.round(panes.historyPaneWidth),
      inspectorPaneWidth: panes.inspectorPaneWidth === undefined ? null : Math.round(panes.inspectorPaneWidth),
    });
  }
}

export function createDesktopLayoutPort(): DesktopLayoutPort {
  return new TauriDesktopLayoutPort();
}
