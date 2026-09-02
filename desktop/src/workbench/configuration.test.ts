import { describe, expect, it } from "vitest";
import {
  builtInToolConfigurationSchema,
  settingsConfigurationV2Schema,
  validateSettingsConfiguration,
  type SettingsConfigurationV2,
} from "./configuration";
import { settingsDraftIssues } from "./settings-v2/settingsDraft";

function configuration(): SettingsConfigurationV2 {
  return {
    schemaVersion: 2,
    providers: [
      {
        id: "provider.local",
        name: "Local provider",
        kind: "openai_compatible",
        baseUrl: "http://127.0.0.1:11434/v1",
        enabled: true,
        credentialRef: null,
        configuration: {
          requestTimeoutSeconds: 300,
          maximumToolOutputBytes: 65_536,
        },
        models: [
          {
            id: "model.chat",
            name: "Chat model",
            remoteId: "chat-model",
            enabled: true,
            capabilities: ["text", "tools"],
            parameters: {},
          },
        ],
      },
    ],
    modelTiers: ["fast", "simple", "balanced", "quality"].map(
      (name) => ({
        id: `tier:${name}`,
        name,
        kind: "standard" as const,
        resolution: {
          strategy: "exact" as const,
          target: { providerId: "provider.local", modelId: "model.chat" },
        },
      }),
    ),
    credentials: [],
    tools: [
      {
        id: "tool.project_files",
        name: "Project Files",
        enabled: true,
        requiresProject: true,
        credentialBindings: [],
        configuration: { mode: "read_write" },
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
  };
}

describe("Settings configuration v2", () => {
  it("validates bounded provider runtime controls", () => {
    const value = configuration();
    value.providers[0]!.configuration = {
      requestTimeoutSeconds: 300,
      maximumToolOutputBytes: 65_536,
    };
    expect(settingsConfigurationV2Schema.safeParse(value).success).toBe(true);

    value.providers[0]!.configuration = { requestTimeoutSeconds: 3_601 };
    expect(settingsConfigurationV2Schema.safeParse(value).success).toBe(false);

    value.providers[0]!.configuration = { apiStyle: "responses" };
    expect(settingsConfigurationV2Schema.safeParse(value).success).toBe(true);
  });

  it("rejects unknown built-in adapter configuration fields before save", () => {
    const parsed = builtInToolConfigurationSchema.safeParse({
      id: "tool.files.read",
      name: "Project file read",
      enabled: true,
      requiresProject: true,
      credentialBindings: [],
      configuration: {
        authorityMode: "project_files",
        effect: "read",
        maximumBytes: 65_536,
        ignoredByRuntime: true,
      },
    });
    expect(parsed.success).toBe(false);
  });

  it("parses a complete secret-free configuration and resolves its references", () => {
    const parsed = settingsConfigurationV2Schema.parse(configuration());
    expect(validateSettingsConfiguration(parsed)).toEqual([]);
    expect(JSON.stringify(parsed)).not.toMatch(/api.?key|password|secret/i);
  });

  it("keeps every standard tier present even when it is unconfigured", () => {
    const value = configuration();
    value.modelTiers = value.modelTiers.slice(0, 3);
    expect(validateSettingsConfiguration(value)).toContainEqual(
      expect.objectContaining({
        section: "model_tiers",
        path: "modelTiers.tier:quality",
      }),
    );
  });

  it("reports unresolved tier and credential references without substituting them", () => {
    const value = configuration();
    value.modelTiers[0] = {
      ...value.modelTiers[0],
      resolution: {
        strategy: "exact",
        target: { providerId: "provider.missing", modelId: "model.missing" },
      },
    };
    value.providers[0] = {
      ...value.providers[0],
      credentialRef: "credential.missing",
    };
    const issues = validateSettingsConfiguration(value);
    expect(issues.map(({ section }) => section)).toEqual(
      expect.arrayContaining(["providers", "model_tiers"]),
    );
    expect(value.modelTiers[0].resolution).toEqual({
      strategy: "exact",
      target: { providerId: "provider.missing", modelId: "model.missing" },
    });
  });

  it("requires MCP environment and header values to be credential references", () => {
    const value = configuration();
    value.mcpServers.push({
      id: "mcp.example",
      name: "Example",
      enabled: false,
      autoConnect: false,
      transport: {
        transport: "http",
        url: "https://example.test/mcp",
        headers: [
          {
            name: "Authorization",
            credentialRef: "credential.missing",
            field: "token",
          },
        ],
      },
    });
    expect(validateSettingsConfiguration(value)).toContainEqual(
      expect.objectContaining({
        section: "mcp",
        message: "Unknown credential reference credential.missing.",
      }),
    );
  });

  it("blocks invalid, reserved, and case-insensitively duplicated MCP HTTP targets", () => {
    const value = configuration();
    value.credentials.push({
      credentialRef: "credential.integration",
      label: "Integration token",
      kind: "token",
      fieldNames: ["token"],
      revision: 1,
      boundProviderId: null,
      boundEndpoint: null,
    });
    const binding = (name: string) => ({
      name,
      credentialRef: "credential.integration",
      field: "token",
    });
    value.mcpServers.push({
      id: "mcp.targets",
      name: "Target validation",
      enabled: false,
      autoConnect: false,
      transport: {
        transport: "http",
        url: "https://example.test/mcp",
        headers: [
          binding("X.Invalid"),
          binding("McP-PrOtOcOl-VeRsIoN"),
          binding("MCP-PARAM-Cursor"),
          binding("Authorization"),
          binding("authorization"),
          binding("X".repeat(129)),
        ],
      },
    });

    const parsed = settingsConfigurationV2Schema.parse(value);
    expect(parsed.mcpServers[0]!.transport).toEqual(value.mcpServers[0]!.transport);
    const messages = validateSettingsConfiguration(parsed).map(
      ({ message }) => message,
    );
    expect(messages).toEqual(
      expect.arrayContaining([
        expect.stringContaining("ASCII letters, digits, hyphens"),
        expect.stringContaining("reserved by the native MCP transport"),
        expect.stringContaining("header names are case-insensitive"),
      ]),
    );
    expect(
      messages.filter((message) =>
        message.includes("reserved by the native MCP transport"),
      ),
    ).toHaveLength(2);
  });

  it("blocks MCP STDIO targets the native consumer cannot use", () => {
    const value = configuration();
    value.credentials.push({
      credentialRef: "credential.integration",
      label: "Integration token",
      kind: "token",
      fieldNames: ["token"],
      revision: 1,
    });
    const binding = (name: string) => ({
      name,
      credentialRef: "credential.integration",
      field: "token",
    });
    value.mcpServers.push({
      id: "mcp.stdio-targets",
      name: "STDIO target validation",
      enabled: false,
      autoConnect: false,
      transport: {
        transport: "stdio",
        command: "bin/mcp-server",
        args: [],
        cwd: "relative/workspace",
        env: [
          binding("API-KEY"),
          binding("Token"),
          binding("token"),
          binding("X".repeat(129)),
        ],
      },
    });

    const messages = validateSettingsConfiguration(value).map(
      ({ message }) => message,
    );
    expect(messages).toEqual(
      expect.arrayContaining([
        expect.stringContaining("absolute or one bare command name from PATH"),
        expect.stringContaining("ASCII letters, digits, or underscores"),
        expect.stringContaining("cross-platform portability"),
      ]),
    );
  });

  it("treats direct external-agent bindings as one portable environment", () => {
    const value = configuration();
    value.credentials.push({
      credentialRef: "credential.integration",
      label: "Integration token",
      kind: "token",
      fieldNames: ["token"],
      revision: 1,
    });
    const binding = (name: string) => ({
      name,
      credentialRef: "credential.integration",
      field: "token",
    });
    value.externalAgents.push({
      id: "agent.codex",
      name: "Codex",
      adapter: "codex_app_server",
      enabled: false,
      connection: {
        transport: "stdio",
        command: "codex",
        args: ["app-server"],
        cwd: null,
        env: [binding("TOKEN")],
      },
      credentialBindings: [binding("token"), binding("API-KEY")],
      mcpServerIds: [],
      capabilities: {
        progress: true,
        continuation: false,
        cancellation: false,
        approvals: false,
      },
      configuration: {},
    });

    const messages = validateSettingsConfiguration(value).map(
      ({ message }) => message,
    );
    expect(messages).toEqual(
      expect.arrayContaining([
        expect.stringContaining("cross-platform portability"),
        expect.stringContaining("ASCII letters, digits, or underscores"),
        expect.stringContaining("ephemeral probe output"),
      ]),
    );
  });

  it("enforces the external-agent consumer's combined environment limit", () => {
    const value = configuration();
    value.credentials.push({
      credentialRef: "credential.integration",
      label: "Integration token",
      kind: "token",
      fieldNames: ["token"],
      revision: 1,
    });
    const binding = (name: string) => ({
      name,
      credentialRef: "credential.integration",
      field: "token",
    });
    value.externalAgents.push({
      id: "agent.bindings",
      name: "Bindings",
      adapter: "acp",
      enabled: false,
      connection: {
        transport: "stdio",
        command: "fixture-agent",
        args: [],
        cwd: null,
        env: Array.from({ length: 128 }, (_, index) =>
          binding(`CONNECTION_${index}`),
        ),
      },
      credentialBindings: Array.from({ length: 129 }, (_, index) =>
        binding(`ADAPTER_${index}`),
      ),
      mcpServerIds: [],
      capabilities: {
        progress: false,
        continuation: false,
        cancellation: false,
        approvals: false,
      },
      configuration: {},
    });

    expect(validateSettingsConfiguration(value)).toContainEqual(
      expect.objectContaining({
        section: "external_agents",
        message: expect.stringContaining("256-binding limit"),
      }),
    );

    value.externalAgents[0]!.credentialBindings.pop();
    expect(validateSettingsConfiguration(value)).toEqual([]);
  });

  it("matches the Codex probe's deterministic command grammar at Save", () => {
    const value = configuration();
    value.externalAgents.push({
      id: "agent.codex",
      name: "Codex",
      adapter: "codex_app_server",
      enabled: false,
      connection: {
        transport: "stdio",
        command: "codex",
        args: ["serve"],
        cwd: null,
        env: [],
      },
      credentialBindings: [],
      mcpServerIds: [],
      capabilities: {
        progress: false,
        continuation: false,
        cancellation: false,
        approvals: false,
      },
      configuration: {},
    });
    const connection = value.externalAgents[0]!.connection;
    if (connection.transport !== "stdio") throw new Error("STDIO fixture");

    expect(validateSettingsConfiguration(value)).toContainEqual(
      expect.objectContaining({
        message: expect.stringContaining("app-server subcommand"),
      }),
    );

    for (const args of [
      ["app-server", "--listen", "http://127.0.0.1:4500"],
      ["app-server", "--listen=http://127.0.0.1:4500"],
    ]) {
      connection.args = args;
      expect(validateSettingsConfiguration(value)).toContainEqual(
        expect.objectContaining({
          message: expect.stringContaining("only --listen stdio"),
        }),
      );
    }

    connection.args = ["app-server"];
    connection.command = "bin/codex";
    expect(validateSettingsConfiguration(value)).toContainEqual(
      expect.objectContaining({
        message: expect.stringContaining("one bare command name from PATH"),
      }),
    );

    connection.command = "codex";
    connection.cwd = "relative/workspace";
    expect(validateSettingsConfiguration(value)).toContainEqual(
      expect.objectContaining({
        message: expect.stringContaining("working directory must be absolute"),
      }),
    );

    connection.cwd = null;
    for (const args of [
      ["app-server", "--listen", "stdio"],
      ["app-server", "--listen=stdio://"],
    ]) {
      connection.args = args;
      expect(validateSettingsConfiguration(value)).toEqual([]);
    }
  });

  it("rejects nested secret-like freeform fields conservatively", () => {
    for (const key of [
      "apiKeyValue",
      "authHeader",
      "credentials",
      "credentialRef",
    ]) {
      const value = configuration();
      value.extensions = [{
        id: "extension.fixture",
        name: "Fixture extension",
        version: "1.0.0",
        status: "discovered",
        enabled: false,
        trustAccepted: false,
        manifestPath: "C:\\fixture\\extension.json",
        configuration: { public: { nested: { [key]: "must-not-persist" } } },
      }];
      expect(settingsDraftIssues(value, {})).toContainEqual(
        expect.objectContaining({
          section: "extensions",
          message: expect.stringContaining(key),
        }),
      );
    }
  });

  it("rejects credentials, query, and fragment in provider and integration URLs", () => {
    for (const baseUrl of [
      "https://user@example.test/v1",
      "https://user:password@example.test/v1",
      "https://example.test/v1?api_key=value",
      "https://example.test/v1#credential",
    ]) {
      const value = configuration();
      value.providers[0]!.baseUrl = baseUrl;
      expect(validateSettingsConfiguration(value)).toContainEqual(
        expect.objectContaining({
          section: "providers",
          message: expect.stringContaining(
            "without credentials, query, or fragment",
          ),
        }),
      );
    }

    const value = configuration();
    value.mcpServers.push({
      id: "mcp.unsafe",
      name: "Unsafe",
      enabled: false,
      autoConnect: false,
      transport: {
        transport: "http",
        url: "https://example.test/mcp?token=plaintext",
        headers: [],
      },
    });
    expect(validateSettingsConfiguration(value)).toContainEqual(
      expect.objectContaining({
        section: "mcp",
        message: expect.stringContaining(
          "without credentials, query, or fragment",
        ),
      }),
    );
  });

  it("rejects plaintext secrets in STDIO arguments and inactive runtime controls", () => {
    const value = configuration();
    value.mcpServers.push({
      id: "mcp.unsafe",
      name: "Unsafe",
      enabled: false,
      autoConnect: true,
      transport: {
        transport: "stdio",
        command: "/usr/bin/example",
        args: ["--auth-header=Bearer plaintext"],
        cwd: null,
        env: [],
      },
    });
    value.data.portableHistoryEnabled = true;
    const issues = validateSettingsConfiguration(value);
    expect(issues).toEqual(
      expect.arrayContaining([
        expect.objectContaining({
          section: "mcp",
          path: "mcpServers.mcp.unsafe.autoConnect",
        }),
        expect.objectContaining({
          section: "mcp",
          message: expect.stringContaining("STDIO arguments cannot contain"),
        }),
        expect.objectContaining({ section: "data", path: "data" }),
      ]),
    );
  });

  it("blocks Settings values that installed adapters would ignore or reject", () => {
    const value = configuration();
    value.credentials.push({
      credentialRef: "credential.scoped",
      label: "Wrong field",
      kind: "token",
      fieldNames: ["token"],
      revision: 1,
      boundProviderId: "provider.local",
      boundEndpoint: value.providers[0]!.baseUrl,
    });
    value.providers[0]!.credentialRef = "credential.scoped";
    value.tools[0]!.credentialBindings = [
      {
        name: "TOKEN",
        credentialRef: "credential.scoped",
        field: "token",
      },
    ];
    value.mcpServers.push({
      id: "mcp.scoped",
      name: "Scoped",
      enabled: false,
      autoConnect: false,
      transport: {
        transport: "stdio",
        command: "fixture-mcp",
        args: [],
        cwd: null,
        env: [
          {
            name: "TOKEN",
            credentialRef: "credential.scoped",
            field: "token",
          },
        ],
      },
    });
    value.externalAgents.push({
      id: "agent.codex",
      name: "Codex",
      adapter: "codex_app_server",
      enabled: false,
      connection: {
        transport: "http",
        url: "https://agent.example/rpc",
        headers: [],
      },
      credentialBindings: [],
      mcpServerIds: ["mcp.scoped"],
      capabilities: {
        progress: false,
        continuation: false,
        cancellation: false,
        approvals: false,
      },
      configuration: { ignored: true },
    });

    expect(validateSettingsConfiguration(value)).toEqual(
      expect.arrayContaining([
        expect.objectContaining({
          section: "providers",
          message: expect.stringContaining("api_key"),
        }),
        expect.objectContaining({
          section: "tools",
          message: expect.stringContaining("do not consume"),
        }),
        expect.objectContaining({
          section: "mcp",
          message: expect.stringContaining("Provider-scoped"),
        }),
        expect.objectContaining({
          section: "external_agents",
          message: expect.stringContaining("only its local STDIO"),
        }),
        expect.objectContaining({
          section: "external_agents",
          message: expect.stringContaining("does not consume MCP"),
        }),
        expect.objectContaining({
          section: "external_agents",
          message: expect.stringContaining("does not consume adapter"),
        }),
      ]),
    );
  });
});
