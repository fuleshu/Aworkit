import { describe, expect, it } from "vitest";
import { mcpServerConfigurationSchema } from "./configuration";

/**
 * Regression: a discovered plugin whose stdio transport carries non-secret
 * environment values must still parse. A strict schema without `environment`
 * rejected the whole settings snapshot, which emptied the Settings screen.
 */
describe("stdio transport public environment", () => {
  it("accepts non-secret environment values on a discovered stdio server", () => {
    const server = {
      id: "plugin.rea",
      name: "REA - Reverse Engineer Anything",
      enabled: false,
      autoConnect: false,
      transport: {
        transport: "stdio",
        command: "C:\\tools\\node24\\node.exe",
        args: ["C:\\tools\\node24\\node_modules\\rea-agents\\scripts\\rea.mjs", "--mcp"],
        env: [],
        environment: {
          JAVA_HOME: "C:\\Program Files\\Java\\jdk-17",
          REA_JADX_MCP_JAR: "C:\\Users\\user\\AppData\\Local\\rea\\android\\jadx-headless-mcp-0.7.1-all.jar",
          REA_MCP_OUTPUT_SCHEMA_PROFILE: "off",
        },
      },
    };

    const parsed = mcpServerConfigurationSchema.parse(server);
    expect(parsed.transport.transport).toBe("stdio");
    if (parsed.transport.transport === "stdio") {
      expect(parsed.transport.environment?.JAVA_HOME).toContain("jdk-17");
    }
  });

  it("still rejects unknown stdio keys", () => {
    const server = {
      id: "plugin.rea",
      name: "REA",
      enabled: false,
      autoConnect: false,
      transport: {
        transport: "stdio",
        command: "node",
        args: [],
        env: [],
        somethingElse: true,
      },
    };

    expect(() => mcpServerConfigurationSchema.parse(server)).toThrow();
  });
});
