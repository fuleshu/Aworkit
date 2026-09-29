import { describe, expect, it, vi } from "vitest";
import {
  folderLeaf,
  projectFromDraft,
  saveProject,
} from "./createProject";
import type { ProjectConfiguration } from "../workbench/configuration";
import type {
  SettingsV2Commit,
  SettingsV2CorePort,
  SettingsV2Receipt,
} from "../workbench/settingsV2Port";
import { nativeToolDefaults } from "../workbench/toolRegistry";

/** The smallest settings document the port contract needs for these cases. */
function snapshot(projects: readonly ProjectConfiguration[], version: number) {
  return {
    version,
    schemaVersion: 2,
    settings: {
      approvals: { defaultMode: "ask_for_approval" },
      schemaVersion: 2,
      providers: [],
      modelTiers: [],
      credentials: [],
      tools: nativeToolDefaults(),
      extensions: [],
      mcpServers: [],
      externalAgents: [],
      data: {
        portableHistoryEnabled: false,
        detailedCaptureEnabled: false,
        portableDirectory: ".aworkit/sessions",
      },
      projects: [...projects],
      appearance: { mode: "system", fontScale: 1 },
      chatDefaults: {},
      layout: {},
    },
    providerHealth: [],
  } as unknown as Awaited<ReturnType<SettingsV2CorePort["snapshot"]>>;
}

const project = projectFromDraft(
  { name: "Repo", kind: "local_directory", location: "/work/repo" },
  "project.abc",
);

describe("project creation from the Chat", () => {
  it("names the project after the chosen folder, for every path flavour", () => {
    expect(folderLeaf("/home/me/Projects/repo")).toBe("repo");
    expect(folderLeaf("/home/me/Projects/repo/")).toBe("repo");
    expect(folderLeaf("C:\\work\\repo")).toBe("repo");
    expect(folderLeaf("C:\\work\\repo\\")).toBe("repo");
    // A root or a trailing-only path has no usable name.
    expect(folderLeaf("/")).toBe("Project");
    expect(folderLeaf("   ")).toBe("Project");
  });

  it("builds the same record Settings → Projects stores", () => {
    expect(project).toEqual({
      id: "project.abc",
      name: "Repo",
      workspace: { kind: "local_directory", location: "/work/repo" },
      defaultWorkflowId: null,
      portableHistoryEnabled: false,
    });
    expect(() =>
      projectFromDraft({ name: "  ", kind: "git_worktree", location: "/w" }),
    ).toThrow();
    expect(() =>
      projectFromDraft({ name: "Repo", kind: "local_directory", location: "" }),
    ).toThrow();
  });

  it("appends the project under the observed settings version", async () => {
    const commit = vi.fn(async (_command: SettingsV2Commit) => ({
      commandId: "desktop.settings.1",
      accepted: true,
      currentVersion: 8,
      reason: null,
    }));
    const port = {
      snapshot: vi.fn(async () => snapshot([], 7)),
      commit,
    } as unknown as Pick<SettingsV2CorePort, "snapshot" | "commit">;

    await saveProject(project, { port, nextCommandId: () => "desktop.settings.1" });

    expect(commit).toHaveBeenCalledOnce();
    const command = commit.mock.calls[0]![0] as SettingsV2Commit;
    expect(command.expectedVersion).toBe(7);
    expect(command.settings.projects).toEqual([project]);
  });

  it("re-reads and retries once when a concurrent save moved the version", async () => {
    const commit = vi
      .fn<(command: SettingsV2Commit) => Promise<SettingsV2Receipt>>()
      .mockResolvedValueOnce({
        commandId: "desktop.settings.1",
        accepted: false,
        currentVersion: 7,
        reason: "settings version conflict: expected 7, actual 8",
      })
      .mockResolvedValueOnce({
        commandId: "desktop.settings.2",
        accepted: true,
        currentVersion: 10,
        reason: null,
      });
    const snapshotMock = vi
      .fn()
      .mockResolvedValueOnce(snapshot([], 7))
      .mockResolvedValueOnce(snapshot([], 9));
    const ids = ["desktop.settings.1", "desktop.settings.2"];
    const port = {
      snapshot: snapshotMock,
      commit,
    } as unknown as Pick<SettingsV2CorePort, "snapshot" | "commit">;

    await saveProject(project, {
      port,
      nextCommandId: () => ids.shift()!,
    });

    expect(commit).toHaveBeenCalledTimes(2);
    // The retry never reuses the first command id with a different body.
    expect((commit.mock.calls[1]![0] as SettingsV2Commit).expectedVersion).toBe(9);
  });

  it("reports a refused save instead of silently dropping the project", async () => {
    const port = {
      snapshot: vi.fn(async () => snapshot([], 7)),
      commit: vi.fn(async () => {
        throw new Error("desktop runtime lock is unavailable");
      }),
    } as unknown as Pick<SettingsV2CorePort, "snapshot" | "commit">;

    await expect(
      saveProject(project, { port, nextCommandId: () => "desktop.settings.1" }),
    ).rejects.toThrow("desktop runtime lock is unavailable");
  });
});
