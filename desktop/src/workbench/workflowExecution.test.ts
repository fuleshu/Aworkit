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

describe.each(["agent", "tool"] as const)("%s tool validation", (type) => {
  it.each(nativeTools.map(({ id }) => id))("accepts bundled executor %s", (id) => {
    expect(assessNativeWorkflow(workflow(id, type))).toEqual({ executable: true, issues: [] });
  });

  it("continues rejecting tools without a bundled executor", () => {
    const result = assessNativeWorkflow(workflow("tool.uninstalled", type));
    expect(result.executable).toBe(false);
    expect(result.issues.some(issue => issue.message.includes("no installed executor"))).toBe(true);
  });
});
