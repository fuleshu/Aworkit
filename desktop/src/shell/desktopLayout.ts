import { invoke } from "@tauri-apps/api/core";

/** Persisted desktop window placement and panel separators. */
export interface DesktopLayout {
  readonly x?: number;
  readonly y?: number;
  readonly width?: number;
  readonly height?: number;
  readonly historyPaneWidth?: number;
  readonly inspectorPaneWidth?: number;
  readonly scaleFactor?: number;
}

/** The outer window frame measured natively, never a renderer estimate. */
export interface CapturedFrame {
  readonly x: number;
  readonly y: number;
  readonly width: number;
  readonly height: number;
  readonly scaleFactor: number;
}

export interface DesktopLayoutPort {
  snapshot(): Promise<DesktopLayout>;
  commit(
    frame: CapturedFrame | null,
    panes: { historyPaneWidth?: number; inspectorPaneWidth?: number },
  ): Promise<void>;
}

function isNative(): boolean {
  return "__TAURI_INTERNALS__" in window;
}

/** Reads and records the desktop window frame and panel separators. */
export class TauriDesktopLayoutPort implements DesktopLayoutPort {
  public async snapshot(): Promise<DesktopLayout> {
    if (!isNative()) return {};
    return (await invoke("desktop_layout")) as DesktopLayout;
  }

  public async commit(
    frame: CapturedFrame | null,
    panes: { historyPaneWidth?: number; inspectorPaneWidth?: number },
  ): Promise<void> {
    if (!isNative()) return;
    await invoke("desktop_layout_commit", {
      frame,
      historyPaneWidth: panes.historyPaneWidth ?? null,
      inspectorPaneWidth: panes.inspectorPaneWidth ?? null,
    });
  }
}

/**
 * Measures the main window's outer frame in physical pixels.
 *
 * `outerPosition`/`outerSize` are the frame the user actually positioned and
 * resized, so capturing them lets startup re-seat the same window. Reading the
 * inner size instead would lose the border and title bar, and the window would
 * drift smaller on every restart.
 *
 * A minimized window reports a sentinel position and a maximized or fullscreen
 * window reports the screen it covers; capturing either would re-seat a normal
 * window somewhere the user never put it, so those states capture nothing and
 * the last real placement is kept.
 */
export async function captureWindowFrame(): Promise<CapturedFrame | null> {
  if (!isNative()) return null;
  try {
    const { getCurrentWindow } = await import("@tauri-apps/api/window");
    const window = getCurrentWindow();
    const [minimized, maximized, fullscreen] = await Promise.all([
      window.isMinimized(),
      window.isMaximized(),
      window.isFullscreen(),
    ]);
    if (minimized || maximized || fullscreen) return null;
    const [position, size, scaleFactor] = await Promise.all([
      window.outerPosition(),
      window.outerSize(),
      window.scaleFactor(),
    ]);
    return {
      x: position.x,
      y: position.y,
      width: size.width,
      height: size.height,
      scaleFactor,
    };
  } catch {
    return null;
  }
}

export function createDesktopLayoutPort(): DesktopLayoutPort {
  return new TauriDesktopLayoutPort();
}
