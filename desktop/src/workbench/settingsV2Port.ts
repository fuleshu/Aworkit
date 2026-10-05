import { nativeToolDefaults } from "./toolRegistry";
import { invoke } from "@tauri-apps/api/core";
import { z } from "zod";
import {
  comfyUiWorkflowToolSchema,
  mcpServerConfigurationSchema,
  mcpToolConfigurationSchema,
} from "./configuration";
import { createDurableCommandId } from "../commandId";
import {
  extensionConfigurationSchema,
  settingsConfigurationV2Schema,
  settingsV2SnapshotSchema,
  type BuiltInToolConfiguration,
  type ComfyUiWorkflowTool,
  type ExternalAgentConfiguration,
  type ExtensionConfiguration,
  type McpServerConfiguration,
  type ProjectConfiguration,
  type ProviderConfiguration,
  type SettingsConfigurationV2,
  type SettingsV2Snapshot,
} from "./configuration";

const settingsReceiptSchema = z
  .object({
    commandId: z.string().min(1),
    accepted: z.boolean(),
    currentVersion: z.number().int().positive(),
    reason: z.string().nullable(),
  })
  .strict();

const credentialMutationOutcomeSchema = z
  .object({
    operation: z.enum(["create", "replace"]),
    previousCredentialRef: z.string().min(1).nullable(),
    freshCredentialRef: z.string().min(1),
  })
  .strict();

const credentialStoreReceiptSchema = settingsReceiptSchema
  .extend({ credentialMutation: credentialMutationOutcomeSchema })
  .strict();

const providerProbeSchema = z
  .object({
    ok: z.boolean(),
    message: z.string(),
    providerId: z.string(),
    modelId: z.string().nullable(),
    remoteModelId: z.string().nullable(),
    latencyMillis: z.number().int().nonnegative(),
    draftFingerprint: z.string(),
  })
  .strict();

const discoveredModelSchema = z
  .object({
    remoteId: z.string().min(1),
    name: z.string().min(1),
    contextWindow: z.number().int().positive().nullable(),
    maxOutputTokens: z.number().int().positive().nullable(),
    capabilities: z.array(z.string()),
  })
  .strict();

const modelDiscoverySchema = z
  .object({
    providerId: z.string(),
    draftFingerprint: z.string(),
    models: z.array(discoveredModelSchema),
    message: z.string(),
  })
  .strict();

const mcpProbeSchema = z
  .object({
    tools: z.array(mcpToolConfigurationSchema).optional(),
    serverId: z.string().min(1),
    protocolVersion: z.string().min(1),
    features: z
      .object({
        tools: z.boolean(),
        resources: z.boolean(),
        prompts: z.boolean(),
        progress: z.boolean(),
        cancellation: z.boolean(),
      })
      .strict(),
    toolNames: z.array(z.string()),
    resourceNames: z.array(z.string()),
    promptNames: z.array(z.string()),
    bindingHash: z.string().min(1),
    catalogHash: z.string().min(1),
    latencyMillis: z.number().int().nonnegative(),
    draftFingerprint: z.string().min(1),
    message: z.string(),
  })
  .strict();

const projectProbeSchema = z
  .object({
    ok: z.boolean(),
    projectId: z.string().min(1),
    workspaceKind: z.enum([
      "local_directory",
      "git_worktree",
      "container_mount",
      "remote",
    ]),
    resolvedLocation: z.string().nullable(),
    message: z.string(),
    draftFingerprint: z.string().min(1),
  })
  .strict();

const toolProbeSchema = z
  .object({
    ok: z.boolean(),
    toolId: z.string().min(1),
    adapter: z.string().min(1),
    message: z.string(),
    draftFingerprint: z.string().min(1),
  })
  .strict();

const externalAgentCapabilitiesSchema = z
  .object({
    progress: z.boolean(),
    continuation: z.boolean(),
    cancellation: z.boolean(),
    approvals: z.boolean(),
  })
  .strict();

const externalAgentProbeSchema = z
  .object({
    agentId: z.string().min(1),
    protocol: z.string().min(1),
    serverIdentity: z.string().nullable(),
    platformFamily: z.string().nullable(),
    platformOs: z.string().nullable(),
    accountType: z.string().nullable(),
    requiresOpenaiAuth: z.boolean(),
    modelIds: z.array(z.string().min(1)),
    capabilities: externalAgentCapabilitiesSchema,
    latencyMillis: z.number().int().nonnegative(),
    draftFingerprint: z.string().min(1),
    message: z.string(),
  })
  .strict();

export const comfyUiProbeResultSchema = z
  .object({
    ok: z.boolean(),
    message: z.string(),
    endpoint: z.string(),
    version: z.string().nullable(),
    latencyMillis: z.number().int().nonnegative(),
    draftFingerprint: z.string(),
  })
  .strict();

export const comfyUiStartResultSchema = z
  .object({
    ok: z.boolean(),
    message: z.string(),
    endpoint: z.string(),
    processId: z.number().int().nonnegative().nullable(),
    logPath: z.string(),
    outputTail: z.string(),
  })
  .strict();

const comfyUiWorkflowInputSchema = z
  .object({
    nodeId: z.string(),
    classType: z.string(),
    title: z.string().nullable(),
    inputName: z.string(),
    valueKind: z.enum(["string", "integer", "number", "boolean"]),
    currentValue: z.unknown(),
    choices: z.array(z.unknown()),
  })
  .strict();

export const comfyUiInspectResultSchema = z
  .object({
    ok: z.boolean(),
    message: z.string(),
    workflowPath: z.string(),
    inspection: z
      .object({
        nodeCount: z.number().int().nonnegative(),
        inputs: z.array(comfyUiWorkflowInputSchema),
      })
      .strict(),
  })
  .strict();

export const comfyUiAutocreateResultSchema = z
  .object({
    ok: z.boolean(),
    message: z.string(),
    tool: comfyUiWorkflowToolSchema,
    summary: z.string(),
    providerId: z.string(),
    modelId: z.string(),
    inputTokens: z.number().int().nonnegative(),
    outputTokens: z.number().int().nonnegative(),
  })
  .strict();

/** One event kind's share of the bytes the local history store holds. */
const historyPayloadKindSchema = z
  .object({
    kind: z.string().min(1),
    events: z.number().int().nonnegative(),
    bytes: z.number().int().nonnegative(),
  })
  .strict();

/**
 * What the local history store holds right now. Measured, not estimated, so
 * Settings can state the retention policy and its current cost.
 */
export const historyStoreStatusSchema = z
  .object({
    storeBytes: z.number().int().nonnegative(),
    payloadBytes: z.number().int().nonnegative(),
    snapshotBytes: z.number().int().nonnegative(),
    deletedChats: z.number().int().nonnegative(),
    deletedChatBytes: z.number().int().nonnegative(),
    /** Delivery records the queue still holds, delivered or not. */
    outboxRows: z.number().int().nonnegative(),
    /** How many of those are already delivered: the ones a reclaim removes. */
    outboxDeliveredRows: z.number().int().nonnegative(),
    /** The payload bytes every delivery record holds together. */
    outboxBytes: z.number().int().nonnegative(),
    retainedTurns: z.number().int().nonnegative(),
    kinds: z.array(historyPayloadKindSchema),
  })
  .strict();

/** What one explicit reclaim pass did to the store. */
export const historyReclaimReportSchema = z
  .object({
    streamsScanned: z.number().int().nonnegative(),
    payloadsPruned: z.number().int().nonnegative(),
    payloadBytesReleased: z.number().int().nonnegative(),
    chatsPurged: z.number().int().nonnegative(),
    eventsRemoved: z.number().int().nonnegative(),
    /** Already-delivered delivery records the pass removed outright. */
    outboxRowsRemoved: z.number().int().nonnegative(),
    /** The payload bytes those removed delivery records held. */
    outboxBytesReleased: z.number().int().nonnegative(),
  })
  .strict();

/** A reclaim pass plus the store size it moved from and to. */
export const historyReclaimOutcomeSchema = z
  .object({
    report: historyReclaimReportSchema,
    storeBytesBefore: z.number().int().nonnegative(),
    storeBytesAfter: z.number().int().nonnegative(),
  })
  .strict();

/** The native progress of one running reclaim pass. */
export const historyReclaimProgressSchema = z
  .object({
    phase: z.enum(["releasing", "rewriting"]),
    done: z.number().int().nonnegative(),
    total: z.number().int().nonnegative(),
  })
  .strict();

export interface SettingsV2Receipt {
  readonly commandId: string;
  readonly accepted: boolean;
  readonly currentVersion: number;
  readonly reason: string | null;
}

export interface CredentialMutationOutcome {
  readonly operation: "create" | "replace";
  readonly previousCredentialRef: string | null;
  readonly freshCredentialRef: string;
}

export interface CredentialStoreReceipt extends SettingsV2Receipt {
  readonly credentialMutation: CredentialMutationOutcome;
}

export interface SettingsV2Commit {
  readonly commandId: string;
  readonly expectedVersion: number;
  readonly settings: SettingsConfigurationV2;
}

/** Write-only create/replace command for an operating-system credential. */
export interface CredentialStoreCommand {
  readonly commandId: string;
  readonly expectedVersion: number;
  readonly replaceCredentialRef: string | null;
  readonly label: string;
  readonly kind: string;
  readonly boundProviderId: string | null;
  readonly boundEndpoint: string | null;
  readonly fields: Readonly<Record<string, string>>;
}

/** Version-checked deletion of one unreferenced credential record. */
export interface CredentialDeleteCommand {
  readonly commandId: string;
  readonly expectedVersion: number;
  readonly credentialRef: string;
}

/** Registers one saved inert discovery after native integrity verification. */
export interface ExtensionRegisterCommand {
  readonly commandId: string;
  readonly expectedVersion: number;
  readonly extensionId: string;
}

export interface ProviderProbeRequest {
  readonly provider: ProviderConfiguration;
  readonly modelId: string;
  readonly replacementCredential: string | null;
  readonly useStoredCredential: boolean;
  readonly draftFingerprint: string;
}

export interface ProviderProbeResult {
  readonly ok: boolean;
  readonly message: string;
  readonly providerId: string;
  readonly modelId: string | null;
  readonly remoteModelId: string | null;
  readonly latencyMillis: number;
  readonly draftFingerprint: string;
}

export interface ModelDiscoveryRequest {
  readonly provider: ProviderConfiguration;
  readonly replacementCredential: string | null;
  readonly useStoredCredential: boolean;
  readonly draftFingerprint: string;
}

export interface DiscoveredModel {
  readonly remoteId: string;
  readonly name: string;
  readonly contextWindow: number | null;
  readonly maxOutputTokens: number | null;
  readonly capabilities: readonly string[];
}

export interface ModelDiscoveryResult {
  readonly providerId: string;
  readonly draftFingerprint: string;
  readonly models: readonly DiscoveredModel[];
  readonly message: string;
}

export interface McpProbeRequest {
  readonly server: McpServerConfiguration;
  readonly draftFingerprint: string;
}

export interface McpProbeResult {
  readonly tools?: readonly import("./configuration").McpToolConfiguration[];
  readonly serverId: string;
  readonly protocolVersion: string;
  readonly features: {
    readonly tools: boolean;
    readonly resources: boolean;
    readonly prompts: boolean;
    readonly progress: boolean;
    readonly cancellation: boolean;
  };
  readonly toolNames: readonly string[];
  readonly resourceNames: readonly string[];
  readonly promptNames: readonly string[];
  readonly bindingHash: string;
  readonly catalogHash: string;
  readonly latencyMillis: number;
  readonly draftFingerprint: string;
  readonly message: string;
}

export interface ProjectProbeRequest {
  readonly project: ProjectConfiguration;
  readonly draftFingerprint: string;
}

export interface ProjectProbeResult {
  readonly ok: boolean;
  readonly projectId: string;
  readonly workspaceKind: ProjectConfiguration["workspace"]["kind"];
  readonly resolvedLocation: string | null;
  readonly message: string;
  readonly draftFingerprint: string;
}

export interface ToolProbeRequest {
  readonly tool: BuiltInToolConfiguration;
  readonly project: ProjectConfiguration | null;
  readonly draftFingerprint: string;
}

export interface ToolProbeResult {
  readonly ok: boolean;
  readonly toolId: string;
  readonly adapter: string;
  readonly message: string;
  readonly draftFingerprint: string;
}

export interface ExternalAgentProbeRequest {
  readonly agent: ExternalAgentConfiguration;
  readonly draftFingerprint: string;
}

export interface ExternalAgentProbeResult {
  readonly agentId: string;
  readonly protocol: string;
  readonly serverIdentity: string | null;
  readonly platformFamily: string | null;
  readonly platformOs: string | null;
  readonly accountType: string | null;
  readonly requiresOpenaiAuth: boolean;
  readonly modelIds: readonly string[];
  readonly capabilities: ExternalAgentConfiguration["capabilities"];
  readonly latencyMillis: number;
  readonly draftFingerprint: string;
  readonly message: string;
}

export interface ComfyUiProbeRequest {
  readonly endpoint: string;
  readonly draftFingerprint: string;
}

export type ComfyUiProbeResult = z.infer<typeof comfyUiProbeResultSchema>;

export interface ComfyUiStartRequest {
  readonly endpoint: string;
  readonly installPath: string;
  readonly launchArguments: readonly string[];
}

export type ComfyUiStartResult = z.infer<typeof comfyUiStartResultSchema>;

export interface ComfyUiInspectRequest {
  readonly workflowPath: string;
  readonly endpoint: string | null;
}

export type ComfyUiInspectResult = z.infer<typeof comfyUiInspectResultSchema>;

export interface ComfyUiAutocreateRequest {
  readonly endpoint: string;
  readonly workflowPath: string;
  readonly draftFingerprint: string;
}

export type ComfyUiAutocreateResult = z.infer<
  typeof comfyUiAutocreateResultSchema
>;

export type HistoryStoreStatus = z.infer<typeof historyStoreStatusSchema>;

export type HistoryReclaimReport = z.infer<typeof historyReclaimReportSchema>;

export type HistoryReclaimOutcome = z.infer<
  typeof historyReclaimOutcomeSchema
>;

export type HistoryReclaimProgress = z.infer<
  typeof historyReclaimProgressSchema
>;

export interface SettingsV2CorePort {
  snapshot(): Promise<SettingsV2Snapshot>;
  commit(command: SettingsV2Commit): Promise<SettingsV2Receipt>;
  storeCredential(command: CredentialStoreCommand): Promise<CredentialStoreReceipt>;
  deleteCredential(command: CredentialDeleteCommand): Promise<SettingsV2Receipt>;
  testProvider(request: ProviderProbeRequest): Promise<ProviderProbeResult>;
  discoverModels(request: ModelDiscoveryRequest): Promise<ModelDiscoveryResult>;
  probeMcp(request: McpProbeRequest): Promise<McpProbeResult>;
  probeProject(request: ProjectProbeRequest): Promise<ProjectProbeResult>;
  probeTool(request: ToolProbeRequest): Promise<ToolProbeResult>;
  probeExternalAgent(
    request: ExternalAgentProbeRequest,
  ): Promise<ExternalAgentProbeResult>;
  comfyuiProbe(request: ComfyUiProbeRequest): Promise<ComfyUiProbeResult>;
  comfyuiStart(request: ComfyUiStartRequest): Promise<ComfyUiStartResult>;
  comfyuiInspect(request: ComfyUiInspectRequest): Promise<ComfyUiInspectResult>;
  comfyuiAutocreate(
    request: ComfyUiAutocreateRequest,
  ): Promise<ComfyUiAutocreateResult>;
  inspectExtension(path: string): Promise<ExtensionConfiguration>;
  registerExtension(
    command: ExtensionRegisterCommand,
  ): Promise<SettingsV2Receipt>;
  /** Copies one chosen plugin package folder into the plugin folder. */
  installToolPlugin(path: string): Promise<SettingsV2Snapshot>;
  /** Removes one sourced plugin package and its saved server. */
  removeToolPlugin(pluginId: string): Promise<SettingsV2Snapshot>;
  /** Reveals the plugin folder in the platform file manager. */
  openToolPluginFolder(): Promise<void>;
  /** Measures what the local history store holds without changing it. */
  historyStoreStatus(): Promise<HistoryStoreStatus>;
  /**
   * Runs one explicit retention pass. It can take minutes, so the caller
   * subscribes to `onHistoryReclaimProgress` before invoking it.
   */
  historyReclaim(): Promise<HistoryReclaimOutcome>;
  /**
   * Subscribes to the running pass's progress, returning its unlisten
   * function. Progress is a report, never a precondition of the pass.
   */
  onHistoryReclaimProgress(
    handler: (progress: HistoryReclaimProgress) => void,
  ): Promise<() => void>;
}

export class TauriSettingsV2CorePort implements SettingsV2CorePort {
  public async snapshot(): Promise<SettingsV2Snapshot> {
    return settingsV2SnapshotSchema.parse(await invoke("settings_v2_snapshot"));
  }

  public async commit(command: SettingsV2Commit): Promise<SettingsV2Receipt> {
    return settingsReceiptSchema.parse(
      await invoke("settings_v2_commit", { command }),
    );
  }

  public async storeCredential(
    command: CredentialStoreCommand,
  ): Promise<CredentialStoreReceipt> {
    return credentialStoreReceiptSchema.parse(
      await invoke("settings_v2_store_credential", { command }),
    );
  }

  public async deleteCredential(
    command: CredentialDeleteCommand,
  ): Promise<SettingsV2Receipt> {
    return settingsReceiptSchema.parse(
      await invoke("settings_v2_delete_credential", { command }),
    );
  }

  public async testProvider(
    request: ProviderProbeRequest,
  ): Promise<ProviderProbeResult> {
    return providerProbeSchema.parse(
      await invoke("settings_v2_test_provider", { request }),
    );
  }

  public async discoverModels(
    request: ModelDiscoveryRequest,
  ): Promise<ModelDiscoveryResult> {
    return modelDiscoverySchema.parse(
      await invoke("settings_v2_discover_models", { request }),
    );
  }

  public async probeMcp(request: McpProbeRequest): Promise<McpProbeResult> {
    return mcpProbeSchema.parse(
      await invoke("settings_v2_probe_mcp", { request }),
    );
  }

  public async probeProject(
    request: ProjectProbeRequest,
  ): Promise<ProjectProbeResult> {
    return projectProbeSchema.parse(
      await invoke("settings_v2_probe_project", { request }),
    );
  }

  public async probeTool(request: ToolProbeRequest): Promise<ToolProbeResult> {
    return toolProbeSchema.parse(
      await invoke("settings_v2_probe_tool", { request }),
    );
  }

  public async probeExternalAgent(
    request: ExternalAgentProbeRequest,
  ): Promise<ExternalAgentProbeResult> {
    return externalAgentProbeSchema.parse(
      await invoke("settings_v2_probe_external_agent", { request }),
    );
  }

  public async comfyuiProbe(
    request: ComfyUiProbeRequest,
  ): Promise<ComfyUiProbeResult> {
    return comfyUiProbeResultSchema.parse(
      await invoke("settings_v2_comfyui_probe", { request }),
    );
  }

  public async comfyuiStart(
    request: ComfyUiStartRequest,
  ): Promise<ComfyUiStartResult> {
    return comfyUiStartResultSchema.parse(
      await invoke("settings_v2_comfyui_start", { request }),
    );
  }

  public async comfyuiInspect(
    request: ComfyUiInspectRequest,
  ): Promise<ComfyUiInspectResult> {
    return comfyUiInspectResultSchema.parse(
      await invoke("settings_v2_comfyui_inspect", { request }),
    );
  }

  public async comfyuiAutocreate(
    request: ComfyUiAutocreateRequest,
  ): Promise<ComfyUiAutocreateResult> {
    return comfyUiAutocreateResultSchema.parse(
      await invoke("settings_v2_comfyui_autocreate", { request }),
    );
  }

  public async inspectExtension(path: string): Promise<ExtensionConfiguration> {
    return extensionConfigurationSchema.parse(
      await invoke("settings_v2_inspect_extension", { path }),
    );
  }

  public async registerExtension(
    command: ExtensionRegisterCommand,
  ): Promise<SettingsV2Receipt> {
    return settingsReceiptSchema.parse(
      await invoke("settings_v2_register_extension", { command }),
    );
  }

  public async installToolPlugin(path: string): Promise<SettingsV2Snapshot> {
    return settingsV2SnapshotSchema.parse(
      await invoke("settings_v2_install_tool_plugin", { path }),
    );
  }

  public async removeToolPlugin(pluginId: string): Promise<SettingsV2Snapshot> {
    return settingsV2SnapshotSchema.parse(
      await invoke("settings_v2_remove_tool_plugin", { pluginId }),
    );
  }

  public async openToolPluginFolder(): Promise<void> {
    await invoke("settings_v2_open_tool_plugin_folder");
  }

  public async historyStoreStatus(): Promise<HistoryStoreStatus> {
    return historyStoreStatusSchema.parse(
      await invoke("desktop_history_store_status"),
    );
  }

  public async historyReclaim(): Promise<HistoryReclaimOutcome> {
    return historyReclaimOutcomeSchema.parse(
      await invoke("desktop_history_reclaim"),
    );
  }

  public async onHistoryReclaimProgress(
    handler: (progress: HistoryReclaimProgress) => void,
  ): Promise<() => void> {
    const { listen } = await import("@tauri-apps/api/event");
    return listen<unknown>("aworkit:history-reclaim", ({ payload }) => {
      const parsed = historyReclaimProgressSchema.safeParse(payload);
      if (parsed.success) handler(parsed.data);
    });
  }
}

export class PreviewSettingsV2CorePort implements SettingsV2CorePort {
  private snapshotValue: SettingsV2Snapshot;

  public constructor(snapshot: SettingsV2Snapshot = emptySettingsV2Snapshot()) {
    this.snapshotValue = settingsV2SnapshotSchema.parse(snapshot);
  }

  public async snapshot(): Promise<SettingsV2Snapshot> {
    return structuredClone(this.snapshotValue);
  }

  public async commit(command: SettingsV2Commit): Promise<SettingsV2Receipt> {
    if (command.expectedVersion !== this.snapshotValue.version) {
      throw new Error(
        `settings version conflict: expected ${command.expectedVersion}, actual ${this.snapshotValue.version}`,
      );
    }
    const settings = settingsConfigurationV2Schema.parse(command.settings);
    this.snapshotValue = {
      ...this.snapshotValue,
      version: this.snapshotValue.version + 1,
      settings: structuredClone(settings),
    };
    return {
      commandId: command.commandId,
      accepted: true,
      currentVersion: this.snapshotValue.version,
      reason: null,
    };
  }

  public async storeCredential(
    _command: CredentialStoreCommand,
  ): Promise<CredentialStoreReceipt> {
    throw new Error(
      "Credential storage requires the native desktop runtime; browser Preview stored no secret.",
    );
  }

  public async deleteCredential(
    _command: CredentialDeleteCommand,
  ): Promise<SettingsV2Receipt> {
    throw new Error(
      "Credential deletion requires the native desktop runtime; browser Preview deleted nothing.",
    );
  }

  public async testProvider(
    request: ProviderProbeRequest,
  ): Promise<ProviderProbeResult> {
    return {
      ok: false,
      message:
        "Provider tests require the native desktop runtime; browser Preview made no network request.",
      providerId: request.provider.id,
      modelId: request.modelId,
      remoteModelId: null,
      latencyMillis: 0,
      draftFingerprint: request.draftFingerprint,
    };
  }

  public async discoverModels(
    request: ModelDiscoveryRequest,
  ): Promise<ModelDiscoveryResult> {
    return {
      providerId: request.provider.id,
      draftFingerprint: request.draftFingerprint,
      models: [],
      message:
        "Model discovery requires the native desktop runtime; browser Preview made no network request.",
    };
  }

  public async probeMcp(request: McpProbeRequest): Promise<McpProbeResult> {
    return {
      serverId: request.server.id,
      protocolVersion: "unavailable",
      features: {
        tools: false,
        resources: false,
        prompts: false,
        progress: false,
        cancellation: false,
      },
      toolNames: [],
      resourceNames: [],
      promptNames: [],
      bindingHash: "unavailable",
      catalogHash: "unavailable",
      latencyMillis: 0,
      draftFingerprint: request.draftFingerprint,
      message:
        "MCP probes require the native desktop runtime; browser Preview started no process and made no connection.",
    };
  }

  public async probeProject(
    request: ProjectProbeRequest,
  ): Promise<ProjectProbeResult> {
    return {
      ok: false,
      projectId: request.project.id,
      workspaceKind: request.project.workspace.kind,
      resolvedLocation: null,
      message:
        "Project probes require the native desktop runtime; browser Preview resolved no workspace root.",
      draftFingerprint: request.draftFingerprint,
    };
  }

  public async probeTool(request: ToolProbeRequest): Promise<ToolProbeResult> {
    return {
      ok: false,
      toolId: request.tool.id,
      adapter: "unavailable",
      message:
        "Tool probes require the native desktop runtime; browser Preview executed no adapter.",
      draftFingerprint: request.draftFingerprint,
    };
  }

  public async probeExternalAgent(
    request: ExternalAgentProbeRequest,
  ): Promise<ExternalAgentProbeResult> {
    return {
      agentId: request.agent.id,
      protocol: "unavailable",
      serverIdentity: null,
      platformFamily: null,
      platformOs: null,
      accountType: null,
      requiresOpenaiAuth: false,
      modelIds: [],
      capabilities: {
        progress: false,
        continuation: false,
        cancellation: false,
        approvals: false,
      },
      latencyMillis: 0,
      draftFingerprint: request.draftFingerprint,
      message:
        "External-agent probes require the native desktop runtime; browser Preview started no process.",
    };
  }

  public async comfyuiProbe(
    request: ComfyUiProbeRequest,
  ): Promise<ComfyUiProbeResult> {
    return {
      ok: false,
      message: "ComfyUI connection tests are not available in browser Preview.",
      endpoint: request.endpoint,
      version: null,
      latencyMillis: 0,
      draftFingerprint: request.draftFingerprint,
    };
  }

  public async comfyuiStart(
    request: ComfyUiStartRequest,
  ): Promise<ComfyUiStartResult> {
    return {
      ok: false,
      message: "Starting ComfyUI is not available in browser Preview.",
      endpoint: request.endpoint,
      processId: null,
      logPath: "",
      outputTail: "",
    };
  }

  public async comfyuiInspect(
    request: ComfyUiInspectRequest,
  ): Promise<ComfyUiInspectResult> {
    return {
      ok: false,
      message: "ComfyUI workflow inspection is not available in browser Preview.",
      workflowPath: request.workflowPath,
      inspection: { nodeCount: 0, inputs: [] },
    };
  }

  public async comfyuiAutocreate(
    request: ComfyUiAutocreateRequest,
  ): Promise<ComfyUiAutocreateResult> {
    const tool: ComfyUiWorkflowTool = {
      id: "workflow-tool",
      name: "Workflow tool",
      description: "",
      workflowPath: request.workflowPath,
      enabled: true,
      parameters: [],
    };
    return {
      ok: false,
      message: "ComfyUI parameter authoring is not available in browser Preview.",
      tool,
      summary: "",
      providerId: "unavailable",
      modelId: "unavailable",
      inputTokens: 0,
      outputTokens: 0,
    };
  }

  public async inspectExtension(_path: string): Promise<ExtensionConfiguration> {
    throw new Error(
      "Extension inspection requires the native desktop runtime; browser Preview read no file and executed nothing.",
    );
  }

  public async registerExtension(
    _command: ExtensionRegisterCommand,
  ): Promise<SettingsV2Receipt> {
    throw new Error(
      "Extension registration requires the native desktop runtime; browser Preview verified or installed nothing.",
    );
  }

  public async installToolPlugin(_path: string): Promise<SettingsV2Snapshot> {
    throw new Error(
      "Installing a plugin requires the native desktop runtime; browser Preview copied nothing.",
    );
  }

  public async removeToolPlugin(_pluginId: string): Promise<SettingsV2Snapshot> {
    throw new Error(
      "Removing a plugin requires the native desktop runtime; browser Preview deleted nothing.",
    );
  }

  public async openToolPluginFolder(): Promise<void> {
    throw new Error(
      "Opening the plugin folder requires the native desktop runtime; browser Preview opened nothing.",
    );
  }

  public async historyStoreStatus(): Promise<HistoryStoreStatus> {
    throw new Error(
      "Measuring the history store requires the native desktop runtime; browser Preview read no store.",
    );
  }

  public async historyReclaim(): Promise<HistoryReclaimOutcome> {
    throw new Error(
      "Reclaiming history space requires the native desktop runtime; browser Preview released nothing.",
    );
  }

  public async onHistoryReclaimProgress(
    _handler: (progress: HistoryReclaimProgress) => void,
  ): Promise<() => void> {
    // Browser Preview runs no pass, so there is never progress to report.
    return () => {};
  }
}

export function createSettingsV2CorePort(): SettingsV2CorePort {
  return "__TAURI_INTERNALS__" in window
    ? new TauriSettingsV2CorePort()
    : new PreviewSettingsV2CorePort();
}

export function nextSettingsV2CommandId(): string {
  return createDurableCommandId("settings");
}

function emptySettingsV2Snapshot(): SettingsV2Snapshot {
  return {
    version: 1,
    schemaVersion: 2,
    settings: {
      schemaVersion: 2,
      providers: [],
      modelTiers: ["fast", "simple", "balanced", "quality"].map(
        (name) => ({
          id: `tier:${name}`,
          name: name.replace(/^./u, (value) => value.toUpperCase()),
          kind: "standard" as const,
          resolution: { strategy: "unconfigured" as const },
        }),
      ),
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
      projects: [],
      appearance: { mode: "system", fontScale: 1 },
      approvals: { defaultMode: "ask_for_approval" },
      // A fresh profile remembers nothing and has no persisted window placement.
      chatDefaults: {},
      layout: {},
      desktop: {},
      comfyui: {
        endpoint: "http://127.0.0.1:8188/",
        installPath: null,
        launchArguments: ["main.py", "--listen", "127.0.0.1"],
        autoStart: false,
        workflowFolder: null,
        workflowTools: [],
      },
    },
    providerHealth: [],
  };
}
