import { expect, it } from "vitest";
import { portableMcpDocument, referencesServer } from "./mcpReferences";
import type { McpServerConfiguration } from "./configuration";

it("exports MCP names without changing opaque source data or the editor draft", () => {
  const server = { id: "mcp.linux-guid", name: "Adashi / α" } as McpServerConfiguration;
  const document = { nodes: [
    { type: "agent", configuration: { toolIds: ["mcp:mcp.linux-guid", "mcp://mcp.linux-guid/read", "tool.todo", "mcp:unknown"] } },
    { type: "tool", configuration: { toolId: "mcp://mcp.linux-guid/write" } },
    { type: "extension", configuration: { toolId: "mcp:mcp.linux-guid" } },
  ], opaque: "mcp:mcp.linux-guid" };
  const portable = portableMcpDocument(document, [server]);
  expect(portable.nodes[0].configuration.toolIds).toEqual(["mcp:Adashi / α", "mcp://Adashi%20%2F%20%CE%B1/read", "tool.todo", "mcp:unknown"]);
  expect(portable.nodes[1].configuration.toolId).toBe("mcp://Adashi%20%2F%20%CE%B1/write");
  expect(portable.nodes[2]).toEqual(document.nodes[2]);
  expect(portable.opaque).toBe(document.opaque);
  expect(document.nodes[0].configuration.toolIds?.[0]).toBe("mcp:mcp.linux-guid");
  const windows = { ...server, id: "mcp.windows-guid" };
  expect(referencesServer(portable.nodes[1].configuration.toolId!, windows)).toBe(true);
});
