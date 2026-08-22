import { invoke } from "@tauri-apps/api/core";
import { z } from "zod";
import type { AppearancePreference } from "./appearance";
import { parseWorkflow, type WorkflowDocument } from "./workflow";

const receiptSchema = z
  .object({
    commandId: z.string().min(1),
    accepted: z.boolean(),
    currentVersion: z.number().int().nonnegative(),
    reason: z.string().nullable(),
  })
  .strict();
const settingsSnapshotSchema = z
  .object({
    version: z.number().int().nonnegative(),
    appearance: z.enum(["system", "light", "dark"]),
    configuredCapabilities: z.array(z.string()),
    portableHistoryEnabled: z.boolean(),
    projectRoots: z.array(z.string()),
  })
  .strict();
const workflowSnapshotSchema = z
  .object({ version: z.number().int().nonnegative(), document: z.unknown() })
  .strict();

export interface WorkbenchReceipt {
  readonly commandId: string;
  readonly accepted: boolean;
  readonly currentVersion: number;
  readonly reason: string | null;
}
export interface SettingsSnapshot {
  readonly version: number;
  readonly appearance: AppearancePreference;
  readonly configuredCapabilities: readonly string[];
  readonly portableHistoryEnabled: boolean;
  readonly projectRoots: readonly string[];
}
export interface SettingsCommit {
  readonly commandId: string;
  readonly expectedVersion: number;
  readonly appearance: AppearancePreference;
  readonly configuredCapabilities: readonly string[];
  readonly portableHistoryEnabled: boolean;
}
export interface SettingsCorePort {
  snapshot(): Promise<SettingsSnapshot>;
  commit(command: SettingsCommit): Promise<WorkbenchReceipt>;
}
export interface WorkflowSnapshot {
  readonly version: number;
  readonly document: WorkflowDocument;
}
export interface WorkflowCommit {
  readonly commandId: string;
  readonly expectedVersion: number;
  readonly document: WorkflowDocument;
}
export interface WorkflowCorePort {
  snapshot(): Promise<WorkflowSnapshot>;
  commit(command: WorkflowCommit): Promise<WorkbenchReceipt>;
}

export class TauriSettingsCorePort implements SettingsCorePort {
  public async snapshot(): Promise<SettingsSnapshot> {
    return settingsSnapshotSchema.parse(await invoke("settings_snapshot"));
  }
  public async commit(command: SettingsCommit): Promise<WorkbenchReceipt> {
    return receiptSchema.parse(await invoke("settings_commit", { command }));
  }
}

export class TauriWorkflowCorePort implements WorkflowCorePort {
  public async snapshot(): Promise<WorkflowSnapshot> {
    return normalizeWorkflowSnapshot(await invoke("workflow_snapshot"));
  }
  public async commit(command: WorkflowCommit): Promise<WorkbenchReceipt> {
    return receiptSchema.parse(await invoke("workflow_commit", { command }));
  }
}

export class PreviewSettingsCorePort implements SettingsCorePort {
  private state: SettingsSnapshot = {
    version: 3,
    appearance: "system",
    configuredCapabilities: [
      "model.local",
      "model.standard",
      "tool.files",
      "tool.shell",
      "agent.codex",
    ],
    portableHistoryEnabled: false,
    projectRoots: ["/workspace/project-atlas"],
  };
  private readonly receipts = new Map<
    string,
    { readonly fingerprint: string; readonly receipt: WorkbenchReceipt }
  >();
  public async snapshot(): Promise<SettingsSnapshot> {
    return this.state;
  }
  public async commit(command: SettingsCommit): Promise<WorkbenchReceipt> {
    const fingerprint = JSON.stringify(command);
    const seen = this.receipts.get(command.commandId);
    if (seen !== undefined) {
      if (seen.fingerprint !== fingerprint)
        throw new Error(
          "settings command ID was reused with different content",
        );
      return seen.receipt;
    }
    if (command.expectedVersion !== this.state.version)
      throw new Error(
        `settings version conflict: expected ${command.expectedVersion}, actual ${this.state.version}`,
      );
    this.state = {
      ...this.state,
      version: this.state.version + 1,
      appearance: command.appearance,
      configuredCapabilities: [...command.configuredCapabilities],
      portableHistoryEnabled: command.portableHistoryEnabled,
    };
    const receipt = {
      commandId: command.commandId,
      accepted: true,
      currentVersion: this.state.version,
      reason: null,
    };
    this.receipts.set(command.commandId, { fingerprint, receipt });
    return receipt;
  }
}

export class PreviewWorkflowCorePort implements WorkflowCorePort {
  private version = 1;
  private document: WorkflowDocument;
  private readonly receipts = new Map<
    string,
    { readonly fingerprint: string; readonly receipt: WorkbenchReceipt }
  >();
  public constructor(document: WorkflowDocument) {
    this.document = parseWorkflow(JSON.stringify(document));
  }
  public async snapshot(): Promise<WorkflowSnapshot> {
    return { version: this.version, document: this.document };
  }
  public async commit(command: WorkflowCommit): Promise<WorkbenchReceipt> {
    const fingerprint = JSON.stringify(command);
    const seen = this.receipts.get(command.commandId);
    if (seen !== undefined) {
      if (seen.fingerprint !== fingerprint)
        throw new Error(
          "workflow command ID was reused with different content",
        );
      return seen.receipt;
    }
    if (command.expectedVersion !== this.version)
      throw new Error(
        `workflow version conflict: expected ${command.expectedVersion}, actual ${this.version}`,
      );
    this.document = parseWorkflow(JSON.stringify(command.document));
    this.version += 1;
    const receipt = {
      commandId: command.commandId,
      accepted: true,
      currentVersion: this.version,
      reason: null,
    };
    this.receipts.set(command.commandId, { fingerprint, receipt });
    return receipt;
  }
}

let nextCommand = 1;
export function nextWorkbenchCommandId(scope: "settings" | "workflow"): string {
  return `desktop.${scope}.${nextCommand++}`;
}
export function createSettingsCorePort(): SettingsCorePort {
  return "__TAURI_INTERNALS__" in window
    ? new TauriSettingsCorePort()
    : new PreviewSettingsCorePort();
}
export function createWorkflowCorePort(
  document: WorkflowDocument,
): WorkflowCorePort {
  return "__TAURI_INTERNALS__" in window
    ? new TauriWorkflowCorePort()
    : new PreviewWorkflowCorePort(document);
}
function normalizeWorkflowSnapshot(value: unknown): WorkflowSnapshot {
  const parsed = workflowSnapshotSchema.parse(value);
  return {
    version: parsed.version,
    document: parseWorkflow(JSON.stringify(parsed.document)),
  };
}
