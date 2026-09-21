import { invoke } from "@tauri-apps/api/core";

/**
 * Global presentation preference for delegated-subagent conversation tabs.
 * It is a user preference, never part of a Chat's frozen tool contract.
 */
export interface SubagentViewPreference {
  /** Open a newly created child's tab in the background without focusing it. */
  readonly autoOpen: boolean;
  /** Close a terminal child's tab when it is open and not the active tab. */
  readonly autoClose: boolean;
}

/** The preference plus the Settings document version it was projected at. */
export interface SubagentViewPreferenceSnapshot extends SubagentViewPreference {
  readonly version: number;
}

export const DEFAULT_SUBAGENT_VIEW: SubagentViewPreferenceSnapshot = {
  autoOpen: true,
  autoClose: false,
  version: 0,
};

export interface SubagentViewPreferencePort {
  snapshot(): Promise<SubagentViewPreferenceSnapshot>;
  /** Commits the whole preference at the version this caller last projected. */
  commit(
    preference: SubagentViewPreference,
    expectedVersion: number,
  ): Promise<void>;
}

function isNative(): boolean {
  return "__TAURI_INTERNALS__" in window;
}

function boolean(value: unknown, fallback: boolean): boolean {
  return typeof value === "boolean" ? value : fallback;
}

/** Native preference port backed by the dedicated version-checked command. */
export class TauriSubagentViewPreferencePort
  implements SubagentViewPreferencePort
{
  public async snapshot(): Promise<SubagentViewPreferenceSnapshot> {
    if (!isNative()) return DEFAULT_SUBAGENT_VIEW;
    const value = (await invoke("desktop_subagent_view")) as {
      autoOpen?: unknown;
      autoClose?: unknown;
      version?: unknown;
    };
    return {
      autoOpen: boolean(value.autoOpen, DEFAULT_SUBAGENT_VIEW.autoOpen),
      autoClose: boolean(value.autoClose, DEFAULT_SUBAGENT_VIEW.autoClose),
      version: typeof value.version === "number" ? value.version : 0,
    };
  }

  public async commit(
    preference: SubagentViewPreference,
    expectedVersion: number,
  ): Promise<void> {
    if (!isNative()) return;
    await invoke("desktop_subagent_view_commit", {
      autoOpen: preference.autoOpen,
      autoClose: preference.autoClose,
      expectedVersion,
    });
  }
}

export function createSubagentViewPreferencePort(): SubagentViewPreferencePort {
  return new TauriSubagentViewPreferencePort();
}
