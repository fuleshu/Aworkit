import {
  settingsConfigurationV2Schema,
  validateSettingsConfiguration,
  type ConnectionConfiguration,
  type ExternalAgentConfiguration,
  type McpServerConfiguration,
  type ProviderConfiguration,
  type SettingsConfigurationV2,
  type SettingsValidationIssue,
} from "../configuration";

export type SettingsSectionId = SettingsValidationIssue["section"];

export type SettingsSectionDefinition = {
  readonly id: SettingsSectionId;
  readonly label: string;
  readonly description: string;
};

export type SettingsUiIssue = SettingsValidationIssue & {
  readonly focusId?: string;
};

export const SETTINGS_SECTIONS: readonly SettingsSectionDefinition[] = [
  { id: "approvals", label: "Approvals", description: "Default approval mode and saved project permissions" },
  {
    id: "providers",
    label: "Providers & models",
    description: "Endpoints, credentials, concrete models, discovery, and tests",
  },
  {
    id: "model_tiers",
    label: "Model tiers",
    description: "Portable tier-to-model resolution",
  },
  {
    id: "credentials",
    label: "Credentials",
    description: "Write-only operating-system secret records",
  },
  {
    id: "tools",
    label: "Tools",
    description: "Built-in tools and Agent bindings",
  },
  {
    id: "tool_plugins",
    label: "Tool Plugins",
    description: "Folder-sourced plugin packages and their complete configuration",
  },
  {
    id: "extensions",
    label: "Extensions",
    description: "Manifest discovery, trust, and configuration",
  },
  {
    id: "mcp",
    label: "MCP servers",
    description: "MCP transports and secret-backed fields",
  },
  {
    id: "external_agents",
    label: "External agents",
    description: "Explicit external-agent lifecycle adapters",
  },
  {
    id: "comfyui",
    label: "ComfyUI",
    description: "ComfyUI server, workflow tools and parameter authoring",
  },
  {
    id: "data",
    label: "Data & sessions",
    description: "Local retention and portable session policy",
  },
  {
    id: "projects",
    label: "Projects",
    description: "Workspace identities and locations",
  },
  {
    id: "appearance",
    label: "Appearance",
    description: "Color mode and application text size",
  },
  {
    id: "desktop",
    label: "Desktop",
    description: "Editor used when opening a file from the conversation",
  },
];

const sectionFields = {
  approvals: "approvals",
  providers: "providers",
  model_tiers: "modelTiers",
  credentials: "credentials",
  tools: "tools",
  extensions: "extensions",
  mcp: "mcpServers",
  external_agents: "externalAgents",
  data: "data",
  projects: "projects",
  appearance: "appearance",
  desktop: "desktop",
  comfyui: "comfyui",
} as const satisfies Record<
  Exclude<SettingsSectionId, "tool_plugins">,
  keyof SettingsConfigurationV2
>;

/**
 * One stored `mcpServers` array holds two user-visible groups: plugin-backed
 * connections, configured with their package in Tool Plugins, and general MCP
 * servers, configured on the MCP servers tab. They share a field but never a
 * section, so both the dirty set and rebasing split them explicitly.
 */
function pluginServers(
  servers: readonly McpServerConfiguration[],
): readonly McpServerConfiguration[] {
  return servers.filter((server) => server.plugin !== undefined);
}

function generalServers(
  servers: readonly McpServerConfiguration[],
): readonly McpServerConfiguration[] {
  return servers.filter((server) => server.plugin === undefined);
}

/** Returns the domains that differ from the last canonical projection. */
export function dirtySettingsSections(
  draft: SettingsConfigurationV2,
  canonical: SettingsConfigurationV2,
): ReadonlySet<SettingsSectionId> {
  const dirty = new Set<SettingsSectionId>();
  for (const { id } of SETTINGS_SECTIONS) {
    if (id === "mcp") {
      if (
        differs(
          generalServers(draft.mcpServers),
          generalServers(canonical.mcpServers),
        )
      )
        dirty.add("mcp");
      continue;
    }
    if (id === "tool_plugins") {
      if (
        differs(
          pluginServers(draft.mcpServers),
          pluginServers(canonical.mcpServers),
        )
      )
        dirty.add("tool_plugins");
      continue;
    }
    const field = sectionFields[id];
    if (differs(draft[field], canonical[field])) dirty.add(id);
  }
  return dirty;
}

/** Rebases only locally edited domains onto a newer canonical projection. */
export function rebaseSettingsDraft(
  canonical: SettingsConfigurationV2,
  draft: SettingsConfigurationV2,
  dirty: ReadonlySet<SettingsSectionId>,
): SettingsConfigurationV2 {
  const next = structuredClone(canonical);
  for (const section of dirty) {
    if (section === "mcp" || section === "tool_plugins") continue;
    const field = sectionFields[section];
    Object.assign(next, { [field]: structuredClone(draft[field]) });
  }
  if (dirty.has("mcp") || dirty.has("tool_plugins")) {
    next.mcpServers = [
      ...(dirty.has("tool_plugins")
        ? pluginServers(draft.mcpServers)
        : pluginServers(next.mcpServers)),
      ...(dirty.has("mcp")
        ? generalServers(draft.mcpServers)
        : generalServers(next.mcpServers)),
    ];
  }
  return next;
}

/**
 * Re-applies the trusted core's opaque reference rewrite after whole-section
 * dirty-draft rebasing. Only references change; every unrelated local edit is
 * preserved exactly. The replacement ref must come from the accepted exact
 * credential command receipt; this helper never infers it from a snapshot.
 * Previous canonical bindings are also considered so a locally removed
 * binding cannot preserve capability metadata invalidated by the replacement.
 */
export function reconcileCredentialReplacementDraft(
  draft: SettingsConfigurationV2,
  previousCredentialRef: string | null,
  replacementCredentialRef: string,
  previousCanonical: SettingsConfigurationV2,
  latestCanonical: SettingsConfigurationV2,
): SettingsConfigurationV2 {
  if (previousCredentialRef === null) return draft;

  // The fresh credential metadata only exists in the canonical snapshot after
  // the mutation; carry it into the draft so the rewritten consumer
  // references stay resolvable and the draft remains saveable.
  const freshCredential = latestCanonical.credentials.find(
    ({ credentialRef }) => credentialRef === replacementCredentialRef,
  );
  return {
    ...draft,
    credentials: [
      ...draft.credentials.filter(
        ({ credentialRef }) =>
          credentialRef !== previousCredentialRef &&
          credentialRef !== replacementCredentialRef,
      ),
      ...(freshCredential === undefined ? [] : [freshCredential]),
    ],
    providers: draft.providers.map((provider) => ({
      ...provider,
      credentialRef:
        provider.credentialRef === previousCredentialRef
          ? replacementCredentialRef
          : provider.credentialRef,
    })),
    tools: draft.tools.map((tool) => ({
      ...tool,
      credentialBindings: replaceCredentialBindings(
        tool.credentialBindings,
        previousCredentialRef,
        replacementCredentialRef,
      ),
    })),
    mcpServers: draft.mcpServers.map((server) => ({
      ...server,
      transport: replaceConnectionCredentialReferences(
        server.transport,
        previousCredentialRef,
        replacementCredentialRef,
      ),
    })),
    externalAgents: draft.externalAgents.map((agent) => {
      const previousCanonicalAgent = previousCanonical.externalAgents.find(
        ({ id }) => id === agent.id,
      );
      const invalidatesCapabilities =
        externalAgentReferencesCredential(agent, previousCredentialRef) ||
        (previousCanonicalAgent !== undefined &&
          externalAgentReferencesCredential(
            previousCanonicalAgent,
            previousCredentialRef,
          ));
      return {
        ...agent,
        connection: replaceConnectionCredentialReferences(
          agent.connection,
          previousCredentialRef,
          replacementCredentialRef,
        ),
        credentialBindings: replaceCredentialBindings(
          agent.credentialBindings,
          previousCredentialRef,
          replacementCredentialRef,
        ),
        capabilities: invalidatesCapabilities
          ? {
              progress: false,
              continuation: false,
              cancellation: false,
              approvals: false,
            }
          : agent.capabilities,
      };
    }),
  };
}

/** Returns every canonical or unsaved consumer of an opaque credential ref. */
export function credentialReferencePaths(
  draft: SettingsConfigurationV2,
  credentialRef: string,
): readonly string[] {
  const paths: string[] = [];
  for (const provider of draft.providers) {
    if (provider.credentialRef === credentialRef)
      paths.push(`provider ${provider.name}`);
  }
  for (const tool of draft.tools) {
    for (const binding of tool.credentialBindings) {
      if (binding.credentialRef === credentialRef)
        paths.push(`tool ${tool.name} binding ${binding.name}`);
    }
  }
  for (const server of draft.mcpServers) {
    for (const binding of connectionCredentialBindings(server.transport)) {
      if (binding.credentialRef === credentialRef)
        paths.push(`MCP server ${server.name} binding ${binding.name}`);
    }
  }
  for (const agent of draft.externalAgents) {
    for (const binding of connectionCredentialBindings(agent.connection)) {
      if (binding.credentialRef === credentialRef)
        paths.push(`external agent ${agent.name} connection ${binding.name}`);
    }
    for (const binding of agent.credentialBindings) {
      if (binding.credentialRef === credentialRef)
        paths.push(`external agent ${agent.name} binding ${binding.name}`);
    }
  }
  return paths;
}

type CredentialBinding = {
  readonly name: string;
  readonly credentialRef: string;
  readonly field: string;
};

function replaceCredentialBindings<T extends CredentialBinding>(
  bindings: readonly T[],
  previousCredentialRef: string,
  replacementCredentialRef: string,
): T[] {
  return bindings.map((binding) =>
    binding.credentialRef === previousCredentialRef
      ? { ...binding, credentialRef: replacementCredentialRef }
      : binding,
  );
}

function replaceConnectionCredentialReferences(
  connection: ConnectionConfiguration,
  previousCredentialRef: string,
  replacementCredentialRef: string,
): ConnectionConfiguration {
  return connection.transport === "http"
    ? {
        ...connection,
        headers: replaceCredentialBindings(
          connection.headers,
          previousCredentialRef,
          replacementCredentialRef,
        ),
      }
    : {
        ...connection,
        env: replaceCredentialBindings(
          connection.env,
          previousCredentialRef,
          replacementCredentialRef,
        ),
      };
}

function connectionCredentialBindings(
  connection: ConnectionConfiguration,
): readonly CredentialBinding[] {
  return connection.transport === "http" ? connection.headers : connection.env;
}

function externalAgentReferencesCredential(
  agent: ExternalAgentConfiguration,
  credentialRef: string,
): boolean {
  return [
    ...connectionCredentialBindings(agent.connection),
    ...agent.credentialBindings,
  ].some((binding) => binding.credentialRef === credentialRef);
}

/** Combines structural, cross-reference, and live JSON-editor validation. */
export function settingsDraftIssues(
  draft: SettingsConfigurationV2,
  jsonErrors: Readonly<Record<string, string>>,
): readonly SettingsUiIssue[] {
  const parsed = settingsConfigurationV2Schema.safeParse(draft);
  const issues: SettingsUiIssue[] = parsed.success
    ? [
        ...validateSettingsConfiguration(parsed.data).map((issue) =>
          decorateValidationIssue(parsed.data, issue),
        ),
        ...freeformSecretIssues(parsed.data),
      ]
    : parsed.error.issues.map((issue) => ({
        section: sectionForSchemaPath(draft, issue.path.map(String)),
        path: issue.path.map(String).join("."),
        message: issue.message,
        focusId: focusIdForSchemaPath(draft, issue.path.map(String)),
      }));
  for (const [focusId, message] of Object.entries(jsonErrors)) {
    issues.push({
      section: sectionForJsonEditor(draft, focusId),
      path: focusId,
      message,
      focusId,
    });
  }
  return issues;
}

function decorateValidationIssue(
  draft: SettingsConfigurationV2,
  issue: SettingsValidationIssue,
): SettingsUiIssue {
  const prefix = "externalAgents.";
  const suffix = ".capabilities";
  if (issue.path.startsWith(prefix) && issue.path.endsWith(suffix)) {
    const agentId = issue.path.slice(prefix.length, -suffix.length);
    return { ...issue, focusId: `${agentId}-clear-capabilities` };
  }
  // A cross-reference issue names a stored location, so the same field mapping
  // the schema issues use points at the editor the user has to change.
  if (issue.section === "mcp" || issue.section === "tool_plugins") {
    const serverId = issue.path.startsWith("mcpServers.")
      ? issue.path.slice("mcpServers.".length).split(".")[0]
      : undefined;
    const server = draft.mcpServers.find((entry) => entry.id === serverId);
    if (server !== undefined)
      return { ...issue, section: server.plugin ? "tool_plugins" : "mcp" };
    return issue;
  }
  return {
    ...issue,
    focusId: focusIdForSchemaPath(draft, issue.path.split(".")),
  };
}

function freeformSecretIssues(
  draft: SettingsConfigurationV2,
): SettingsUiIssue[] {
  const candidates: {
    readonly section: SettingsSectionId;
    readonly path: string;
    readonly focusId: string;
    readonly value: Readonly<Record<string, unknown>>;
  }[] = [];
  for (const provider of draft.providers) {
    candidates.push({
      section: "providers",
      path: `providers.${provider.id}.configuration`,
      focusId: `${provider.id}-unsupported-configuration`,
      value: provider.configuration,
    });
    for (const model of provider.models) {
      candidates.push({
        section: "providers",
        path: `providers.${provider.id}.models.${model.id}.parameters`,
        focusId: `${provider.id}-${model.id}-parameters`,
        value: model.parameters,
      });
    }
  }
  for (const tool of draft.tools)
    candidates.push({
      section: "tools",
      path: `tools.${tool.id}.configuration`,
      focusId: `${tool.id}-configuration`,
      value: tool.configuration,
    });
  for (const extension of draft.extensions)
    candidates.push({
      section: "extensions",
      path: `extensions.${extension.id}.configuration`,
      focusId: `${extension.id}-configuration`,
      value: extension.configuration,
    });
  for (const agent of draft.externalAgents)
    candidates.push({
      section: "external_agents",
      path: `externalAgents.${agent.id}.configuration`,
      focusId: `${agent.id}-configuration`,
      value: agent.configuration,
    });
  return candidates.flatMap((candidate) => {
    const secretKey = findSecretLikeKey(candidate.value);
    return secretKey === null
      ? []
      : [
          {
            section: candidate.section,
            path: candidate.path,
            focusId: candidate.focusId,
            message: `Secret-like JSON field ${secretKey} is forbidden; store its value as a credential binding.`,
          },
        ];
  });
}

function findSecretLikeKey(value: unknown): string | null {
  if (Array.isArray(value)) {
    for (const item of value) {
      const result = findSecretLikeKey(item);
      if (result !== null) return result;
    }
    return null;
  }
  if (value === null || typeof value !== "object") return null;
  for (const [key, nested] of Object.entries(value)) {
    const normalized = key.replaceAll(/[^a-zA-Z0-9]/gu, "").toLowerCase();
    if (
      [
        "apikey",
        "accesstoken",
        "authtoken",
        "authorization",
        "authheader",
        "bearertoken",
        "clientsecret",
        "password",
        "passwd",
        "privatekey",
        "secret",
      ].some((marker) => normalized.includes(marker)) ||
      normalized === "token" ||
      normalized.endsWith("tokenvalue") ||
      normalized === "credential" ||
      normalized.endsWith("credentials") ||
      normalized.includes("credentialref")
    )
      return key;
    const result = findSecretLikeKey(nested);
    if (result !== null) return result;
  }
  return null;
}

/** Stable fingerprint attached to native unsaved-provider operations. */
export function providerDraftFingerprint(
  provider: ProviderConfiguration,
): string {
  return JSON.stringify(provider);
}

/** Stable fingerprint attached to native unsaved-MCP operations. */
export function mcpDraftFingerprint(server: McpServerConfiguration): string {
  return JSON.stringify(server);
}

/** Stable fingerprint for another exact secret-free Settings draft record. */
export function settingsRecordFingerprint(record: unknown): string {
  return JSON.stringify(record);
}

function differs(left: unknown, right: unknown): boolean {
  return JSON.stringify(left) !== JSON.stringify(right);
}

function sectionFromSchemaPath(
  path: readonly PropertyKey[],
): SettingsSectionId {
  switch (String(path[0] ?? "")) {
    case "modelTiers":
      return "model_tiers";
    case "mcpServers":
      return "mcp";
    case "externalAgents":
      return "external_agents";
    case "comfyui":
      return "comfyui";
    case "credentials":
    case "approvals":
    case "tools":
    case "extensions":
      return String(path[0]) as SettingsSectionId;
    case "data":
    case "projects":
    case "appearance":
    case "desktop":
    case "providers":
      return String(path[0]) as SettingsSectionId;
    default:
      return "providers";
  }
}

/**
 * A stored `mcpServers` entry is shown either on the MCP servers tab or, when it
 * is backed by a plugin package, inside Tool Plugins. Zod paths use the array
 * index, so resolve the entry to decide where its problem belongs.
 */
function sectionForSchemaPath(
  draft: SettingsConfigurationV2,
  path: readonly string[],
): SettingsSectionId {
  const section = sectionFromSchemaPath(path);
  if (section === "mcp" && path[0] === "mcpServers") {
    const server = draft.mcpServers[Number(path[1])];
    if (server?.plugin !== undefined) return "tool_plugins";
  }
  return section;
}

function focusIdForSchemaPath(
  draft: SettingsConfigurationV2,
  path: readonly string[],
): string | undefined {
  if (path[0] === "projects") {
    const project = draft.projects[Number(path[1])];
    if (project === undefined) return undefined;
    if (path[2] === "name") return `${project.id}-name`;
    if (path[2] === "workspace" && path[3] === "kind")
      return `${project.id}-kind`;
    if (path[2] === "workspace" && path[3] === "location")
      return `${project.id}-location`;
    return undefined;
  }
  if (path[0] === "desktop")
    return path[1] === "editor" ? "desktop-editor" : undefined;
  if (path[0] !== "providers") return undefined;
  const provider = draft.providers[Number(path[1])];
  if (provider === undefined) return undefined;
  if (path[2] === "baseUrl") return `${provider.id}-base-url`;
  if (path[2] === "name") return `${provider.id}-name`;
  if (path[2] !== "models") return undefined;
  const model = provider.models[Number(path[3])];
  if (model === undefined) return undefined;
  const suffix: Readonly<Record<string, string>> = {
    name: "name",
    remoteId: "remote",
    contextWindow: "context",
    maxOutputTokens: "output",
    capabilities: "capabilities",
  };
  const field = suffix[path[4] ?? ""];
  return field === undefined ? undefined : `${provider.id}-${model.id}-${field}`;
}

function sectionForJsonEditor(
  draft: SettingsConfigurationV2,
  id: string,
): SettingsSectionId {
  if (
    draft.providers.some(
      (provider) =>
        id.startsWith(`${provider.id}-`) ||
        provider.models.some((model) =>
          id.startsWith(`${provider.id}-${model.id}-`),
        ),
    )
  )
    return "providers";
  if (draft.tools.some((item) => id.startsWith(`${item.id}-`))) return "tools";
  if (draft.extensions.some((item) => id.startsWith(`${item.id}-`)))
    return "extensions";
  if (draft.externalAgents.some((item) => id.startsWith(`${item.id}-`)))
    return "external_agents";
  return "providers";
}
