import { describe, expect, it } from "vitest";
import {
  CATALOG_NODE_TYPES,
  catalogEntryForType,
  isCatalogNodeType,
  portKindsConnect,
  resolveInputPortKind,
  resolveOutputPortKind,
} from "./nodeCatalog";
import { nodesInCycles, validateWorkflow, type WorkflowDocument } from "./workflow";

describe("typed V1 node catalog", () => {
  it("describes exactly the ten executable node types in palette order", () => {
    expect(CATALOG_NODE_TYPES).toEqual([
      "input",
      "model_call",
      "agent",
      "tool",
      "condition",
      "parallel",
      "approval",
      "output",
      "wait",
      "completion",
    ]);
    for (const type of CATALOG_NODE_TYPES) {
      expect(isCatalogNodeType(type)).toBe(true);
      expect(catalogEntryForType(type)).toBeDefined();
    }
    expect(isCatalogNodeType("future_node")).toBe(false);
    expect(catalogEntryForType("future_node")).toBeUndefined();
  });

  it("types ports so condition branches route and text nodes carry text", () => {
    expect(resolveOutputPortKind("condition", "true")).toBe("route");
    expect(resolveOutputPortKind("condition", "false")).toBe("route");
    expect(resolveOutputPortKind("condition", "out")).toBe("unknown-port");
    expect(resolveOutputPortKind("agent", "out")).toBe("text");
    expect(resolveInputPortKind("agent", "in")).toBe("text");
    expect(resolveInputPortKind("completion", "in")).toBe("flow");
    expect(resolveOutputPortKind("future_node", "out")).toBeUndefined();
  });

  it("allows text-to-text and any-to-flow, rejects text to a route input", () => {
    expect(portKindsConnect("text", "text")).toBe(true);
    expect(portKindsConnect("flow", "flow")).toBe(true);
    expect(portKindsConnect("text", "flow")).toBe(true);
    expect(portKindsConnect("flow", "text")).toBe(false);
    expect(portKindsConnect("route", "flow")).toBe(true);
    expect(portKindsConnect("route", "text")).toBe(false);
  });

  it("does not expose or create an aggregate Agent run timeout", () => {
    const agent = catalogEntryForType("agent");
    expect(agent?.fields.some((field) => field.key === "timeoutSeconds")).toBe(false);
    expect(agent?.defaultConfiguration).not.toHaveProperty("timeoutSeconds");
  });
});

describe("workflow connection and cycle validation", () => {
  it("accepts the exact Simple Chat graph with default ports", () => {
    expect(validateWorkflow(simpleChat()).filter((issue) => issue.code !== "missing_dependency")).toEqual([]);
  });

  it("flags a condition transition that does not route true or false", () => {
    const document: WorkflowDocument = {
      schemaVersion: 1,
      nodes: [
        { id: "c.1", type: "condition" },
        { id: "o.1", type: "output" },
      ],
      edges: [{ id: "e.1", source: "c.1", target: "o.1" }],
    };
    const codes = validateWorkflow(document).map((issue) => issue.code);
    expect(codes).toContain("condition_route_missing");
  });

  it("flags an unknown output port and incompatible connection kinds", () => {
    const document: WorkflowDocument = {
      schemaVersion: 1,
      nodes: [
        { id: "i.1", type: "input" },
        { id: "a.1", type: "agent", configuration: { modelTierId: "tier:balanced", toolIds: [] } },
      ],
      edges: [{ id: "e.1", source: "i.1", target: "a.1", sourcePort: "missing" }],
    };
    const codes = validateWorkflow(document).map((issue) => issue.code);
    expect(codes).toContain("unknown_port");
  });

  it("detects nodes participating in a directed cycle", () => {
    const document: WorkflowDocument = {
      schemaVersion: 1,
      nodes: [
        { id: "a.1", type: "parallel" },
        { id: "b.1", type: "approval" },
        { id: "c.1", type: "completion" },
      ],
      edges: [
        { id: "e.1", source: "a.1", target: "b.1" },
        { id: "e.2", source: "b.1", target: "c.1" },
        { id: "e.3", source: "c.1", target: "a.1" },
      ],
    };
    expect(nodesInCycles(document)).toEqual(["a.1", "b.1", "c.1"]);
    expect(validateWorkflow(document).map((issue) => issue.code)).toContain(
      "cycle_detected",
    );
  });

  it("keeps unknown node types lossless and unvalidated for ports", () => {
    const document: WorkflowDocument = {
      schemaVersion: 1,
      nodes: [
        { id: "x.1", type: "future@2" },
        { id: "y.1", type: "future@2" },
      ],
      edges: [{ id: "e.1", source: "x.1", target: "y.1", sourcePort: "out", targetPort: "in" }],
    };
    expect(validateWorkflow(document)).toEqual([]);
  });
});

function simpleChat(): WorkflowDocument {
  return {
    schemaVersion: 1,
    id: "workflow.simple-chat",
    name: "Simple Chat",
    nodes: [
      { id: "input.1", label: "Input", type: "input" },
      {
        id: "agent.1",
        label: "Agent",
        type: "agent",
        configuration: { modelTierId: "tier:balanced", toolIds: [] },
      },
      { id: "output.1", label: "Output", type: "output" },
      { id: "wait.1", label: "Wait for input", type: "wait" },
    ],
    edges: [
      { id: "input-agent", source: "input.1", target: "agent.1" },
      { id: "agent-output", source: "agent.1", target: "output.1" },
      { id: "output-wait", source: "output.1", target: "wait.1" },
    ],
  };
}
