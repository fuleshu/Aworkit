/*
 * The editor kernel's 16 ms interaction gate for a representative
 * 1,000-node workflow. A wall-clock budget, so it is opt-in (`pnpm
 * test:perf`) instead of a default-suite test that fails under load.
 */
import { performance } from "node:perf_hooks";
import { describe, expect, it } from "vitest";
import { projectWorkflowSurface } from "../workbench/graphSurface";
import {
  createEditor,
  moveWorkflowNode,
} from "../workbench/workflow";

describe("workflow kernel frame budget", () => {
  it("keeps representative 1,000-node kernel interactions inside the frame-budget gate", () => {
    const document = {
      schemaVersion: 1,
      nodes: Array.from({ length: 1_000 }, (_, index) => ({
        id: `node.${index}`,
        type: "model",
        position: { x: index % 50, y: Math.floor(index / 50) },
      })),
      edges: [],
    };
    const initial = createEditor(document);
    const start = performance.now();
    const editor = moveWorkflowNode(initial, "node.999", {
      x: 100,
      y: 200,
    });
    const surface = projectWorkflowSurface(editor);
    const elapsed = performance.now() - start;
    expect(editor.document.nodes).toHaveLength(1_000);
    expect(surface.nodes).toHaveLength(1_000);
    expect(elapsed).toBeLessThan(16);
  });
});
