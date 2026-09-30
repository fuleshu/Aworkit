import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { bundledWorkflowTemplates } from "./bundledWorkflows";
import { CATALOG_NODE_TYPES } from "./nodeCatalog";
import { assessNativeWorkflow } from "./workflowExecution";
import { validateWorkflow } from "./workflow";

/**
 * The editor catalog and the native executable catalog both understand these
 * twelve node types. Shipping a bundled workflow for every one of them keeps
 * the palette honest: a node a user can drag is a node the library actually
 * demonstrates, and every bundled document is proven executable here.
 *
 * Coverage is measured over the whole bundle, not only the templates seeded on
 * a fresh profile: a template with a real Settings prerequisite (an external
 * agent target) ships importable instead of auto-seeded, but stays valid.
 */
describe("bundled workflow node coverage", () => {
  const bundled = bundledWorkflowTemplates;

  /** Workflows added specifically to demonstrate the previously-unused nodes. */
  const showcaseWorkflowIds = [
    "workflow.triage-router",
    "workflow.evidence-brief",
    "workflow.iterative-planning",
    "workflow.delegated-code-review",
  ] as const;

  it("uses every catalog node type across the bundled library", () => {
    const used = new Set<string>();
    for (const { document } of bundled)
      for (const node of document.nodes)
        if (typeof node.type === "string") used.add(node.type);
    const missing = CATALOG_NODE_TYPES.filter((type) => !used.has(type));
    expect(missing).toEqual([]);
  });

  it("keeps every bundled workflow executable under both validators", () => {
    for (const { templateId, document } of bundled) {
      // The blank creation canvas is intentionally an empty, editable document.
      if (document.nodes.length === 0) continue;
      expect(
        validateWorkflow(document),
        `${templateId} must satisfy the editor contract`,
      ).toEqual([]);
      const compatibility = assessNativeWorkflow(document);
      expect(
        compatibility.issues,
        `${templateId} must satisfy the native executable catalog`,
      ).toEqual([]);
      expect(compatibility.executable, `${templateId} must be executable`).toBe(
        true,
      );
    }
  });

  it("ships an importable copy of every showcased workflow", () => {
    for (const workflowId of showcaseWorkflowIds) {
      const template = bundled.find(({ workflowId: id }) => id === workflowId);
      expect(template, `${workflowId} must be a bundled template`).toBeDefined();
      const examplePath = new URL(
        `../../workflows/examples/${workflowId.replace(/^workflow\./u, "")}.aworkit.json`,
        import.meta.url,
      );
      const example = JSON.parse(readFileSync(examplePath, "utf8"));
      expect(example, `${workflowId} example drifted from the bundle`).toEqual(
        template!.document,
      );
    }
  });
});
