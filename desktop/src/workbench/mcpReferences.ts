/** Authored references use names; runtime IDs belong only to frozen execution. */
import type { McpServerConfiguration } from "./configuration";

export function referencesServer(id: string, server: McpServerConfiguration): boolean {
  return id === `mcp:${server.name}` || id === `mcp:${server.id}` ||
    id.startsWith(`mcp://${encodeURIComponent(server.name)}/`) || id.startsWith(`mcp://${server.id}/`);
}

export function portableMcpReference(id: string, servers: readonly McpServerConfiguration[]): string {
  const server = servers.find(server => referencesServer(id, server));
  if (!server) return id;
  if (!id.startsWith("mcp://")) return `mcp:${server.name}`;
  return `mcp://${encodeURIComponent(server.name)}/${id.slice(id.indexOf("/", 6) + 1)}`;
}

export function portableMcpDocument<T>(document: T, servers: readonly McpServerConfiguration[]): T {
  const copy = structuredClone(document);
  if (!copy || typeof copy !== "object" || !("nodes" in copy) || !Array.isArray(copy.nodes)) return copy;
  for (const node of copy.nodes) {
    if (!node || typeof node !== "object" || !node.configuration) continue;
    if (node.type === "agent" && Array.isArray(node.configuration.toolIds)) {
      node.configuration.toolIds = node.configuration.toolIds.map((id: unknown) => typeof id === "string" ? portableMcpReference(id, servers) : id);
    } else if (node.type === "tool" && typeof node.configuration.toolId === "string") {
      node.configuration.toolId = portableMcpReference(node.configuration.toolId, servers);
    }
  }
  return copy;
}
