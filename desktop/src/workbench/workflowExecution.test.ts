import { describe, expect, it } from "vitest";
import { nativeTools } from "./toolRegistry";
import type { WorkflowDocument } from "./workflow";
import { assessNativeWorkflow } from "./workflowExecution";

/** Exercise both workflow binding paths using the catalog shipped with the app. */
function workflow(toolId: string, type: "agent" | "tool"): WorkflowDocument {
  return {
    schemaVersion: 1,
    id: "workflow.tools",
    name: "Tool validation",
    nodes: [
      { id: "input.1", type: "input" },
      {
        id: `${type}.1`,
        type,
        configuration: type === "agent"
          ? { modelTierId: "tier:balanced", toolIds: [toolId] }
          : { toolId, parameters: {} },
      },
      { id: "output.1", type: "output" },
      { id: "wait.1", type: "wait" },
    ],
    edges: [
      { id: "input-work", source: "input.1", target: `${type}.1` },
      { id: "work-output", source: `${type}.1`, target: "output.1" },
      { id: "output-wait", source: "output.1", target: "wait.1" },
    ],
  };
}

describe("bounded loop admission", () => {
  const looped = (): WorkflowDocument => ({
    schemaVersion: 1,
    id: "workflow.loop",
    name: "Bounded loop",
    nodes: [
      { id: "input.1", type: "input" },
      {
        id: "loop.1",
        type: "loop",
        configuration: {
          exitCondition: { kind: "exists", path: "done" },
          maximumIterations: 4,
        },
      },
      { id: "parallel.1", type: "parallel" },
      { id: "wait.1", type: "wait" },
    ],
    edges: [
      { id: "input-loop", source: "input.1", target: "loop.1" },
      {
        id: "loop-body",
        source: "loop.1",
        target: "parallel.1",
        configuration: { route: "body" },
      },
      {
        id: "loop-exit",
        source: "loop.1",
        target: "wait.1",
        configuration: { route: "exit" },
      },
      {
        id: "loop-fallback",
        source: "loop.1",
        target: "wait.1",
        configuration: { route: "fallback" },
      },
      {
        id: "loop-feedback",
        source: "parallel.1",
        target: "loop.1",
        configuration: { route: "feedback" },
      },
    ],
  });

  it("admits a declared bounded loop", () => {
    expect(assessNativeWorkflow(looped())).toEqual({ executable: true, issues: [] });
  });

  it("requires the three frozen routes, one feedback edge and a bounded contract", () => {
    const missingRoute: WorkflowDocument = {
      ...looped(),
      edges: looped().edges.filter((edge) => edge.id !== "loop-exit"),
    };
    expect(
      assessNativeWorkflow(missingRoute).issues.some(
        (issue) => issue.code === "native_loop_routes",
      ),
    ).toBe(true);

    const noFeedback: WorkflowDocument = {
      ...looped(),
      edges: looped().edges.filter((edge) => edge.id !== "loop-feedback"),
    };
    expect(
      assessNativeWorkflow(noFeedback).issues.some(
        (issue) => issue.code === "native_loop_routes",
      ),
    ).toBe(true);

    const invalidBound = looped();
    const nodes = invalidBound.nodes.map((node) =>
      node.id === "loop.1"
        ? {
            ...node,
            configuration: {
              exitCondition: { kind: "always" },
              maximumIterations: 0,
            },
          }
        : node,
    );
    expect(
      assessNativeWorkflow({ ...invalidBound, nodes }).issues.some(
        (issue) =>
          issue.code === "native_node_configuration" &&
          issue.message.includes("maximum iterations"),
      ),
    ).toBe(true);
  });

  it("admits a loop that declares no bound, naming the choice as a warning", () => {
    const unbounded = looped();
    const nodes = unbounded.nodes.map((node) =>
      node.id === "loop.1"
        ? { ...node, configuration: { exitCondition: { kind: "exists", path: "done" } } }
        : node,
    );
    const result = assessNativeWorkflow({ ...unbounded, nodes });
    // Repeating until the exit condition holds is the author's decision, so it
    // is reported rather than blocked.
    expect(result.executable).toBe(true);
    expect(
      result.issues.some((issue) => issue.code === "native_loop_unbounded"),
    ).toBe(true);
  });

  it("still refuses a loop without an exit condition", () => {
    const noCondition = looped();
    const nodes = noCondition.nodes.map((node) =>
      node.id === "loop.1"
        ? { ...node, configuration: { maximumIterations: 4 } }
        : node,
    );
    const result = assessNativeWorkflow({ ...noCondition, nodes });
    expect(result.executable).toBe(false);
    expect(
      result.issues.some(
        (issue) =>
          issue.code === "native_node_configuration" &&
          issue.message.includes("exitCondition"),
      ),
    ).toBe(true);
  });
});

describe.each(["agent", "tool"] as const)("%s tool validation", (type) => {
  it.each(nativeTools.filter(entry => type === "agent" || entry.activation !== "automatic_context").map(({ id }) => id))("accepts bundled executor %s", (id) => {
    expect(assessNativeWorkflow(workflow(id, type))).toEqual({ executable: true, issues: [] });
  });

  it("continues rejecting tools without a bundled executor", () => {
    const result = assessNativeWorkflow(workflow("tool.uninstalled", type));
    expect(result.executable).toBe(false);
    expect(result.issues.some(issue => issue.message.includes("no installed executor"))).toBe(true);
  });
});

it("explains that automatic context contributions belong on Agent nodes", () => {
  const result = assessNativeWorkflow(workflow("tool.workspace_instructions", "tool"));
  expect(result.executable).toBe(false);
  expect(result.issues.some(issue => issue.message.includes("automatic context plugins belong on Agent nodes"))).toBe(true);
});
