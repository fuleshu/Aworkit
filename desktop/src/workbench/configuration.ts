import { z } from "zod";

const stableIdSchema = z
  .string()
  .trim()
  .min(1)
  .max(256)
  .refine((value) => !/[\u0000-\u001f\u007f]/u.test(value), {
    message: "Identifiers cannot contain control characters.",
  });

const credentialBindingSchema = z
  .object({
    name: stableIdSchema,
    credentialRef: stableIdSchema,
    field: stableIdSchema,
  })
  .strict();

type CredentialBindingConfiguration = z.infer<
  typeof credentialBindingSchema
>;

export const modelTargetSchema = z
  .object({ providerId: stableIdSchema, modelId: stableIdSchema })
  .strict();

export const DEFAULT_PROVIDER_REQUEST_TIMEOUT_SECONDS = 300;
export const MAXIMUM_PROVIDER_REQUEST_TIMEOUT_SECONDS = 3_600;
export const DEFAULT_MAXIMUM_TOOL_OUTPUT_BYTES = 65_536;
export const MINIMUM_MAXIMUM_TOOL_OUTPUT_BYTES = 1_024;
export const MAXIMUM_MAXIMUM_TOOL_OUTPUT_BYTES = 524_288;

const providerRuntimeConfigurationSchema = z
  .object({
    requestTimeoutSeconds: z
      .number()
      .int()
      .min(1)
      .max(MAXIMUM_PROVIDER_REQUEST_TIMEOUT_SECONDS)
      .optional(),
    maximumToolOutputBytes: z
      .number()
      .int()
      .min(MINIMUM_MAXIMUM_TOOL_OUTPUT_BYTES)
      .max(MAXIMUM_MAXIMUM_TOOL_OUTPUT_BYTES)
      .optional(),
  })
  // Preserve older provider metadata so Settings can render it and let the
  // user remove it. The native runtime still rejects every unconsumed field.
  .catchall(z.unknown());

export const modelConfigurationSchema = z
  .object({
    id: stableIdSchema,
    name: z.string().trim().min(1).max(256),
    remoteId: z.string().trim().min(1).max(512),
    enabled: z.boolean(),
    contextWindow: z.number().int().positive().nullable().optional(),
    maxOutputTokens: z.number().int().positive().nullable().optional(),
    capabilities: z.array(stableIdSchema),
    parameters: z.record(z.string(), z.unknown()),
  })
  .strict();

export const providerConfigurationSchema = z
  .object({
    id: stableIdSchema,
    name: z.string().trim().min(1).max(256),
    kind: stableIdSchema,
    baseUrl: z.string().trim().min(1).max(4096),
    enabled: z.boolean(),
    credentialRef: stableIdSchema.nullable().optional(),
    models: z.array(modelConfigurationSchema),
    configuration: providerRuntimeConfigurationSchema,
  })
  .strict();

const modelTierResolutionSchema = z.discriminatedUnion("strategy", [
  z.object({ strategy: z.literal("unconfigured") }).strict(),
  z
    .object({ strategy: z.literal("exact"), target: modelTargetSchema })
    .strict(),
  z
    .object({
      strategy: z.literal("fallback"),
      targets: z.array(modelTargetSchema).min(2),
    })
    .strict(),
  z
    .object({
      strategy: z.literal("policy"),
      candidates: z.array(modelTargetSchema).min(1),
      preference: z.enum(["quality", "latency", "cost"]),
    })
    .strict(),
]);

export const modelTierConfigurationSchema = z
  .object({
    id: stableIdSchema,
    name: z.string().trim().min(1).max(256),
    kind: z.enum(["standard", "custom"]),
    resolution: modelTierResolutionSchema,
  })
  .strict();

export const credentialMetadataConfigurationSchema = z
  .object({
    credentialRef: stableIdSchema,
    label: z.string().trim().min(1).max(256),
    kind: stableIdSchema,
    fieldNames: z.array(stableIdSchema),
    revision: z.number().int().positive(),
    boundProviderId: stableIdSchema.nullable().optional(),
    boundEndpoint: z.string().max(4096).nullable().optional(),
  })
  .strict();

const BUILT_IN_TOOL_CONFIGURATION_KEYS: Readonly<Record<string, readonly string[]>> = {
  "tool.files.read": ["authorityMode", "effect", "maximumBytes"],
  "tool.files.search": ["authorityMode", "effect", "maximumResults"],
  "tool.files.edit": [
    "authorityMode",
    "effect",
    "maximumBytes",
    "requiresApproval",
  ],
  "tool.shell.host": [
    "authorityMode",
    "maximumOutputBytes",
    "requiresApproval",
    "timeoutSeconds",
  ],
  "tool.python.host": [
    "authorityMode",
    "isolatedInterpreter",
    "maximumOutputBytes",
    "requiresApproval",
    "timeoutSeconds",
  ],
  "tool.web_search": [
    "backend",
    "cacheEnabled",
    "cacheTtlMinutes",
    "credentialBackend",
    "deepseekBaseUrl",
    "deepseekMaximumOutputTokens",
    "deepseekModel",
    "keylessFallback",
    "keylessRescue",
    "maximumResults",
    "maximumRetries",
    "parallelSearchMode",
    "providerBaseUrl",
    "providerTier",
    "requestTimeoutSeconds",
    "searxngBaseUrl",
    "xaiAllowedDomains",
    "xaiExcludedDomains",
    "xaiModel",
  ],
};

export const builtInToolConfigurationSchema = z
  .object({
    id: stableIdSchema,
    name: z.string().trim().min(1).max(256),
    enabled: z.boolean(),
    requiresProject: z.boolean(),
    credentialBindings: z.array(credentialBindingSchema),
    configuration: z.record(z.string(), z.unknown()),
  })
  .strict()
  .superRefine((tool, context) => {
    const expected = BUILT_IN_TOOL_CONFIGURATION_KEYS[tool.id];
    if (expected === undefined) return;
    const actual = Object.keys(tool.configuration).sort();
    if (
      actual.length !== expected.length ||
      actual.some((key, index) => key !== [...expected].sort()[index])
    )
      context.addIssue({
        code: z.ZodIssueCode.custom,
        path: ["configuration"],
        message:
          "Configuration must contain exactly the installed adapter fields.",
      });
    const value = tool.configuration;
    const validImplementedContract =
      tool.id === "tool.files.read"
        ? tool.requiresProject === true &&
          value.authorityMode === "project_files" &&
          value.effect === "read" &&
          typeof value.maximumBytes === "number" &&
          Number.isInteger(value.maximumBytes) &&
          value.maximumBytes >= 1 &&
          value.maximumBytes <= 65_536
        : tool.id === "tool.files.search"
          ? tool.requiresProject === true &&
            value.authorityMode === "project_files" &&
            value.effect === "search" &&
            typeof value.maximumResults === "number" &&
            Number.isInteger(value.maximumResults) &&
            value.maximumResults >= 1 &&
            value.maximumResults <= 512
          : tool.id === "tool.web_search"
            ? webSearchConfigurationIsValid(tool)
            : true;
    if (!validImplementedContract)
      context.addIssue({
        code: z.ZodIssueCode.custom,
        path: ["configuration"],
        message:
          "Configuration exceeds or contradicts the installed persistence-safe adapter contract.",
      });
  });

function webSearchConfigurationIsValid(tool: {
  readonly requiresProject: boolean;
  readonly credentialBindings: readonly CredentialBindingConfiguration[];
  readonly configuration: Readonly<Record<string, unknown>>;
}): boolean {
  const value = tool.configuration;
  const backend = value.backend;
  const validBackend =
    backend === "automatic" ||
    backend === "keyless" ||
    backend === "duckduckgo" ||
    backend === "searxng" ||
    backend === "exa" ||
    backend === "parallel" ||
    backend === "firecrawl" ||
    backend === "tavily" ||
    backend === "brave" ||
    backend === "keenable" ||
    backend === "xai" ||
    backend === "deepseek";
  const credentialBackend = value.credentialBackend;
  const validCredentialBackend =
    credentialBackend === "exa" ||
    credentialBackend === "parallel" ||
    credentialBackend === "firecrawl" ||
    credentialBackend === "tavily" ||
    credentialBackend === "brave" ||
    credentialBackend === "keenable" ||
    credentialBackend === "xai" ||
    credentialBackend === "deepseek";
  const providerTier = value.providerTier;
  const validProviderTier =
    providerTier === "automatic" || providerTier === "free" || providerTier === "paid";
  const integerIn = (candidate: unknown, minimum: number, maximum: number) =>
    typeof candidate === "number" &&
    Number.isInteger(candidate) &&
    candidate >= minimum &&
    candidate <= maximum;
  const validEndpoint = (candidate: unknown, allowLoopbackHttp: boolean) => {
    if (typeof candidate !== "string") return false;
    if (candidate.trim() === "") return true;
    try {
      const parsed = new URL(candidate);
      const loopback =
        parsed.hostname === "localhost" ||
        parsed.hostname === "127.0.0.1" ||
        parsed.hostname === "[::1]";
      return (
        parsed.username === "" &&
        parsed.password === "" &&
        parsed.search === "" &&
        parsed.hash === "" &&
        (parsed.protocol === "https:" ||
          (allowLoopbackHttp && parsed.protocol === "http:" && loopback))
      );
    } catch {
      return false;
    }
  };
  const validDomains = (candidate: unknown) =>
    Array.isArray(candidate) &&
    candidate.length <= 5 &&
    candidate.every(
      (domain) =>
        typeof domain === "string" &&
        domain.length >= 1 &&
        domain.length <= 253 &&
        domain.split(".").every(
          (label) =>
            label.length >= 1 &&
            label.length <= 63 &&
            !label.startsWith("-") &&
            !label.endsWith("-") &&
            /^[a-zA-Z0-9-]+$/u.test(label),
        ),
    );
  const dualTierBackend =
    backend === "exa" ||
    backend === "parallel" ||
    backend === "firecrawl" ||
    backend === "tavily" ||
    backend === "keenable";
  const requiresKey =
    backend === "brave" ||
    backend === "xai" ||
    backend === "deepseek" ||
    (dualTierBackend && providerTier === "paid");
  const forbidsKey =
    backend === "keyless" ||
    backend === "duckduckgo" ||
    backend === "searxng" ||
    (dualTierBackend && providerTier === "free");
  const credentialsValid =
    tool.credentialBindings.length <= 1 &&
    tool.credentialBindings.every(({ name }) => name === "api_key") &&
    (!requiresKey || tool.credentialBindings.length === 1) &&
    (!forbidsKey || tool.credentialBindings.length === 0);
  return (
    tool.requiresProject === false &&
    validBackend &&
    validCredentialBackend &&
    validProviderTier &&
    (backend !== "automatic" || providerTier === "automatic") &&
    (!(backend === "keyless" || backend === "duckduckgo" || backend === "searxng") ||
      providerTier === "automatic") &&
    (!(backend === "brave" || backend === "xai" || backend === "deepseek") ||
      providerTier !== "free") &&
    credentialsValid &&
    integerIn(value.maximumResults, 1, 100) &&
    integerIn(value.requestTimeoutSeconds, 5, 120) &&
    integerIn(value.maximumRetries, 0, 3) &&
    typeof value.keylessFallback === "boolean" &&
    typeof value.keylessRescue === "boolean" &&
    (!value.keylessRescue || value.keylessFallback === true) &&
    typeof value.cacheEnabled === "boolean" &&
    integerIn(value.cacheTtlMinutes, 1, 1_440) &&
    validEndpoint(value.searxngBaseUrl, true) &&
    validEndpoint(value.providerBaseUrl, true) &&
    validEndpoint(value.deepseekBaseUrl, false) &&
    (backend !== "searxng" ||
      (typeof value.searxngBaseUrl === "string" &&
        value.searxngBaseUrl.trim() !== "")) &&
    (backend !== "deepseek" ||
      (typeof value.deepseekBaseUrl === "string" &&
        value.deepseekBaseUrl.trim() !== "")) &&
    (value.parallelSearchMode === "fast" ||
      value.parallelSearchMode === "one-shot" ||
      value.parallelSearchMode === "agentic") &&
    typeof value.xaiModel === "string" &&
    value.xaiModel.trim().length >= 1 &&
    value.xaiModel.length <= 256 &&
    validDomains(value.xaiAllowedDomains) &&
    validDomains(value.xaiExcludedDomains) &&
    !(
      (value.xaiAllowedDomains as unknown[]).length > 0 &&
      (value.xaiExcludedDomains as unknown[]).length > 0
    ) &&
    typeof value.deepseekModel === "string" &&
    value.deepseekModel.trim().length >= 1 &&
    value.deepseekModel.length <= 256 &&
    integerIn(value.deepseekMaximumOutputTokens, 256, 16_384)
  );
}

export const extensionConfigurationSchema = z
  .object({
    id: stableIdSchema,
    name: z.string().trim().min(1).max(256),
    version: z.string().trim().min(1).max(256),
    status: z.enum(["discovered", "installed", "incompatible"]),
    enabled: z.boolean(),
    trustAccepted: z.boolean(),
    manifestPath: z.string().trim().min(1).max(4096),
    entryPoint: z.string().max(4096).nullable().optional(),
    contentHash: z.string().max(256).nullable().optional(),
    compatibility: z.string().max(512).nullable().optional(),
    provenance: z.string().max(2048).nullable().optional(),
    configuration: z.record(z.string(), z.unknown()),
  })
  .strict();

const namedTransportValueSchema = z
  .object({ name: stableIdSchema, credentialRef: stableIdSchema, field: stableIdSchema })
  .strict();

export const connectionConfigurationSchema = z.discriminatedUnion("transport", [
  z
    .object({
      transport: z.literal("http"),
      url: z.string().trim().min(1).max(4096),
      headers: z.array(namedTransportValueSchema),
    })
    .strict(),
  z
    .object({
      transport: z.literal("stdio"),
      command: z.string().trim().min(1).max(4096),
      args: z.array(z.string().max(16_384)),
      cwd: z.string().max(4096).nullable().optional(),
      env: z.array(namedTransportValueSchema),
    })
    .strict(),
]);

export const mcpServerConfigurationSchema = z
  .object({
    id: stableIdSchema,
    name: z.string().trim().min(1).max(256),
    enabled: z.boolean(),
    autoConnect: z.boolean(),
    transport: connectionConfigurationSchema,
  })
  .strict();

export const externalAgentConfigurationSchema = z
  .object({
    id: stableIdSchema,
    name: z.string().trim().min(1).max(256),
    adapter: stableIdSchema,
    enabled: z.boolean(),
    connection: connectionConfigurationSchema,
    credentialBindings: z.array(credentialBindingSchema),
    mcpServerIds: z.array(stableIdSchema),
    capabilities: z
      .object({
        progress: z.boolean(),
        continuation: z.boolean(),
        cancellation: z.boolean(),
        approvals: z.boolean(),
      })
      .strict(),
    configuration: z.record(z.string(), z.unknown()),
  })
  .strict();

export const dataConfigurationSchema = z
  .object({
    portableHistoryEnabled: z.boolean(),
    detailedCaptureEnabled: z.boolean(),
    detailedCaptureRetentionDays: z.number().int().positive().nullable().optional(),
    localHistoryRetentionDays: z.number().int().positive().nullable().optional(),
    portableDirectory: z.string().max(4096),
  })
  .strict();

export const projectConfigurationSchema = z
  .object({
    id: stableIdSchema,
    name: z.string().trim().min(1).max(256),
    workspace: z
      .object({
        kind: z.enum([
          "local_directory",
          "git_worktree",
          "container_mount",
          "remote",
        ]),
        location: z.string().trim().min(1).max(4096),
      })
      .strict(),
    defaultWorkflowId: stableIdSchema.nullable().optional(),
    portableHistoryEnabled: z.boolean(),
  })
  .strict();

export const appearanceConfigurationSchema = z
  .object({
    mode: z.enum(["system", "light", "dark"]),
    fontScale: z.number().min(0.75).max(2),
  })
  .strict();

export const settingsConfigurationV2Schema = z
  .object({
    schemaVersion: z.literal(2),
    providers: z.array(providerConfigurationSchema),
    modelTiers: z.array(modelTierConfigurationSchema),
    credentials: z.array(credentialMetadataConfigurationSchema),
    tools: z.array(builtInToolConfigurationSchema),
    extensions: z.array(extensionConfigurationSchema),
    mcpServers: z.array(mcpServerConfigurationSchema),
    externalAgents: z.array(externalAgentConfigurationSchema),
    data: dataConfigurationSchema,
    projects: z.array(projectConfigurationSchema),
    appearance: appearanceConfigurationSchema,
  })
  .strict();

export const providerHealthSnapshotV2Schema = z
  .object({
    providerId: stableIdSchema,
    state: z.enum([
      "unconfigured",
      "configured",
      "testing",
      "ready",
      "error",
      "disabled",
    ]),
    detail: z.string().nullable(),
  })
  .strict();

export const settingsV2SnapshotSchema = z
  .object({
    version: z.number().int().positive(),
    schemaVersion: z.literal(2),
    settings: settingsConfigurationV2Schema,
    providerHealth: z.array(providerHealthSnapshotV2Schema),
  })
  .strict();

export type ModelTarget = z.infer<typeof modelTargetSchema>;
export type ModelConfiguration = z.infer<typeof modelConfigurationSchema>;
export type ProviderConfiguration = z.infer<typeof providerConfigurationSchema>;
export type ModelTierConfiguration = z.infer<typeof modelTierConfigurationSchema>;
export type CredentialMetadataConfiguration = z.infer<
  typeof credentialMetadataConfigurationSchema
>;
export type BuiltInToolConfiguration = z.infer<
  typeof builtInToolConfigurationSchema
>;
export type ExtensionConfiguration = z.infer<
  typeof extensionConfigurationSchema
>;
export type ConnectionConfiguration = z.infer<
  typeof connectionConfigurationSchema
>;
export type McpServerConfiguration = z.infer<
  typeof mcpServerConfigurationSchema
>;
export type ExternalAgentConfiguration = z.infer<
  typeof externalAgentConfigurationSchema
>;
export type DataConfiguration = z.infer<typeof dataConfigurationSchema>;
export type ProjectConfiguration = z.infer<typeof projectConfigurationSchema>;
export type AppearanceConfiguration = z.infer<
  typeof appearanceConfigurationSchema
>;
export type SettingsConfigurationV2 = z.infer<
  typeof settingsConfigurationV2Schema
>;
export type ProviderHealthSnapshotV2 = z.infer<
  typeof providerHealthSnapshotV2Schema
>;
export type SettingsV2Snapshot = z.infer<typeof settingsV2SnapshotSchema>;

export const STANDARD_TIER_IDS = [
  "tier:fast",
  "tier:simple",
  "tier:balanced",
  "tier:quality",
] as const;

export type SettingsValidationIssue = {
  readonly section:
    | "providers"
    | "model_tiers"
    | "credentials"
    | "tools"
    | "extensions"
    | "mcp"
    | "external_agents"
    | "data"
    | "projects"
    | "appearance";
  readonly path: string;
  readonly message: string;
};

/** Validates cross-record references that individual JSON schemas cannot prove. */
export function validateSettingsConfiguration(
  settings: SettingsConfigurationV2,
): readonly SettingsValidationIssue[] {
  const issues: SettingsValidationIssue[] = [];
  const providerIds = uniqueIds(
    settings.providers.map(({ id }) => id),
    "providers",
    "providers",
    issues,
  );
  const modelKeys = new Set<string>();
  for (const provider of settings.providers) {
    const urlIssue = secretFreeHttpUrlIssue(provider.baseUrl);
    if (urlIssue !== null) {
      issues.push({
        section: "providers",
        path: `providers.${provider.id}.baseUrl`,
        message: `Provider base URL ${urlIssue}`,
      });
    }
    const modelIds = new Set<string>();
    for (const model of provider.models) {
      if (modelIds.has(model.id)) {
        issues.push({
          section: "providers",
          path: `providers.${provider.id}.models.${model.id}`,
          message: "Model IDs must be unique within a provider.",
        });
      }
      modelIds.add(model.id);
      modelKeys.add(targetKey({ providerId: provider.id, modelId: model.id }));
    }
    if (provider.enabled && !provider.models.some(({ enabled }) => enabled)) {
      issues.push({
        section: "providers",
        path: `providers.${provider.id}.enabled`,
        message: "An enabled provider must contain at least one enabled model.",
      });
    }
  }
  const credentialRefs = uniqueIds(
    settings.credentials.map(({ credentialRef }) => credentialRef),
    "credentials",
    "credentials",
    issues,
  );
  const credentialFields = new Map(
    settings.credentials.map((credential) => [
      credential.credentialRef,
      new Set(credential.fieldNames),
    ]),
  );
  for (const credential of settings.credentials) {
    if (credential.boundEndpoint == null) continue;
    const endpointIssue = secretFreeHttpUrlIssue(credential.boundEndpoint);
    if (endpointIssue !== null) {
      issues.push({
        section: "credentials",
        path: `credentials.${credential.credentialRef}.boundEndpoint`,
        message: `Credential-bound endpoint ${endpointIssue}`,
      });
    }
  }
  for (const provider of settings.providers) {
    if (
      provider.credentialRef !== null &&
      provider.credentialRef !== undefined &&
      !credentialRefs.has(provider.credentialRef)
    ) {
      issues.push({
        section: "providers",
        path: `providers.${provider.id}.credentialRef`,
        message: "Provider references an unknown credential.",
      });
    }
    const credential = settings.credentials.find(
      ({ credentialRef }) => credentialRef === provider.credentialRef,
    );
    if (
      credential?.boundProviderId != null &&
      (credential.boundProviderId !== provider.id ||
        credential.boundEndpoint !== provider.baseUrl)
    ) {
      issues.push({
        section: "providers",
        path: `providers.${provider.id}.credentialRef`,
        message:
          "Provider cannot use a credential bound to another provider or endpoint.",
      });
    }
    if (
      credential !== undefined &&
      !credential.fieldNames.includes("api_key")
    ) {
      issues.push({
        section: "providers",
        path: `providers.${provider.id}.credentialRef`,
        message:
          "Installed provider adapters require a credential with an api_key field.",
      });
    }
  }
  const tierIds = uniqueIds(
    settings.modelTiers.map(({ id }) => id),
    "model_tiers",
    "model tiers",
    issues,
  );
  for (const standardId of STANDARD_TIER_IDS) {
    const tier = settings.modelTiers.find(({ id }) => id === standardId);
    if (tier === undefined || tier.kind !== "standard") {
      issues.push({
        section: "model_tiers",
        path: `modelTiers.${standardId}`,
        message: `${standardId} must always exist as a standard tier.`,
      });
    }
  }
  for (const tier of settings.modelTiers) {
    const targets = resolutionTargets(tier.resolution);
    if (tier.resolution.strategy === "fallback" && targets.length < 2) {
      issues.push({
        section: "model_tiers",
        path: `modelTiers.${tier.id}.resolution`,
        message: "Ordered fallback requires at least two model targets.",
      });
    }
    const targetKeys = targets.map(targetKey);
    if (new Set(targetKeys).size !== targetKeys.length) {
      issues.push({
        section: "model_tiers",
        path: `modelTiers.${tier.id}.resolution`,
        message: "Tier candidates must not contain duplicate model targets.",
      });
    }
    for (const target of targets) {
      if (!providerIds.has(target.providerId) || !modelKeys.has(targetKey(target))) {
        issues.push({
          section: "model_tiers",
          path: `modelTiers.${tier.id}.resolution`,
          message: `Tier target ${target.providerId}/${target.modelId} is unresolved.`,
        });
      }
    }
  }
  void tierIds;
  const mcpIds = uniqueIds(
    settings.mcpServers.map(({ id }) => id),
    "mcp",
    "MCP servers",
    issues,
  );
  for (const server of settings.mcpServers) {
    if (server.autoConnect) {
      issues.push({
        section: "mcp",
        path: `mcpServers.${server.id}.autoConnect`,
        message:
          "Connect at launch is unavailable; this build supports only explicit one-shot Discover and Test sessions.",
      });
    }
    for (const binding of connectionBindings(server.transport)) {
      const credential = settings.credentials.find(
        ({ credentialRef }) => credentialRef === binding.credentialRef,
      );
      if (credential?.boundProviderId != null) {
        issues.push({
          section: "mcp",
          path: `mcpServers.${server.id}.transport`,
          message:
            "Provider-scoped credentials cannot be injected into an MCP server.",
        });
      }
    }
  }
  validateConnectionCredentials(settings.mcpServers, credentialFields, issues);
  for (const agent of settings.externalAgents) {
    validateConnection(
      agent.connection,
      "external_agents",
      `externalAgents.${agent.id}.connection`,
      credentialFields,
      issues,
      {
        adapterEnvironmentBindings: agent.credentialBindings,
        adapterEnvironmentPath: `externalAgents.${agent.id}.credentialBindings`,
        codexAppServer: agent.adapter === "codex_app_server",
      },
    );
    validateBindings(
      agent.credentialBindings,
      credentialFields,
      "external_agents",
      `externalAgents.${agent.id}`,
      issues,
    );
    for (const binding of [
      ...connectionBindings(agent.connection),
      ...agent.credentialBindings,
    ]) {
      const credential = settings.credentials.find(
        ({ credentialRef }) => credentialRef === binding.credentialRef,
      );
      if (credential?.boundProviderId != null) {
        issues.push({
          section: "external_agents",
          path: `externalAgents.${agent.id}.credentialBindings`,
          message:
            "Provider-scoped credentials cannot be injected into an external agent.",
        });
      }
    }
    if (
      agent.capabilities.progress ||
      agent.capabilities.continuation ||
      agent.capabilities.cancellation ||
      agent.capabilities.approvals
    ) {
      issues.push({
        section: "external_agents",
        path: `externalAgents.${agent.id}.capabilities`,
        message:
          "Negotiated capabilities are ephemeral probe output and cannot be persisted by generic or dedicated Settings commands.",
      });
    }
    if (agent.adapter === "codex_app_server") {
      if (agent.connection.transport !== "stdio") {
        issues.push({
          section: "external_agents",
          path: `externalAgents.${agent.id}.connection.transport`,
          message: "Codex App Server supports only its local STDIO transport.",
        });
      }
      if (agent.mcpServerIds.length > 0) {
        issues.push({
          section: "external_agents",
          path: `externalAgents.${agent.id}.mcpServerIds`,
          message:
            "The installed Codex handshake does not consume MCP forwarding metadata.",
        });
      }
      if (Object.keys(agent.configuration).length > 0) {
        issues.push({
          section: "external_agents",
          path: `externalAgents.${agent.id}.configuration`,
          message:
            "The installed Codex handshake does not consume adapter configuration fields.",
        });
      }
    }
    for (const serverId of agent.mcpServerIds) {
      if (!mcpIds.has(serverId)) {
        issues.push({
          section: "external_agents",
          path: `externalAgents.${agent.id}.mcpServerIds`,
          message: `External agent references unknown MCP server ${serverId}.`,
        });
      }
    }
  }
  for (const tool of settings.tools) {
    validateBindings(
      tool.credentialBindings,
      credentialFields,
      "tools",
      `tools.${tool.id}`,
      issues,
    );
    if (tool.credentialBindings.length > 0 && tool.id !== "tool.web_search") {
      issues.push({
        section: "tools",
        path: `tools.${tool.id}.credentialBindings`,
        message:
          "Installed built-in adapters do not consume credential bindings.",
      });
    }
    if (tool.id === "tool.web_search") {
      if (!webSearchConfigurationIsValid(tool)) {
        issues.push({
          section: "tools",
          path: `tools.${tool.id}.credentialBindings`,
          message:
            "The selected web-search provider tier and api_key binding do not satisfy the installed adapter contract.",
        });
      }
      for (const binding of tool.credentialBindings) {
        const credential = settings.credentials.find(
          ({ credentialRef }) => credentialRef === binding.credentialRef,
        );
        if (credential?.boundProviderId != null) {
          issues.push({
            section: "tools",
            path: `tools.${tool.id}.credentialBindings`,
            message:
              "Web search requires an unbound integration credential so it cannot cross a model-provider endpoint binding.",
          });
        }
      }
    }
  }
  uniqueIds(
    settings.extensions.map(({ id }) => id),
    "extensions",
    "extensions",
    issues,
  );
  for (const extension of settings.extensions) {
    if (extension.trustAccepted && extension.status !== "installed") {
      issues.push({
        section: "extensions",
        path: `extensions.${extension.id}.trustAccepted`,
        message:
          "Trust can be accepted only after native installation verification.",
      });
    }
    if (
      extension.enabled &&
      (extension.status !== "installed" || !extension.trustAccepted)
    ) {
      issues.push({
        section: "extensions",
        path: `extensions.${extension.id}.enabled`,
        message:
          "Enabled legacy extension metadata requires verified installation and explicit trust metadata; this build does not provide extension enablement or execution.",
      });
    }
  }
  uniqueIds(
    settings.externalAgents.map(({ id }) => id),
    "external_agents",
    "external agents",
    issues,
  );
  uniqueIds(
    settings.projects.map(({ id }) => id),
    "projects",
    "projects",
    issues,
  );
  for (const project of settings.projects) {
    if (project.portableHistoryEnabled) {
      issues.push({
        section: "projects",
        path: `projects.${project.id}.portableHistoryEnabled`,
        message:
          "Portable project history is unavailable; current Chats use local SQLite history.",
      });
    }
  }
  if (
    settings.data.portableHistoryEnabled ||
    settings.data.detailedCaptureEnabled ||
    settings.data.detailedCaptureRetentionDays != null ||
    settings.data.localHistoryRetentionDays != null
  ) {
    issues.push({
      section: "data",
      path: "data",
      message:
        "Portable history, detailed capture, and retention policies are not active in this build and must remain disabled.",
    });
  }
  if (
    settings.data.portableDirectory.trim() === "" ||
    settings.data.portableDirectory.startsWith("/") ||
    settings.data.portableDirectory.startsWith("\\") ||
    settings.data.portableDirectory.split(/[\\/]/u).includes("..")
  ) {
    issues.push({
      section: "data",
      path: "data.portableDirectory",
      message: "Portable directory must be a non-empty project-relative path.",
    });
  }
  return issues;
}

function connectionBindings(
  connection: ConnectionConfiguration,
): readonly CredentialBindingConfiguration[] {
  return connection.transport === "http"
    ? connection.headers
    : connection.env;
}

function uniqueIds(
  values: readonly string[],
  section: SettingsValidationIssue["section"],
  label: string,
  issues: SettingsValidationIssue[],
): ReadonlySet<string> {
  const result = new Set<string>();
  for (const value of values) {
    if (result.has(value)) {
      issues.push({
        section,
        path: label,
        message: `Duplicate ${label} identifier ${value}.`,
      });
    }
    result.add(value);
  }
  return result;
}

function resolutionTargets(
  resolution: ModelTierConfiguration["resolution"],
): readonly ModelTarget[] {
  switch (resolution.strategy) {
    case "unconfigured":
      return [];
    case "exact":
      return [resolution.target];
    case "fallback":
      return resolution.targets;
    case "policy":
      return resolution.candidates;
  }
}

function targetKey(target: ModelTarget): string {
  return `${target.providerId}\u0000${target.modelId}`;
}

function validateConnectionCredentials(
  servers: readonly McpServerConfiguration[],
  credentialFields: ReadonlyMap<string, ReadonlySet<string>>,
  issues: SettingsValidationIssue[],
): void {
  for (const server of servers) {
    validateConnection(
      server.transport,
      "mcp",
      `mcpServers.${server.id}`,
      credentialFields,
      issues,
      { mcp: true },
    );
  }
}

type ConnectionValidationOptions = {
  readonly mcp?: boolean;
  readonly adapterEnvironmentBindings?: readonly CredentialBindingConfiguration[];
  readonly adapterEnvironmentPath?: string;
  readonly codexAppServer?: boolean;
};

function validateConnection(
  connection: ConnectionConfiguration,
  section: SettingsValidationIssue["section"],
  path: string,
  credentialFields: ReadonlyMap<string, ReadonlySet<string>>,
  issues: SettingsValidationIssue[],
  options: ConnectionValidationOptions = {},
): void {
  if (connection.transport === "http") {
    const urlIssue = secretFreeHttpUrlIssue(connection.url);
    if (urlIssue !== null) {
      issues.push({
        section,
        path: `${path}.url`,
        message: `Integration URL ${urlIssue}`,
      });
    }
    validateHttpCredentialTargets(
      connection.headers,
      section,
      `${path}.headers`,
      issues,
      options.mcp === true,
    );
    validateEnvironmentCredentialTargets(
      options.adapterEnvironmentBindings ?? [],
      section,
      options.adapterEnvironmentPath ?? `${path}.credentialBindings`,
      issues,
    );
    validateBindings(connection.headers, credentialFields, section, path, issues);
    return;
  }
  if (options.mcp === true) {
    if (!runtimePathIsAbsolute(unquoteRuntimePath(connection.command)) && !runtimePathIsBareCommand(unquoteRuntimePath(connection.command))) {
      issues.push({
        section,
        path: `${path}.command`,
        message:
          "MCP STDIO executable must be absolute or one bare command name from PATH for the installed native adapter.",
      });
    }
  }
  for (const [index, argument] of connection.args.entries()) {
    if (argumentContainsSecret(argument)) {
      issues.push({
        section,
        path: `${path}.args.${index}`,
        message:
          "STDIO arguments cannot contain authentication or credential material; use a secret-backed environment binding.",
      });
    }
    for (const candidate of [argument, argument.split("=", 2)[1]]) {
      if (candidate === undefined || !/^https?:\/\//iu.test(candidate)) continue;
      const issue = secretFreeHttpUrlIssue(candidate);
      if (issue !== null) {
        issues.push({
          section,
          path: `${path}.args.${index}`,
          message: `STDIO URL argument ${issue}`,
        });
      }
    }
  }
  const environmentBindings = [
    ...connection.env.map((binding, index) => ({
      binding,
      path: `${path}.env.${index}.name`,
    })),
    ...(options.adapterEnvironmentBindings ?? []).map((binding, index) => ({
      binding,
      path: `${options.adapterEnvironmentPath ?? `${path}.credentialBindings`}.${index}.name`,
    })),
  ];
  if (
    options.adapterEnvironmentBindings !== undefined &&
    environmentBindings.length > 256
  ) {
    issues.push({
      section,
      path: options.adapterEnvironmentPath ?? path,
      message:
        "External-agent environment exceeds the 256-binding limit across connection.env and credentialBindings.",
    });
  }
  validateEnvironmentCredentialTargetEntries(
    environmentBindings,
    section,
    issues,
  );
  if (options.codexAppServer === true) {
    if (connection.args[0] !== "app-server") {
      issues.push({
        section,
        path: `${path}.args.0`,
        message:
          "Codex App Server arguments must begin with the explicit app-server subcommand.",
      });
    }
    if (usesNonStdioListener(connection.args)) {
      issues.push({
        section,
        path: `${path}.args`,
        message:
          "Codex App Server supports only --listen stdio or --listen stdio://.",
      });
    }
    if (
      !runtimePathIsAbsolute(connection.command) &&
      !runtimePathIsBareCommand(connection.command)
    ) {
      issues.push({
        section,
        path: `${path}.command`,
        message:
          "Codex App Server executable must be absolute or one bare command name from PATH.",
      });
    }
    if (connection.cwd != null && !runtimePathIsAbsolute(connection.cwd)) {
      issues.push({
        section,
        path: `${path}.cwd`,
        message:
          "Codex App Server working directory must be absolute when configured.",
      });
    }
  }
  validateBindings(connection.env, credentialFields, section, path, issues);
}

function validateHttpCredentialTargets(
  bindings: readonly CredentialBindingConfiguration[],
  section: SettingsValidationIssue["section"],
  path: string,
  issues: SettingsValidationIssue[],
  rejectMcpReservedHeaders: boolean,
): void {
  const names = new Set<string>();
  for (const [index, binding] of bindings.entries()) {
    const targetPath = `${path}.${index}.name`;
    if (!/^[A-Za-z0-9_-]{1,128}$/u.test(binding.name)) {
      issues.push({
        section,
        path: targetPath,
        message:
          "HTTP credential target must be at most 128 ASCII letters, digits, hyphens, or underscores.",
      });
    } else if (
      rejectMcpReservedHeaders &&
      reservedMcpHeaderName(binding.name)
    ) {
      issues.push({
        section,
        path: targetPath,
        message: `HTTP credential target ${binding.name} is reserved by the native MCP transport.`,
      });
    }
    const folded = binding.name.toLowerCase();
    if (names.has(folded)) {
      issues.push({
        section,
        path: targetPath,
        message: `HTTP credential target ${binding.name} is configured more than once; header names are case-insensitive.`,
      });
    }
    names.add(folded);
  }
}

function validateEnvironmentCredentialTargets(
  bindings: readonly CredentialBindingConfiguration[],
  section: SettingsValidationIssue["section"],
  path: string,
  issues: SettingsValidationIssue[],
): void {
  validateEnvironmentCredentialTargetEntries(
    bindings.map((binding, index) => ({
      binding,
      path: `${path}.${index}.name`,
    })),
    section,
    issues,
  );
}

function validateEnvironmentCredentialTargetEntries(
  entries: readonly {
    readonly binding: CredentialBindingConfiguration;
    readonly path: string;
  }[],
  section: SettingsValidationIssue["section"],
  issues: SettingsValidationIssue[],
): void {
  const names = new Set<string>();
  for (const { binding, path } of entries) {
    if (!/^[A-Za-z0-9_]{1,128}$/u.test(binding.name)) {
      issues.push({
        section,
        path,
        message:
          "Environment credential target must be at most 128 ASCII letters, digits, or underscores.",
      });
    }
    const folded = binding.name.toLowerCase();
    if (names.has(folded)) {
      issues.push({
        section,
        path,
        message: `Environment credential target ${binding.name} is configured more than once; names are compared case-insensitively for cross-platform portability.`,
      });
    }
    names.add(folded);
  }
}

function reservedMcpHeaderName(value: string): boolean {
  const name = value.toLowerCase();
  return (
    [
      "accept",
      "connection",
      "content-length",
      "content-type",
      "expect",
      "host",
      "proxy-authorization",
      "transfer-encoding",
      "upgrade",
      "mcp-session-id",
      "mcp-protocol-version",
      "last-event-id",
      "mcp-method",
      "mcp-name",
    ].includes(name) || name.startsWith("mcp-param-")
  );
}

function runtimePathIsAbsolute(value: string): boolean {
  if (runtimeUsesWindowsPaths()) {
    return (
      /^[A-Za-z]:[\\/]/u.test(value) ||
      /^\\\\[^\\/]+[\\/][^\\/]+(?:[\\/]|$)/u.test(value)
    );
  }
  return value.startsWith("/");
}

/** Windows users commonly paste a quoted executable path from a shell. */
function unquoteRuntimePath(value: string): string {
  const trimmed = value.trim();
  if (trimmed.length >= 2) {
    const quote = trimmed[0];
    if ((quote === '"' || quote === "'") && trimmed.at(-1) === quote)
      return trimmed.slice(1, -1);
  }
  return trimmed;
}

function runtimePathIsBareCommand(value: string): boolean {
  if (value === "" || value === "." || value === "..") return false;
  return runtimeUsesWindowsPaths()
    ? !/[\\/:]/u.test(value)
    : !value.includes("/");
}

function runtimeUsesWindowsPaths(): boolean {
  const runtimeIdentity =
    typeof navigator === "undefined"
      ? ""
      : `${navigator.userAgent} ${navigator.platform}`;
  return /(?:windows|win32|win64|wow64)/iu.test(runtimeIdentity);
}

function usesNonStdioListener(arguments_: readonly string[]): boolean {
  return (
    arguments_.some(
      (argument, index) =>
        argument === "--listen" &&
        index + 1 < arguments_.length &&
        arguments_[index + 1] !== "stdio" &&
        arguments_[index + 1] !== "stdio://",
    ) ||
    arguments_.some((argument) => {
      const transport = argument.startsWith("--listen=")
        ? argument.slice("--listen=".length)
        : null;
      return transport !== null && transport !== "stdio" && transport !== "stdio://";
    })
  );
}

function secretFreeHttpUrlIssue(value: string): string | null {
  try {
    const url = new URL(value);
    if (
      (url.protocol !== "http:" && url.protocol !== "https:") ||
      url.hostname === "" ||
      url.username !== "" ||
      url.password !== "" ||
      value.includes("?") ||
      value.includes("#")
    )
      throw new Error();
    return null;
  } catch {
    return "must be an absolute HTTP(S) URL without credentials, query, or fragment.";
  }
}

function argumentContainsSecret(value: string): boolean {
  const normalized = value.replaceAll(/[^a-zA-Z0-9]/gu, "").toLowerCase();
  return (
    [
      "apikey",
      "accesstoken",
      "authtoken",
      "authorization",
      "authheader",
      "bearertoken",
      "clientsecret",
      "credential",
      "password",
      "passwd",
      "privatekey",
      "secret",
      "token",
    ].some((marker) => normalized.includes(marker)) ||
    /^(?:sk-|ghp_|github_pat_|bearer\s)/iu.test(value)
  );
}

function validateBindings(
  bindings: readonly {
    readonly credentialRef: string;
    readonly field?: string;
  }[],
  credentialFields: ReadonlyMap<string, ReadonlySet<string>>,
  section: SettingsValidationIssue["section"],
  path: string,
  issues: SettingsValidationIssue[],
): void {
  for (const binding of bindings) {
    const fields = credentialFields.get(binding.credentialRef);
    if (fields === undefined) {
      issues.push({
        section,
        path,
        message: `Unknown credential reference ${binding.credentialRef}.`,
      });
    } else if (binding.field !== undefined && !fields.has(binding.field)) {
      issues.push({
        section,
        path,
        message: `Credential ${binding.credentialRef} has no field named ${binding.field}.`,
      });
    }
  }
}
