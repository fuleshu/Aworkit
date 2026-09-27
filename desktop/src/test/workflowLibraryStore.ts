/**
 * Native-shaped test double for the workflow library and document ports.
 *
 * One document store stands behind both surfaces, so a library entry's name is
 * always derived from the stored document exactly as the trusted core derives
 * it, and a save through one port is visible through the other.
 */
import { bundledDefaultWorkflowId, bundledWorkflowTemplates } from "../workbench/bundledWorkflows";
import type {
  WorkbenchReceipt,
  WorkflowCommit,
  WorkflowCorePort,
  WorkflowCreateCommand,
  WorkflowCreateReceipt,
  WorkflowLibraryPort,
  WorkflowLibrarySnapshot,
  WorkflowRenameCommand,
  WorkflowTargetCommand,
} from "../workbench/corePort";
import type { WorkflowDocument } from "../workbench/workflow";

export class WorkflowStore {
  private readonly stored = new Map<
    string,
    {
      readonly document: WorkflowDocument;
      readonly version: number;
      readonly editable: boolean;
    }
  >();
  private defaultWorkflowId = bundledDefaultWorkflowId;
  private libraryVersion = 1;
  private customIds = 0;

  public constructor() {
    for (const template of bundledWorkflowTemplates) {
      if (!template.seedOnFreshProfile) continue;
      this.stored.set(template.workflowId, {
        document: structuredClone(template.document),
        version: 1,
        editable: true,
      });
    }
  }

  public readonly libraryPort: WorkflowLibraryPort = {
    snapshot: async (): Promise<WorkflowLibrarySnapshot> => ({
      version: this.libraryVersion,
      defaultWorkflowId: this.defaultWorkflowId,
      entries: [...this.stored].map(([id, entry]) => ({
        id,
        name: displayName(entry.document, id),
        version: entry.version,
        editable: entry.editable,
        default: id === this.defaultWorkflowId,
      })),
    }),
    create: async (
      command: WorkflowCreateCommand,
    ): Promise<WorkflowCreateReceipt> => {
      const template = bundledWorkflowTemplates.find(
        ({ templateId }) => templateId === (command.template ?? ""),
      );
      if (template === undefined)
        throw new Error(
          `unknown bundled workflow template '${command.template}'`,
        );
      const id = `workflow.custom.${(this.customIds += 1)}`;
      this.stored.set(id, {
        document: { ...structuredClone(template.document), id, name: command.name },
        version: 1,
        editable: true,
      });
      this.libraryVersion += 1;
      return {
        commandId: command.commandId,
        accepted: true,
        currentVersion: 1,
        workflowId: id,
      };
    },
    duplicate: async (
      command: WorkflowRenameCommand,
    ): Promise<WorkflowCreateReceipt> => {
      const source = this.require(command.workflowId);
      const id = `workflow.custom.${(this.customIds += 1)}`;
      this.stored.set(id, {
        document: { ...structuredClone(source.document), id, name: command.name },
        version: 1,
        editable: true,
      });
      this.libraryVersion += 1;
      return {
        commandId: command.commandId,
        accepted: true,
        currentVersion: 1,
        workflowId: id,
      };
    },
    rename: async (command: WorkflowRenameCommand): Promise<WorkbenchReceipt> => {
      const source = this.require(command.workflowId);
      this.stored.set(command.workflowId, {
        ...source,
        document: { ...source.document, name: command.name },
        version: source.version + 1,
      });
      this.libraryVersion += 1;
      return {
        commandId: command.commandId,
        accepted: true,
        currentVersion: source.version + 1,
        reason: null,
      };
    },
    remove: async (command: WorkflowTargetCommand): Promise<WorkbenchReceipt> => {
      if (this.stored.size <= 1)
        throw new Error(
          "at least one workflow must remain in the workflow library",
        );
      this.require(command.workflowId);
      this.stored.delete(command.workflowId);
      if (this.defaultWorkflowId === command.workflowId)
        this.defaultWorkflowId = [...this.stored.keys()][0]!;
      this.libraryVersion += 1;
      return {
        commandId: command.commandId,
        accepted: true,
        currentVersion: this.libraryVersion,
        reason: null,
      };
    },
    setDefault: async (
      command: WorkflowTargetCommand,
    ): Promise<WorkbenchReceipt> => {
      this.require(command.workflowId);
      this.defaultWorkflowId = command.workflowId;
      this.libraryVersion += 1;
      return {
        commandId: command.commandId,
        accepted: true,
        currentVersion: this.libraryVersion,
        reason: null,
      };
    },
  };

  public readonly documentPort: WorkflowCorePort = {
    snapshot: async (workflowId?: string) => {
      const entry = this.require(workflowId ?? this.defaultWorkflowId);
      return {
        version: entry.version,
        document: structuredClone(entry.document),
        editable: entry.editable,
      };
    },
    commit: async (command: WorkflowCommit): Promise<WorkbenchReceipt> => {
      const target = command.workflowId ?? this.defaultWorkflowId;
      const entry = this.require(target);
      if (command.expectedVersion !== entry.version)
        throw new Error(
          `workflow version conflict: expected ${command.expectedVersion}, actual ${entry.version}`,
        );
      this.stored.set(target, {
        ...entry,
        document: structuredClone(command.document),
        version: entry.version + 1,
      });
      return {
        commandId: command.commandId,
        accepted: true,
        currentVersion: entry.version + 1,
        reason: null,
      };
    },
  };

  /** The stored document of one entry, or `null` when the entry is gone. */
  public document(workflowId: string): WorkflowDocument | null {
    const entry = this.stored.get(workflowId);
    return entry === undefined ? null : structuredClone(entry.document);
  }

  private require(workflowId: string) {
    const entry = this.stored.get(workflowId);
    if (entry === undefined)
      throw new Error(
        `workflow '${workflowId}' does not exist in the workflow library`,
      );
    return entry;
  }
}

/** Stored documents are the naming truth, exactly as the trusted core derives it. */
export function displayName(
  document: WorkflowDocument,
  fallbackId: string,
): string {
  return typeof document.name === "string" && document.name.trim() !== ""
    ? document.name
    : fallbackId;
}
