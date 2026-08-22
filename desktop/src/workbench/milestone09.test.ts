import { describe, expect, it } from "vitest";
import {
  resolveCapabilities,
  type CapabilityRecord,
  type CapabilityRequirement,
} from "./settings";

describe("Milestone 09 integration requirement resolution", () => {
  it("keeps missing, disabled, incompatible, drifted, and ready integrations distinct", () => {
    const requirements: readonly CapabilityRequirement[] = [
      {
        id: "extension.review",
        label: "Review extension",
        requiredVersion: "1.0.0",
      },
      { id: "mcp.github", label: "GitHub MCP" },
      { id: "agent.codex", label: "Codex agent" },
      { id: "isolation.container", label: "Container isolation" },
      {
        id: "extension.drifted",
        label: "Drifted extension",
        requiredVersion: "2.0.0",
      },
    ];
    const records: readonly CapabilityRecord[] = [
      {
        id: "extension.review",
        label: "Review extension",
        kind: "extension",
        state: "ready",
        version: "1.0.0",
      },
      {
        id: "mcp.github",
        label: "GitHub MCP",
        kind: "mcp",
        state: "disabled",
      },
      {
        id: "agent.codex",
        label: "Codex agent",
        kind: "external_agent",
        state: "incompatible",
        detail: "attested protocol version is outside the supported range",
      },
      {
        id: "extension.drifted",
        label: "Drifted extension",
        kind: "extension",
        state: "ready",
        version: "1.9.0",
      },
    ];

    const resolution = resolveCapabilities(
      requirements,
      new Set([
        "extension.review",
        "mcp.github",
        "agent.codex",
        "extension.drifted",
      ]),
      records,
    );

    expect(resolution.available.map(({ id }) => id)).toEqual([
      "extension.review",
    ]);
    expect(resolution.incompatible.map(({ id }) => id)).toEqual([
      "agent.codex",
    ]);
    expect(resolution.disabled.map(({ id }) => id)).toEqual(["mcp.github"]);
    expect(resolution.drifted.map(({ id }) => id)).toEqual([
      "extension.drifted",
    ]);
    expect(resolution.missing.map(({ id }) => id)).toEqual([
      "isolation.container",
    ]);
  });
});
