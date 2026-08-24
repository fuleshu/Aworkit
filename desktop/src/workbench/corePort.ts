import { invoke } from "@tauri-apps/api/core";
import { z } from "zod";
import { createDurableCommandId } from "../commandId";
import type { AppearancePreference } from "./appearance";
import {
  parseWorkflow,
  validateWorkflow,
  type WorkflowDocument,
} from "./workflow";

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
    portableHistoryEnabled: z.boolean(),
    projectRoots: z.array(z.string()),
    provider: z
      .object({
        baseUrl: z.string(),
        model: z.string(),
        credentialConfigured: z.boolean(),
        state: z.enum(["unconfigured", "configured", "ready", "error"]),
        detail: z.string().nullable(),
      })
      .strict(),
  })
  .strict();
const providerTestResultSchema = z
  .object({
    ok: z.boolean(),
    message: z.string(),
    model: z.string().nullable(),
  })
  .strict();
const workflowSnapshotSchema = z
  .object({
    version: z.number().int().nonnegative(),
    document: z.unknown(),
    editable: z.boolean(),
  })
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
  readonly portableHistoryEnabled: boolean;
  readonly projectRoots: readonly string[];
  readonly provider: ProviderSettings;
}
export type ProviderState = "unconfigured" | "configured" | "ready" | "error";
export interface ProviderSettings {
  readonly baseUrl: string;
  readonly model: string;
  readonly credentialConfigured: boolean;
  readonly state: ProviderState;
  readonly detail: string | null;
}
export type CredentialAction = "keep" | "replace" | "clear";
export interface ProviderSettingsCommit {
  readonly baseUrl: string;
  readonly model: string;
  readonly credentialAction: CredentialAction;
  readonly apiKey: string | null;
}
export interface SettingsCommit {
  readonly commandId: string;
  readonly expectedVersion: number;
  readonly appearance: AppearancePreference;
  readonly portableHistoryEnabled: boolean;
  readonly provider: ProviderSettingsCommit;
}
export interface ProviderTestRequest {
  readonly baseUrl: string;
  readonly model: string;
  readonly apiKey: string | null;
  readonly useStoredCredential: boolean;
}
export interface ProviderTestResult {
  readonly ok: boolean;
  readonly message: string;
  readonly model: string | null;
}
export interface SettingsCorePort {
  snapshot(): Promise<SettingsSnapshot>;
  commit(command: SettingsCommit): Promise<WorkbenchReceipt>;
  testProvider(request: ProviderTestRequest): Promise<ProviderTestResult>;
}
export interface WorkflowSnapshot {
  readonly version: number;
  readonly document: WorkflowDocument;
  readonly editable: boolean;
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
  public async testProvider(
    request: ProviderTestRequest,
  ): Promise<ProviderTestResult> {
    return providerTestResultSchema.parse(
      await invoke("settings_test_provider", { request }),
    );
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
    version: 0,
    appearance: "system",
    portableHistoryEnabled: false,
    projectRoots: [],
    provider: {
      baseUrl: "",
      model: "",
      credentialConfigured: false,
      state: "unconfigured",
      detail: "Enter a base URL and model ID, then save the provider.",
    },
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
    const baseUrl = command.provider.baseUrl.trim();
    const model = command.provider.model.trim();
    if ((baseUrl === "") !== (model === ""))
      throw new Error("provider base URL and model ID must be set together");
    if (
      command.provider.credentialAction === "replace" &&
      (command.provider.apiKey?.trim() ?? "") === ""
    )
      throw new Error("replacement API key cannot be empty");
    const credentialConfigured =
      command.provider.credentialAction === "replace"
        ? true
        : command.provider.credentialAction === "clear"
          ? false
          : this.state.provider.credentialConfigured;
    this.state = {
      ...this.state,
      version: this.state.version + 1,
      appearance: command.appearance,
      portableHistoryEnabled: command.portableHistoryEnabled,
      provider: {
        baseUrl,
        model,
        credentialConfigured,
        state: baseUrl === "" ? "unconfigured" : "configured",
        detail:
          baseUrl === ""
            ? "Enter a base URL and model ID, then save the provider."
            : "Saved. Test the connection before starting a Chat.",
      },
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
  public async testProvider(
    request: ProviderTestRequest,
  ): Promise<ProviderTestResult> {
    if (request.baseUrl.trim() === "" || request.model.trim() === "")
      return {
        ok: false,
        message: "Enter both a base URL and model ID before testing.",
        model: null,
      };
    try {
      const url = new URL(request.baseUrl);
      if (url.protocol !== "http:" && url.protocol !== "https:") throw new Error();
    } catch {
      return {
        ok: false,
        message: "The base URL must be a valid HTTP or HTTPS URL.",
        model: null,
      };
    }
    return {
      ok: false,
      message:
        "Connection testing requires the native desktop runtime; browser Preview did not contact the provider.",
      model: null,
    };
  }
}

export class PreviewWorkflowCorePort implements WorkflowCorePort {
  private version = 1;
  private document: WorkflowDocument;
  private editable: boolean;
  private readonly receipts = new Map<
    string,
    { readonly fingerprint: string; readonly receipt: WorkbenchReceipt }
  >();
  public constructor(document: WorkflowDocument) {
    this.document = parseWorkflow(JSON.stringify(document));
    this.editable = this.document.schemaVersion === 1;
  }
  public async snapshot(): Promise<WorkflowSnapshot> {
    return {
      version: this.version,
      document: this.document,
      editable: this.editable,
    };
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
    if (!this.editable)
      throw new Error(
        "stored workflow uses an inspectable read-only schema and cannot be overwritten",
      );
    const blockingIssue = validateWorkflow(command.document).find(
      (issue) => issue.code !== "missing_dependency",
    );
    if (blockingIssue !== undefined) throw new Error(blockingIssue.message);
    this.document = parseWorkflow(JSON.stringify(command.document));
    this.editable = true;
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

export function nextWorkbenchCommandId(scope: "settings" | "workflow"): string {
  return createDurableCommandId(scope);
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
    editable: parsed.editable,
  };
}
