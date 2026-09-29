import { createLocalId } from "../commandId";
import {
  projectConfigurationSchema,
  type ProjectConfiguration,
  type SettingsConfigurationV2,
} from "../workbench/configuration";
import {
  createSettingsV2CorePort,
  nextSettingsV2CommandId,
  type SettingsV2CorePort,
} from "../workbench/settingsV2Port";

/**
 * The workspace kinds the Chat may create. `remote` is configured in Settings
 * only: it needs a server address the Chat's create dialog has no field for,
 * and a remote project is never selectable in the composer anyway.
 */
export type NewProjectKind =
  | "local_directory"
  | "git_worktree"
  | "container_mount";

/** What the Chat's create-project dialog collects. */
export interface ProjectDraft {
  readonly name: string;
  readonly kind: NewProjectKind;
  readonly location: string;
}

/**
 * One saved project record, exactly as Settings → Projects creates it.
 *
 * The id is generated here rather than in Settings because the record is
 * created from the Chat; both surfaces share `createLocalId` so their ids can
 * never drift apart.
 */
export type NewProjectConfiguration = Omit<ProjectConfiguration, "workspace"> & {
  readonly workspace: { readonly kind: NewProjectKind; readonly location: string };
};

export function projectFromDraft(
  draft: ProjectDraft,
  id: string = createLocalId("project"),
): NewProjectConfiguration {
  const record: NewProjectConfiguration = {
    id,
    name: draft.name.trim(),
    workspace: { kind: draft.kind, location: draft.location.trim() },
    defaultWorkflowId: null,
    // This build keeps Chat history in local SQLite only.
    portableHistoryEnabled: false,
  };
  // The settings schema is the single definition of a valid project record.
  projectConfigurationSchema.parse(record);
  return record;
}

/** The last path segment, used to seed the project name from its folder. */
export function folderLeaf(path: string): string {
  const leaf = path.replace(/[\\/]+$/u, "").split(/[\\/]/u).at(-1)?.trim();
  return leaf === undefined || leaf === "" ? "Project" : leaf;
}

export interface SaveProjectOptions {
  readonly port?: Pick<SettingsV2CorePort, "snapshot" | "commit">;
  readonly nextCommandId?: () => string;
}

function isVersionConflict(message: string): boolean {
  return /version conflict/i.test(message);
}

/**
 * Saves one new project into the settings document Settings edits.
 *
 * The write goes through the same version fence and idempotency key as a
 * Settings save: a concurrent Settings edit moves the version between our read
 * and write, so that round is re-read and retried once with a fresh command id
 * (reusing an id with a different body is refused by the core). Anything else
 * is reported to the dialog, which keeps the user's input.
 */
export async function saveProject<T extends ProjectConfiguration>(
  project: T,
  options: SaveProjectOptions = {},
): Promise<T> {
  const port = options.port ?? createSettingsV2CorePort();
  const nextCommandId = options.nextCommandId ?? nextSettingsV2CommandId;
  for (let attempt = 0; attempt < 2; attempt += 1) {
    const snapshot = await port.snapshot();
    const settings: SettingsConfigurationV2 = {
      ...snapshot.settings,
      projects: [...snapshot.settings.projects, project],
    };
    const commandId = nextCommandId();
    try {
      const receipt = await port.commit({
        commandId,
        expectedVersion: snapshot.version,
        settings,
      });
      if (
        receipt.accepted &&
        receipt.commandId === commandId &&
        receipt.currentVersion === snapshot.version + 1
      ) {
        return project;
      }
      const reason = receipt.reason ?? "The project could not be saved.";
      if (!isVersionConflict(reason)) throw new Error(reason);
    } catch (failure) {
      const message =
        failure instanceof Error ? failure.message : String(failure);
      if (!isVersionConflict(message)) throw failure;
    }
  }
  throw new Error(
    "Settings changed while the project was created. Try creating it again.",
  );
}
