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
    description: "Built-in tool availability and bindings",
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
];

const sectionFields = {
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
} as const satisfies Record<SettingsSectionId, keyof SettingsConfigurationV2>;

/** Returns the domains that differ from the last canonical projection. */
export function dirtySettingsSections(
  draft: SettingsConfigurationV2,
  canonical: SettingsConfigurationV2,
): ReadonlySet<SettingsSectionId> {
  return new Set(
    SETTINGS_SECTIONS.filter(({ id }) =>
      differs(draft[sectionFields[id]], canonical[sectionFields[id]]),
    ).map(({ id }) => id),
  );
}

/** Rebases only locally edited domains onto a newer canonical projection. */
export function rebaseSettingsDraft(
  canonical: SettingsConfigurationV2,
  draft: SettingsConfigurationV2,
  dirty: ReadonlySet<SettingsSectionId>,
): SettingsConfigurationV2 {
  const next = structuredClone(canonical);
  for (const section of dirty) {
    const field = sectionFields[section];
    Object.assign(next, { [field]: structuredClone(draft[field]) });
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
        ...validateSettingsConfiguration(parsed.data).map(decorateValidationIssue),
        ...freeformSecretIssues(parsed.data),
      ]
    : parsed.error.issues.map((issue) => ({
        section: sectionFromSchemaPath(issue.path),
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
  issue: SettingsValidationIssue,
): SettingsUiIssue {
  const prefix = "externalAgents.";
  const suffix = ".capabilities";
  if (issue.path.startsWith(prefix) && issue.path.endsWith(suffix)) {
    const agentId = issue.path.slice(prefix.length, -suffix.length);
    return { ...issue, focusId: `${agentId}-clear-capabilities` };
  }
  return issue;
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
      focusId: `${provider.id}-configuration`,
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
        "credential",
        "password",
        "passwd",
        "privatekey",
        "secret",
        "token",
      ].some((marker) => normalized.includes(marker))
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
    case "credentials":
    case "tools":
    case "extensions":
    case "data":
    case "projects":
    case "appearance":
    case "providers":
      return String(path[0]) as SettingsSectionId;
    default:
      return "providers";
  }
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
