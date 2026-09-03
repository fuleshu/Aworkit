import { invoke } from "@tauri-apps/api/core";
import { z } from "zod";
import { createDurableCommandId } from "../commandId";
import {
  extensionConfigurationSchema,
  settingsConfigurationV2Schema,
  settingsV2SnapshotSchema,
  type BuiltInToolConfiguration,
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
  inspectExtension(path: string): Promise<ExtensionConfiguration>;
  registerExtension(
    command: ExtensionRegisterCommand,
  ): Promise<SettingsV2Receipt>;
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
      tools: [
        {
          id: "tool.files.read",
          name: "Project file read",
          enabled: false,
          requiresProject: true,
          credentialBindings: [],
          configuration: {
            authorityMode: "project_files",
            effect: "read",
            maximumBytes: 65_536,
          },
        },
        {
          id: "tool.files.search",
          name: "Project file search",
          enabled: false,
          requiresProject: true,
          credentialBindings: [],
          configuration: {
            authorityMode: "project_files",
            effect: "search",
            maximumResults: 512,
          },
        },
        {
          id: "tool.files.edit",
          name: "Project file edit",
          enabled: false,
          requiresProject: true,
          credentialBindings: [],
          configuration: {
            authorityMode: "project_files",
            effect: "write",
            requiresApproval: true,
            maximumBytes: 1_048_576,
          },
        },
        {
          id: "tool.shell.host",
          name: "Host shell",
          enabled: false,
          requiresProject: false,
          credentialBindings: [],
          configuration: {
            authorityMode: "host_shell",
            requiresApproval: true,
            timeoutSeconds: 30,
            maximumOutputBytes: 262_144,
          },
        },
        {
          id: "tool.python.host",
          name: "Host Python",
          enabled: false,
          requiresProject: false,
          credentialBindings: [],
          configuration: {
            authorityMode: "host_python",
            requiresApproval: true,
            isolatedInterpreter: true,
            timeoutSeconds: 30,
            maximumOutputBytes: 262_144,
          },
        },
        {
          id: "tool.web_search",
          name: "Web search",
          enabled: false,
          requiresProject: false,
          credentialBindings: [],
          configuration: {
            backend: "automatic",
            credentialBackend: "deepseek",
            providerTier: "automatic",
            maximumResults: 10,
            requestTimeoutSeconds: 30,
            maximumRetries: 1,
            keylessFallback: true,
            keylessRescue: true,
            cacheEnabled: true,
            cacheTtlMinutes: 20,
            searxngBaseUrl: "",
            providerBaseUrl: "",
            parallelSearchMode: "agentic",
            xaiModel: "grok-build-0.1",
            xaiAllowedDomains: [],
            xaiExcludedDomains: [],
            deepseekBaseUrl: "https://api.deepseek.com",
            deepseekModel: "deepseek-v4-flash",
            deepseekMaximumOutputTokens: 4_096,
          },
        },
      ],
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
    },
    providerHealth: [],
  };
}
